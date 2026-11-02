use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub library_roots: Vec<String>,
    /// Legacy/global default; per-field values fall back to this when empty.
    pub tag_separator: String,
    pub separators: FieldSeparators,
    /// Separator used for display artists in scraped metadata. Source results
    /// use different separators (iTunes "&", NetEase "/"), so this normalizes
    /// them into one representation.
    pub scrape_separator: String,
    /// Separator used for display artists when building filenames.
    pub naming_artist_separator: String,
    pub split_separators: Vec<String>,
    /// Internal migration marker for newly added default split separators.
    pub defaults_version: u32,
    pub naming_template: String,
    pub naming_presets: Vec<NamingPreset>,
    pub lyrics: LyricsConfig,
    pub providers: ProvidersConfig,
    /// Base URLs for external APIs. Keeping these editable lets users point
    /// TagNest at a mirror, reverse proxy, or self-hosted NetEase/MusicBrainz
    /// gateway without rebuilding.
    pub endpoints: ProviderEndpoints,
    /// User-managed source interfaces. Order is the scrape priority.
    pub sources: Vec<MusicSource>,
    /// False means this config predates selectable interfaces and should be
    /// migrated from providers/endpoints once. An intentionally empty list is
    /// preserved when this is true.
    pub sources_initialized: bool,
    pub theme: String,
    pub auto_match_threshold: f64,
    pub itunes_country: String,
    pub scan_ignore: Vec<String>,
    pub max_cover_mb: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct FieldSeparators {
    pub artists: String,
    pub albumartist: String,
    pub genres: String,
    pub composers: String,
    pub lyricists: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct NamingPreset {
    pub name: String,
    pub template: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct LyricsConfig {
    pub bilingual: bool,
    pub embed: bool,
    pub prefer_synced: bool,
    pub overwrite: bool,
    /// Create a sidecar `.lrc` by default. New installs default to embedding
    /// only; users can opt in per operation or in settings.
    pub write_file: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct ProvidersConfig {
    pub musicbrainz: bool,
    pub itunes: bool,
    pub deezer: bool,
    pub lrclib: bool,
    pub netease: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct ProviderEndpoints {
    pub musicbrainz: String,
    pub coverart: String,
    pub itunes: String,
    pub deezer: String,
    pub lrclib: String,
    pub netease: String,
}

impl Default for ProviderEndpoints {
    fn default() -> Self {
        Self {
            musicbrainz: "https://musicbrainz.org".into(),
            coverart: "https://coverartarchive.org".into(),
            itunes: "https://itunes.apple.com".into(),
            deezer: "https://api.deezer.com".into(),
            lrclib: "https://lrclib.net".into(),
            netease: "https://music.163.com".into(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct MusicSource {
    pub id: String,
    pub name: String,
    /// Parser/adapter: musicbrainz, itunes, deezer, netease, lrclib.
    pub adapter: String,
    /// Base URL of this interface (path prefix and query are not allowed).
    pub base_url: String,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

impl Default for MusicSource {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            adapter: "netease".into(),
            base_url: "https://music.163.com".into(),
            enabled: true,
        }
    }
}

fn default_enabled() -> bool {
    true
}

pub fn sources_from_legacy(endpoints: &ProviderEndpoints, providers: &ProvidersConfig) -> Vec<MusicSource> {
    vec![
        MusicSource {
            id: "musicbrainz".into(),
            name: "MusicBrainz".into(),
            adapter: "musicbrainz".into(),
            base_url: endpoints.musicbrainz.clone(),
            enabled: providers.musicbrainz,
        },
        MusicSource {
            id: "itunes".into(),
            name: "iTunes / Apple Music".into(),
            adapter: "itunes".into(),
            base_url: endpoints.itunes.clone(),
            enabled: providers.itunes,
        },
        MusicSource {
            id: "deezer".into(),
            name: "Deezer".into(),
            adapter: "deezer".into(),
            base_url: endpoints.deezer.clone(),
            enabled: providers.deezer,
        },
        MusicSource {
            id: "netease".into(),
            name: "网易云音乐".into(),
            adapter: "netease".into(),
            base_url: endpoints.netease.clone(),
            enabled: providers.netease,
        },
        MusicSource {
            id: "lrclib".into(),
            name: "LRCLIB".into(),
            adapter: "lrclib".into(),
            base_url: endpoints.lrclib.clone(),
            enabled: providers.lrclib,
        },
    ]
}

impl Default for Config {
    fn default() -> Self {
        Self {
            library_roots: Vec::new(),
            tag_separator: "; ".to_string(),
            separators: FieldSeparators::default(),
            scrape_separator: "/".to_string(),
            naming_artist_separator: ", ".to_string(),
            split_separators: vec![
                ";".to_string(),
                "；".to_string(),
                "、".to_string(),
                "|".to_string(),
                "&".to_string(),
                ",".to_string(),
                "/".to_string(),
            ],
            defaults_version: 4,
            naming_template: "{artist}/{album}/{track:02d} - {title}".to_string(),
            naming_presets: vec![
                NamingPreset {
                    name: "歌手 / 专辑 / 序号 - 标题".into(),
                    template: "{artist}/{album}/{track:02d} - {title}".into(),
                },
                NamingPreset {
                    name: "歌手 - 标题".into(),
                    template: "{artist} - {title}".into(),
                },
                NamingPreset {
                    name: "专辑 / 序号. 标题".into(),
                    template: "{album}/{track:02d}. {title}".into(),
                },
                NamingPreset {
                    name: "多碟：唱片-序号 标题".into(),
                    template: "{artist}/{album}/{disc}-{track:02d} {title}".into(),
                },
                NamingPreset {
                    name: "仅标题（按歌手/专辑归档）".into(),
                    template: "{artist}/{album}/{title}".into(),
                },
            ],
            lyrics: LyricsConfig::default(),
            providers: ProvidersConfig::default(),
            endpoints: ProviderEndpoints::default(),
            sources: Vec::new(),
            sources_initialized: false,
            theme: "emerald".into(),
            auto_match_threshold: 0.62,
            itunes_country: "cn".to_string(),
            scan_ignore: vec![
                "@eaDir".into(),
                "#recycle".into(),
                ".DS_Store".into(),
                "Thumbs.db".into(),
                ".git".into(),
                "node_modules".into(),
            ],
            max_cover_mb: 10,
        }
    }
}

impl Default for LyricsConfig {
    fn default() -> Self {
        Self {
            bilingual: true,
            embed: true,
            prefer_synced: true,
            overwrite: true,
            write_file: false,
        }
    }
}

impl Default for ProvidersConfig {
    fn default() -> Self {
        Self {
            musicbrainz: true,
            itunes: true,
            deezer: true,
            lrclib: true,
            netease: true,
        }
    }
}

impl Config {
    pub fn load(paths: &Paths, cli_music: Option<Vec<String>>) -> Result<Self> {
        let mut config = if paths.config.exists() {
            let raw = fs::read_to_string(&paths.config)
                .with_context(|| format!("读取配置失败: {}", paths.config.display()))?;
            serde_json::from_str::<Config>(&raw)
                .with_context(|| format!("配置文件格式错误: {}", paths.config.display()))?
        } else {
            Config::default()
        };

        if config.library_roots.is_empty() {
            if let Some(music) = cli_music {
                config.library_roots = music;
            } else if let Ok(value) = std::env::var("TAGNEST_MUSIC_DIRS") {
                config.library_roots = value
                    .split(',')
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
                    .map(ToOwned::to_owned)
                    .collect();
            } else if let Ok(value) = std::env::var("MUSIC_DIR") {
                if !value.trim().is_empty() {
                    config.library_roots = vec![value];
                }
            } else if Path::new("/music").exists() {
                config.library_roots = vec!["/music".to_string()];
            }
        }
        config.normalize();
        config.save(&paths.config)?;
        Ok(config)
    }

    pub fn normalize(&mut self) {
        self.library_roots = self
            .library_roots
            .iter()
            .map(|p| p.trim().trim_end_matches(['/', '\\']).to_string())
            .filter(|p| !p.is_empty())
            .collect();
        self.library_roots.sort();
        self.library_roots.dedup();

        if self.tag_separator.is_empty() {
            self.tag_separator = "; ".to_string();
        }
        if self.scrape_separator.trim().is_empty() {
            self.scrape_separator = "/".to_string();
        }
        if self.defaults_version < 4 {
            for extra in ["&", ",", "/"] {
                if !self.split_separators.iter().any(|s| s == extra) {
                    self.split_separators.push(extra.to_string());
                }
            }
            self.defaults_version = 4;
        }
        if self.split_separators.is_empty() {
            self.split_separators = vec![";".into(), "；".into(), "、".into()];
        }
        self.split_separators = self
            .split_separators
            .iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        self.split_separators.sort();
        self.split_separators.dedup();

        if self.naming_template.trim().is_empty() {
            self.naming_template = "{artist}/{album}/{track:02d} - {title}".into();
        }
        if self.itunes_country.len() != 2 {
            self.itunes_country = "cn".into();
        } else {
            self.itunes_country = self.itunes_country.to_lowercase();
        }
        if self.naming_artist_separator.trim().is_empty() {
            self.naming_artist_separator.clear();
        }
        self.theme = self.theme.trim().to_lowercase();
        if !matches!(self.theme.as_str(), "emerald" | "ocean" | "violet" | "light") {
            self.theme = "emerald".into();
        }
        self.auto_match_threshold = self.auto_match_threshold.clamp(0.0, 1.0);
        self.max_cover_mb = self.max_cover_mb.clamp(1, 50);

        let defaults = ProviderEndpoints::default();
        self.endpoints.musicbrainz =
            sanitize_base_url(&self.endpoints.musicbrainz, &defaults.musicbrainz);
        self.endpoints.coverart = sanitize_base_url(&self.endpoints.coverart, &defaults.coverart);
        self.endpoints.itunes = sanitize_base_url(&self.endpoints.itunes, &defaults.itunes);
        self.endpoints.deezer = sanitize_base_url(&self.endpoints.deezer, &defaults.deezer);
        self.endpoints.lrclib = sanitize_base_url(&self.endpoints.lrclib, &defaults.lrclib);
        self.endpoints.netease = sanitize_base_url(&self.endpoints.netease, &defaults.netease);

        if !self.sources_initialized {
            self.sources = sources_from_legacy(&self.endpoints, &self.providers);
            self.sources_initialized = true;
        }
        self.normalize_sources();
    }

    fn normalize_sources(&mut self) {
        let mut used = std::collections::HashSet::new();
        for (index, source) in self.sources.iter_mut().enumerate() {
            source.adapter = source.adapter.trim().to_lowercase();
            if !matches!(
                source.adapter.as_str(),
                "musicbrainz" | "itunes" | "deezer" | "netease" | "lrclib"
            ) {
                source.adapter = "netease".into();
            }
            source.base_url = sanitize_base_url(
                &source.base_url,
                &default_base_for_adapter(&source.adapter),
            );
            source.name = source.name.trim().to_string();
            if source.name.is_empty() {
                source.name = adapter_label(&source.adapter).to_string();
            }
            let mut id = slugify_id(&source.id);
            if id.is_empty() {
                id = format!("{}-{}", source.adapter, index + 1);
            }
            let mut candidate = id.clone();
            let mut suffix = 2;
            while !used.insert(candidate.clone()) {
                candidate = format!("{id}-{suffix}");
                suffix += 1;
            }
            source.id = candidate;
        }
    }

    /// Effective separator for one multi-value tag field. Empty per-field
    /// values fall back to the legacy global separator.
    pub fn separator_for(&self, field: &str) -> &str {
        let value = match field {
            "artists" => &self.separators.artists,
            "albumartist" => &self.separators.albumartist,
            "genres" => &self.separators.genres,
            "composers" => &self.separators.composers,
            "lyricists" => &self.separators.lyricists,
            _ => &self.tag_separator,
        };
        if value.trim().is_empty() {
            &self.tag_separator
        } else {
            value
        }
    }

    pub fn source_by_id(&self, id: &str) -> Option<&MusicSource> {
        self.sources.iter().find(|source| source.id == id)
    }

    pub fn enabled_sources(&self) -> impl Iterator<Item = &MusicSource> {
        self.sources.iter().filter(|source| source.enabled)
    }

    pub fn naming_separator(&self) -> &str {
        if self.naming_artist_separator.trim().is_empty() {
            // A slash is valid inside tags but not inside filenames. Keep the
            // filename default safe and independent from scrape_separator.
            ", "
        } else {
            &self.naming_artist_separator
        }
    }

    pub fn validate(&self) -> Result<()> {
        for root in &self.library_roots {
            let path = Path::new(root);
            if !path.is_absolute() {
                bail!("音乐目录必须是绝对路径: {root}");
            }
            if !path.is_dir() {
                bail!("音乐目录不存在或不是文件夹: {root}");
            }
        }
        if self.tag_separator.is_empty() {
            bail!("写入分隔符不能为空");
        }
        if self.split_separators.is_empty() {
            bail!("至少需要一个识别分隔符");
        }
        if self.naming_template.trim().is_empty() {
            bail!("命名模板不能为空");
        }
        Ok(())
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("json.tmp");
        let raw = serde_json::to_vec_pretty(self)?;
        fs::write(&tmp, raw).with_context(|| format!("写入配置失败: {}", tmp.display()))?;
        fs::rename(&tmp, path).with_context(|| format!("保存配置失败: {}", path.display()))?;
        Ok(())
    }
}

pub fn adapter_label(adapter: &str) -> &'static str {
    match adapter {
        "musicbrainz" => "MusicBrainz",
        "itunes" => "iTunes / Apple Music",
        "deezer" => "Deezer",
        "lrclib" => "LRCLIB",
        _ => "网易云音乐",
    }
}

pub fn adapter_metadata(adapter: &str) -> bool {
    matches!(adapter, "musicbrainz" | "itunes" | "deezer" | "netease")
}

pub fn adapter_lyrics(adapter: &str) -> bool {
    matches!(adapter, "lrclib" | "netease")
}

fn default_base_for_adapter(adapter: &str) -> String {
    match adapter {
        "musicbrainz" => "https://musicbrainz.org",
        "itunes" => "https://itunes.apple.com",
        "deezer" => "https://api.deezer.com",
        "lrclib" => "https://lrclib.net",
        _ => "https://music.163.com",
    }
    .to_string()
}

fn slugify_id(value: &str) -> String {
    let mut id = String::new();
    for ch in value.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            id.push(ch.to_ascii_lowercase());
        } else if ch == '-' || ch == '_' || ch.is_whitespace() {
            if !matches!(id.chars().last(), Some('-') | Some('_')) {
                id.push('-');
            }
        }
    }
    id.trim_matches(|ch| ch == '-' || ch == '_').to_string()
}

fn sanitize_base_url(value: &str, fallback: &str) -> String {
    let value = value.trim().trim_end_matches('/');
    if value.starts_with("http://") || value.starts_with("https://") {
        value.to_string()
    } else {
        fallback.to_string()
    }
}

#[derive(Clone, Debug)]
pub struct Paths {
    pub data: PathBuf,
    pub db: PathBuf,
    pub covers: PathBuf,
    pub config: PathBuf,
    pub secret: PathBuf,
}

impl Paths {
    pub fn new(data: PathBuf) -> Result<Self> {
        fs::create_dir_all(&data)
            .with_context(|| format!("无法创建数据目录: {}", data.display()))?;
        let covers = data.join("covers");
        fs::create_dir_all(&covers)?;
        Ok(Self {
            db: data.join("library.redb"),
            config: data.join("config.json"),
            secret: data.join("secret.key"),
            covers,
            data,
        })
    }
}

#[derive(Clone, Debug)]
pub struct Options {
    pub bind: String,
    pub port: u16,
    pub data: PathBuf,
    pub music: Vec<String>,
    pub password: Option<String>,
    pub healthcheck: bool,
}

impl Options {
    pub fn parse() -> Result<Self> {
        let mut bind = std::env::var("TAGNEST_BIND").unwrap_or_else(|_| "0.0.0.0".into());
        let mut port = std::env::var("TAGNEST_PORT")
            .ok()
            .and_then(|v| v.parse::<u16>().ok())
            .unwrap_or(8080);
        let mut data = std::env::var("TAGNEST_DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("data"));
        let mut music: Vec<String> = Vec::new();
        let mut healthcheck = false;
        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--bind" => bind = args.next().context("--bind 缺少参数")?,
                "--port" => {
                    port = args
                        .next()
                        .context("--port 缺少参数")?
                        .parse()
                        .context("--port 必须是数字")?
                }
                "--data" => data = PathBuf::from(args.next().context("--data 缺少参数")?),
                "--music" => music.push(args.next().context("--music 缺少参数")?),
                "--healthcheck" => healthcheck = true,
                "--help" | "-h" => {
                    println!(
                        "TagNest {}\n\n用法:\n  tagnest [--data DIR] [--music DIR] [--bind ADDR] [--port PORT]\n\n环境变量:\n  TAGNEST_DATA_DIR   数据目录（默认 /data）\n  TAGNEST_MUSIC_DIRS 逗号分隔的音乐目录\n  TAGNEST_PORT       HTTP 端口（默认 8080）\n  TAGNEST_PASSWORD   可选登录密码\n",
                        env!("CARGO_PKG_VERSION")
                    );
                    std::process::exit(0);
                }
                other => bail!("未知参数: {other}（使用 --help 查看用法）"),
            }
        }
        let password = std::env::var("TAGNEST_PASSWORD")
            .ok()
            .filter(|v| !v.is_empty());
        Ok(Self {
            bind,
            port,
            data,
            music,
            password,
            healthcheck,
        })
    }
}
