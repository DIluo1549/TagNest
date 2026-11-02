mod api;
mod auth;
mod config;
mod lyrics;
mod models;
mod naming;
mod providers;
mod scanner;
mod store;
mod tagger;
mod util;

use crate::api::AppState;
use crate::auth::Auth;
use crate::config::{Config, Options, Paths};
use crate::providers::Providers;
use crate::scanner::ScanManager;
use crate::store::Store;
use anyhow::{Context, Result};
use std::net::TcpStream;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};
use tracing_subscriber::EnvFilter;

fn main() -> Result<()> {
    let options = Options::parse()?;
    if options.healthcheck {
        return healthcheck(options.port);
    }
    init_logging();
    let workers = std::env::var("TAGNEST_THREADS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map(|v| v.get())
                .unwrap_or(2)
                .clamp(1, 4)
                .min(2)
        });
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(workers)
        .enable_all()
        .build()
        .context("创建运行时失败")?;
    runtime.block_on(run(options))
}

async fn run(options: Options) -> Result<()> {
    let paths = Paths::new(options.data.clone())?;
    let music = if options.music.is_empty() {
        None
    } else {
        Some(options.music.clone())
    };
    let config = Config::load(&paths, music)?;
    if let Err(err) = config.validate() {
        tracing::warn!("配置需要处理: {err:#}");
    }
    let store = Arc::new(Store::open(&paths.db)?);
    let auth = Arc::new(Auth::new(options.password.clone(), &paths.secret)?);
    let providers = Arc::new(Providers::new()?);
    let scan = Arc::new(ScanManager::default());
    let auth_enabled = auth.enabled();
    let state = AppState {
        config: Arc::new(RwLock::new(config.clone())),
        store,
        scan,
        providers,
        auth,
        paths: paths.clone(),
        started_at: Arc::new(Instant::now()),
    };
    let app = api::router(state);
    let bind = format!("{}:{}", options.bind, options.port);
    let listener = tokio::net::TcpListener::bind(&bind)
        .await
        .with_context(|| format!("无法监听 {bind}"))?;
    tracing::info!(
        "TagNest {} 已启动: http://{bind}",
        env!("CARGO_PKG_VERSION")
    );
    tracing::info!("数据目录: {}", paths.data.display());
    for root in &config.library_roots {
        tracing::info!("音乐目录: {root}");
    }
    if auth_enabled {
        tracing::info!("已启用访问密码");
    } else {
        tracing::warn!("未设置 TAGNEST_PASSWORD，WebUI 在局域网内无密码访问");
    }
    println!(
        "\n  TagNest 已就绪  →  http://127.0.0.1:{}\n  音乐目录: {}\n  数据目录: {}\n",
        options.port,
        if config.library_roots.is_empty() {
            "未配置".to_string()
        } else {
            config.library_roots.join(", ")
        },
        paths.data.display()
    );
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("HTTP 服务异常退出")?;
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        let mut signal = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("install SIGTERM handler");
        signal.recv().await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
    tracing::info!("正在关闭 TagNest…");
}

fn healthcheck(port: u16) -> Result<()> {
    let address = format!("127.0.0.1:{port}");
    TcpStream::connect_timeout(
        &address.parse().context("健康检查地址无效")?,
        Duration::from_secs(3),
    )
    .with_context(|| format!("无法连接 {address}"))?;
    Ok(())
}

fn init_logging() {
    let filter = std::env::var("TAGNEST_LOG")
        .or_else(|_| std::env::var("RUST_LOG"))
        .unwrap_or_else(|_| "tagnest=info".to_string());
    let _ = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(filter))
        .with_target(false)
        .compact()
        .try_init();
}
