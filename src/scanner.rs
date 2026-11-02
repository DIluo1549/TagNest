use crate::config::Config;
use crate::models::Track;
use crate::store::Store;
use crate::tagger::read_metadata;
use crate::util::{extension, is_audio_file, native_path_string, now_secs};
use anyhow::{bail, Context, Result};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use walkdir::{DirEntry, WalkDir};

#[derive(Clone, Debug, Default, Serialize)]
pub struct ScanProgress {
    pub running: bool,
    pub phase: String,
    pub total: usize,
    pub done: usize,
    pub added: usize,
    pub updated: usize,
    pub removed: usize,
    pub errors: usize,
    pub current_file: String,
    pub started_at: i64,
    pub finished_at: i64,
    pub elapsed_secs: u64,
    pub cancel_requested: bool,
    pub error: String,
    pub roots: Vec<String>,
}

pub struct ScanManager {
    progress: Mutex<ScanProgress>,
    cancel: AtomicBool,
}

impl Default for ScanManager {
    fn default() -> Self {
        Self {
            progress: Mutex::new(ScanProgress {
                phase: "idle".into(),
                ..Default::default()
            }),
            cancel: AtomicBool::new(false),
        }
    }
}

impl ScanManager {
    pub fn snapshot(&self) -> ScanProgress {
        self.progress.lock().map(|s| s.clone()).unwrap_or_default()
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
        if let Ok(mut progress) = self.progress.lock() {
            progress.cancel_requested = true;
        }
    }

    pub fn start(self: &Arc<Self>, store: Arc<Store>, config: Config) -> Result<()> {
        if config.library_roots.is_empty() {
            bail!("还没有配置音乐目录，请先在设置中添加 NAS 挂载路径");
        }
        {
            let mut progress = self.progress.lock().unwrap();
            if progress.running {
                bail!("已有扫描任务在运行");
            }
            *progress = ScanProgress {
                running: true,
                phase: "counting".into(),
                started_at: now_secs(),
                roots: config.library_roots.clone(),
                ..Default::default()
            };
            self.cancel.store(false, Ordering::SeqCst);
        }
        let manager = Arc::clone(self);
        std::thread::Builder::new()
            .name("tagnest-scan".into())
            .spawn(move || {
                let started = Instant::now();
                let result = scan_library(&manager, &store, &config);
                if let Ok(mut progress) = manager.progress.lock() {
                    progress.running = false;
                    progress.finished_at = now_secs();
                    progress.elapsed_secs = started.elapsed().as_secs();
                    progress.current_file.clear();
                    match result {
                        Ok(cancelled) => {
                            if cancelled {
                                progress.phase = "cancelled".into();
                            } else {
                                progress.phase = "done".into();
                            }
                        }
                        Err(err) => {
                            progress.phase = "error".into();
                            progress.error = format!("{err:#}");
                        }
                    }
                }
            })
            .context("无法启动扫描线程")?;
        Ok(())
    }
}

fn scan_library(manager: &ScanManager, store: &Store, config: &Config) -> Result<bool> {
    let ignore: HashSet<String> = config.scan_ignore.iter().cloned().collect();
    let existing: HashMap<String, Track> = store
        .all()?
        .into_iter()
        .map(|t| (t.path.clone(), t))
        .collect();

    let mut total = 0usize;
    let mut scanned_roots = Vec::new();
    for root in &config.library_roots {
        if !Path::new(root).is_dir() {
            tracing::warn!("扫描根目录不存在，保留已有索引: {root}");
            continue;
        }
        scanned_roots.push(root.clone());
        for entry in walker(root, &ignore) {
            if manager.cancel.load(Ordering::SeqCst) {
                return Ok(true);
            }
            let Ok(entry) = entry else { continue };
            if entry.file_type().is_file() && is_audio_file(entry.path()) {
                total += 1;
            }
        }
    }
    set_progress(manager, |p| {
        p.total = total;
        p.phase = "scanning".into();
    });

    let now = now_secs();
    let mut batch: Vec<Track> = Vec::with_capacity(50);
    let mut added = 0usize;
    let mut updated = 0usize;
    let mut errors = 0usize;
    let mut done = 0usize;
    let mut cancelled = false;

    for root in &config.library_roots {
        if cancelled {
            break;
        }
        if !Path::new(root).is_dir() {
            continue;
        }
        for entry in walker(root, &ignore) {
            if manager.cancel.load(Ordering::SeqCst) {
                cancelled = true;
                break;
            }
            let Ok(entry) = entry else {
                errors += 1;
                continue;
            };
            if !entry.file_type().is_file() || !is_audio_file(entry.path()) {
                continue;
            }
            let path = entry.path();
            let path_str = native_path_string(path);
            let rel = path.strip_prefix(root).unwrap_or(path);
            let rel_path = rel.to_string_lossy().replace('\\', "/");
            let was_known = existing.contains_key(&path_str);

            let track = match read_metadata(path, config) {
                Ok(mut track) => {
                    track.path = path_str.clone();
                    track.root = root.clone();
                    track.rel_path = rel_path.clone();
                    track.ext = extension(path);
                    track.added_at = existing
                        .get(&path_str)
                        .map(|t| t.added_at)
                        .filter(|v| *v > 0)
                        .unwrap_or(now);
                    if track.has_lyrics && track.lyrics_source.is_empty() {
                        // Embedded-only lyrics have no `[re:]` sidecar header;
                        // keep the source recorded when they were written.
                        if let Some(previous) = existing.get(&path_str) {
                            track.lyrics_source = previous.lyrics_source.clone();
                        }
                    }
                    track.updated_at = now;
                    track
                }
                Err(err) => {
                    errors += 1;
                    tracing::warn!("读取失败 {}: {err:#}", path.display());
                    let meta = std::fs::metadata(path).ok();
                    Track {
                        path: path_str.clone(),
                        root: root.clone(),
                        rel_path: rel_path.clone(),
                        filename: path
                            .file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_default(),
                        ext: extension(path),
                        size: meta.as_ref().map(|m| m.len()).unwrap_or(0),
                        mtime: meta
                            .and_then(|m| m.modified().ok())
                            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                            .map(|d| d.as_secs() as i64)
                            .unwrap_or(0),
                        added_at: now,
                        updated_at: now,
                        scan_error: format!("{err:#}"),
                        ..Default::default()
                    }
                }
            };
            if was_known {
                updated += 1;
            } else {
                added += 1;
            }
            batch.push(track);
            done += 1;

            if batch.len() >= 50 {
                store.upsert_many(&mut batch)?;
                batch.clear();
                set_progress(manager, |p| {
                    p.done = done;
                    p.added = added;
                    p.updated = updated;
                    p.errors = errors;
                });
            }
            set_progress(manager, |p| {
                p.done = done;
                p.added = added;
                p.updated = updated;
                p.errors = errors;
                p.current_file = rel_path.clone();
            });
        }
    }

    if !batch.is_empty() {
        store.upsert_many(&mut batch)?;
    }

    let mut removed = 0usize;
    if !cancelled {
        set_progress(manager, |p| p.phase = "cleaning".into());
        for root in &scanned_roots {
            removed += store.delete_missing(root).unwrap_or(0);
        }
    }

    set_progress(manager, |p| {
        p.done = done;
        p.total = total.max(done);
        p.added = added;
        p.updated = updated;
        p.removed = removed;
        p.errors = errors;
        p.current_file.clear();
    });
    Ok(cancelled)
}

fn walker<'a>(
    root: &'a str,
    ignore: &'a HashSet<String>,
) -> impl Iterator<Item = walkdir::Result<DirEntry>> + 'a {
    WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(move |entry| {
            if entry.depth() == 0 {
                return true;
            }
            let name = entry.file_name().to_string_lossy();
            if ignore.contains(name.as_ref()) {
                return false;
            }
            if entry.file_type().is_dir() && name.starts_with('.') {
                return false;
            }
            if entry.path_is_symlink() {
                return false;
            }
            true
        })
}

fn set_progress(manager: &ScanManager, f: impl FnOnce(&mut ScanProgress)) {
    if let Ok(mut progress) = manager.progress.lock() {
        f(&mut progress);
    }
}
