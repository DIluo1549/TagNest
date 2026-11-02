use crate::auth::{self, Auth};
use crate::config::{Config, Paths};
use crate::lyrics;
use crate::models::{year_from_date, Page, Stats, Track, TrackPatch, TrackView};
use crate::naming;
use crate::providers::{Candidate, Providers, SearchQuery};
use crate::scanner::ScanManager;
use crate::store::Store;
use crate::tagger::{self, WriteOptionsExtra};
use crate::util::{extension, join_values, now_secs, safe_join, sidecar_path, split_values};
use axum::body::Body;
use axum::extract::{Path as AxPath, Query, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, RwLock};
use std::time::Instant;
use tokio_util::io::ReaderStream;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<RwLock<Config>>,
    pub store: Arc<Store>,
    pub scan: Arc<ScanManager>,
    pub providers: Arc<Providers>,
    pub auth: Arc<Auth>,
    pub paths: Paths,
    pub started_at: Arc<Instant>,
}

impl AppState {
    pub fn config(&self) -> Config {
        self.config.read().map(|c| c.clone()).unwrap_or_default()
    }
}

#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn bad(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
        }
    }

    fn not_found(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: message.into(),
        }
    }

    fn internal(err: anyhow::Error) -> Self {
        tracing::error!("API error: {err:#}");
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: format!("{err:#}"),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(json!({ "error": self.message }))).into_response()
    }
}

type ApiResult<T> = Result<T, ApiError>;

pub fn router(state: AppState) -> Router {
    let protected = Router::new()
        .route("/api/stats", get(stats))
        .route("/api/tracks", get(list_tracks))
        .route("/api/tracks/normalize", post(normalize_tags))
        .route("/api/tracks/{id}", get(get_track).patch(update_track))
        .route("/api/tracks/{id}/cover", get(get_cover))
        .route("/api/tracks/{id}/stream", get(stream_track))
        .route("/api/tracks/{id}/lyrics", get(get_lyrics))
        .route("/api/lyrics/search", post(search_lyrics))
        .route("/api/lyrics/apply", post(apply_lyrics))
        .route("/api/lyrics/batch", post(batch_lyrics))
        .route("/api/scrape/search", get(search_scrape))
        .route("/api/scrape/auto", post(auto_scrape))
        .route("/api/scrape/apply", post(apply_scrape))
        .route("/api/rename/preview", post(preview_rename))
        .route("/api/rename", post(execute_rename))
        .route("/api/scan", post(start_scan))
        .route("/api/scan/status", get(scan_status))
        .route("/api/scan/cancel", post(cancel_scan))
        .route("/api/settings", get(get_settings).put(update_settings))
        .route("/api/providers", get(get_providers))
        .route("/api/naming/variables", get(naming_variables))
        .route_layer(middleware::from_fn_with_state(state.clone(), require_auth));

    Router::new()
        .route("/api/health", get(health))
        .route("/api/auth/status", get(auth_status))
        .route("/api/auth/login", post(login))
        .route("/api/auth/logout", post(logout))
        .merge(protected)
        .fallback(static_file)
        .with_state(state)
}

async fn require_auth(State(state): State<AppState>, req: Request, next: Next) -> Response {
    if state.auth.authenticated(req.headers()) {
        next.run(req).await
    } else {
        (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "请先登录", "auth_required": true })),
        )
            .into_response()
    }
}

async fn health(State(state): State<AppState>) -> Json<Value> {
    let tracks = state.store.count().unwrap_or(0);
    Json(json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
        "tracks": tracks,
        "auth_enabled": state.auth.enabled(),
        "uptime_secs": state.started_at.elapsed().as_secs(),
    }))
}

#[derive(Deserialize)]
struct LoginRequest {
    password: String,
}

async fn auth_status(State(state): State<AppState>, headers: HeaderMap) -> Json<Value> {
    Json(json!({
        "enabled": state.auth.enabled(),
        "authenticated": state.auth.authenticated(&headers),
    }))
}

async fn login(State(state): State<AppState>, Json(body): Json<LoginRequest>) -> Response {
    if state.auth.enabled() && !state.auth.verify_password(&body.password) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "密码错误" })),
        )
            .into_response();
    }
    let cookie = state.auth.issue_cookie();
    let mut response = Json(json!({ "ok": true })).into_response();
    if let Ok(value) = HeaderValue::from_str(&auth::set_cookie_header(&cookie, false)) {
        response.headers_mut().insert(header::SET_COOKIE, value);
    }
    response
}

async fn logout() -> Response {
    let mut response = Json(json!({ "ok": true })).into_response();
    if let Ok(value) = HeaderValue::from_str(&auth::clear_cookie_header()) {
        response.headers_mut().insert(header::SET_COOKIE, value);
    }
    response
}

async fn stats(State(state): State<AppState>) -> ApiResult<Json<Stats>> {
    let tracks = state.store.all().map_err(ApiError::internal)?;
    Ok(Json(build_stats(&tracks)))
}

fn build_stats(tracks: &[Track]) -> Stats {
    let artists: HashSet<&str> = tracks
        .iter()
        .flat_map(|t| {
            std::iter::once(t.artist.as_str()).chain(t.artists.iter().map(String::as_str))
        })
        .filter(|v| !v.is_empty())
        .collect();
    let albums: HashSet<(&str, &str)> = tracks
        .iter()
        .filter(|t| !t.album.is_empty())
        .map(|t| {
            (
                t.album.as_str(),
                t.albumartist.first().map(String::as_str).unwrap_or(""),
            )
        })
        .collect();
    let mut formats: BTreeMap<String, usize> = BTreeMap::new();
    for track in tracks {
        *formats.entry(track.format.clone()).or_default() += 1;
    }
    let mut formats: Vec<_> = formats
        .into_iter()
        .map(|(format, count)| crate::models::FormatStat { format, count })
        .collect();
    formats.sort_by(|a, b| b.count.cmp(&a.count));
    Stats {
        total: tracks.len(),
        artists: artists.len(),
        albums: albums.len(),
        missing_core: tracks.iter().filter(|t| t.is_missing_core_tags()).count(),
        with_lyrics: tracks.iter().filter(|t| t.has_lyrics).count(),
        with_cover: tracks.iter().filter(|t| t.has_cover).count(),
        total_size: tracks.iter().map(|t| t.size).sum(),
        total_duration_ms: tracks.iter().map(|t| t.duration_ms).sum(),
        formats,
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct TrackListQuery {
    q: String,
    limit: Option<usize>,
    offset: Option<usize>,
    sort: String,
    filter: String,
    root: String,
}

async fn list_tracks(
    State(state): State<AppState>,
    Query(query): Query<TrackListQuery>,
) -> ApiResult<Json<Page<TrackView>>> {
    let config = state.config();
    let mut tracks = state.store.all().map_err(ApiError::internal)?;
    let needle = query.q.trim().to_lowercase();
    tracks.retain(|track| {
        if !query.root.is_empty() && track.root != query.root {
            return false;
        }
        match query.filter.as_str() {
            "missing" if !track.is_missing_core_tags() => return false,
            "no_lyrics" if track.has_lyrics => return false,
            "no_cover" if track.has_cover => return false,
            "lyrics" if !track.has_lyrics => return false,
            "cover" if !track.has_cover => return false,
            other if other.starts_with("format:") => {
                let wanted = other.trim_start_matches("format:").to_lowercase();
                if track.format.to_lowercase() != wanted {
                    return false;
                }
            }
            _ => {}
        }
        if needle.is_empty() {
            return true;
        }
        track.title.to_lowercase().contains(&needle)
            || track.album.to_lowercase().contains(&needle)
            || track.filename.to_lowercase().contains(&needle)
            || track.artist.to_lowercase().contains(&needle)
            || track
                .artists
                .iter()
                .any(|v| v.to_lowercase().contains(&needle))
    });
    let total = tracks.len();
    match query.sort.as_str() {
        "title" => tracks.sort_by(|a, b| a.title.to_lowercase().cmp(&b.title.to_lowercase())),
        "album" => tracks.sort_by(|a, b| {
            a.album
                .to_lowercase()
                .cmp(&b.album.to_lowercase())
                .then_with(|| a.disc.cmp(&b.disc))
                .then_with(|| a.track.cmp(&b.track))
        }),
        "added" => tracks.sort_by(|a, b| b.added_at.cmp(&a.added_at)),
        "updated" => tracks.sort_by(|a, b| b.updated_at.cmp(&a.updated_at)),
        "duration" => tracks.sort_by(|a, b| b.duration_ms.cmp(&a.duration_ms)),
        "path" => tracks.sort_by(|a, b| a.rel_path.cmp(&b.rel_path)),
        _ => tracks.sort_by(|a, b| {
            a.primary_artist()
                .to_lowercase()
                .cmp(&b.primary_artist().to_lowercase())
                .then_with(|| a.album.to_lowercase().cmp(&b.album.to_lowercase()))
                .then_with(|| a.disc.cmp(&b.disc))
                .then_with(|| a.track.cmp(&b.track))
                .then_with(|| a.title.to_lowercase().cmp(&b.title.to_lowercase()))
        }),
    }
    let limit = query.limit.unwrap_or(100).clamp(1, 500);
    let offset = query.offset.unwrap_or(0).min(total);
    let items = tracks
        .into_iter()
        .skip(offset)
        .take(limit)
        .map(|track| TrackView::new(track, &config))
        .collect();
    Ok(Json(Page {
        items,
        total,
        offset,
        limit,
    }))
}

async fn get_track(
    State(state): State<AppState>,
    AxPath(id): AxPath<u64>,
) -> ApiResult<Json<TrackView>> {
    let track = state
        .store
        .get(id)
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found("歌曲不存在"))?;
    let config = state.config();
    Ok(Json(TrackView::new(track, &config)))
}

async fn update_track(
    State(state): State<AppState>,
    AxPath(id): AxPath<u64>,
    Json(patch): Json<TrackPatch>,
) -> ApiResult<Json<TrackView>> {
    if patch.is_empty() {
        return Err(ApiError::bad("没有需要保存的字段"));
    }
    let mut track = state
        .store
        .get(id)
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found("歌曲不存在"))?;
    let config = state.config();
    patch.apply(&mut track, now_secs());
    tagger::write_metadata(
        Path::new(&track.path),
        &track,
        &config,
        &WriteOptionsExtra::default(),
    )
    .map_err(ApiError::internal)?;
    state.store.save(&track).map_err(ApiError::internal)?;
    invalidate_cover(&state.paths, id);
    Ok(Json(TrackView::new(track, &config)))
}

#[derive(Deserialize)]
struct NormalizeRequest {
    ids: Vec<u64>,
    #[serde(default)]
    fields: Vec<String>,
    input_separators: Option<Vec<String>>,
    output_separator: Option<String>,
}

async fn normalize_tags(
    State(state): State<AppState>,
    Json(body): Json<NormalizeRequest>,
) -> ApiResult<Json<Value>> {
    if body.ids.is_empty() {
        return Err(ApiError::bad("请选择歌曲"));
    }
    let config = state.config();
    // Empty output means "use the per-field defaults from settings".
    let output = body
        .output_separator
        .clone()
        .filter(|value| !value.trim().is_empty());
    let input = body
        .input_separators
        .clone()
        .unwrap_or_else(|| config.split_separators.clone());
    let fields: HashSet<String> = if body.fields.is_empty() {
        [
            "artist",
            "artists",
            "albumartist",
            "genres",
            "composers",
            "lyricists",
        ]
        .iter()
        .map(|v| v.to_string())
        .collect()
    } else {
        body.fields.iter().cloned().collect()
    };
    let mut extra = WriteOptionsExtra::default();
    let mut only_fields = HashSet::new();
    for field in &fields {
        if matches!(
            field.as_str(),
            "artist" | "artists" | "albumartist" | "genres" | "composers" | "lyricists"
        ) {
            if let Some(output) = &output {
                if field != "artist" {
                    extra
                        .separator_overrides
                        .insert(field.clone(), output.clone());
                }
            }
            only_fields.insert(field.clone());
        }
    }
    extra.only_fields = Some(only_fields);
    let mut updated = 0usize;
    let mut errors = Vec::new();
    for id in body.ids.iter().take(500) {
        let Some(mut track) = state.store.get(*id).map_err(ApiError::internal)? else {
            errors.push(json!({ "id": id, "error": "歌曲不存在" }));
            continue;
        };
        for field in &fields {
            match field.as_str() {
                "artist" => {
                    let separator = output
                        .clone()
                        .unwrap_or_else(|| config.separator_for("artists").to_string());
                    track.artist = normalize_joined(&track.artist, &input, &separator);
                }
                "artists" => track.artists = normalize_values(&track.artists, &input),
                "albumartist" => track.albumartist = normalize_values(&track.albumartist, &input),
                "genres" => track.genres = normalize_values(&track.genres, &input),
                "composers" => track.composers = normalize_values(&track.composers, &input),
                "lyricists" => track.lyricists = normalize_values(&track.lyricists, &input),
                other => {
                    errors.push(
                        json!({ "id": id, "error": format!("不支持批量处理的字段: {other}") }),
                    );
                }
            }
        }
        track.updated_at = now_secs();
        match tagger::write_metadata(Path::new(&track.path), &track, &config, &extra) {
            Ok(()) => match state.store.save(&track) {
                Ok(()) => updated += 1,
                Err(err) => errors.push(json!({ "id": id, "error": format!("{err:#}") })),
            },
            Err(err) => errors.push(json!({ "id": id, "error": format!("{err:#}") })),
        }
    }
    Ok(Json(json!({ "updated": updated, "errors": errors })))
}

fn normalize_joined(value: &str, input: &[String], output: &str) -> String {
    let pieces = normalize_values(&[value.to_string()], input);
    join_values(&pieces, output)
}

fn normalize_values(values: &[String], input: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for value in values {
        for piece in split_values(value, input) {
            if !out.iter().any(|v: &String| *v == piece) {
                out.push(piece);
            }
        }
    }
    out
}

async fn start_scan(State(state): State<AppState>) -> ApiResult<Json<Value>> {
    let config = state.config();
    state
        .scan
        .start(Arc::clone(&state.store), config)
        .map_err(|err| ApiError::bad(format!("{err:#}")))?;
    Ok(Json(json!({ "ok": true })))
}

async fn scan_status(State(state): State<AppState>) -> Json<Value> {
    Json(serde_json::to_value(state.scan.snapshot()).unwrap_or_else(|_| json!({})))
}

async fn cancel_scan(State(state): State<AppState>) -> Json<Value> {
    state.scan.cancel();
    Json(json!({ "ok": true }))
}

async fn get_cover(State(state): State<AppState>, AxPath(id): AxPath<u64>) -> Response {
    let mime_path = state.paths.covers.join(format!("{id}.mime"));
    let bin_path = state.paths.covers.join(format!("{id}.bin"));
    if bin_path.exists() {
        if let Ok(bytes) = std::fs::read(&bin_path) {
            let mime =
                std::fs::read_to_string(&mime_path).unwrap_or_else(|_| "image/jpeg".to_string());
            return image_response(bytes, &mime);
        }
    }
    let Ok(Some(track)) = state.store.get(id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Some((bytes, mime)) = tagger::read_cover(Path::new(&track.path)) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let _ = std::fs::write(&bin_path, &bytes);
    let _ = std::fs::write(&mime_path, &mime);
    image_response(bytes, &mime)
}

fn image_response(bytes: Vec<u8>, mime: &str) -> Response {
    let mut response = Response::new(Body::from(bytes));
    if let Ok(value) = HeaderValue::from_str(mime) {
        response.headers_mut().insert(header::CONTENT_TYPE, value);
    }
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=3600"),
    );
    response
}

async fn stream_track(State(state): State<AppState>, AxPath(id): AxPath<u64>) -> Response {
    let Ok(Some(track)) = state.store.get(id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Ok(file) = tokio::fs::File::open(&track.path).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let len = file.metadata().await.map(|m| m.len()).unwrap_or(0);
    let mime = audio_mime(&extension(Path::new(&track.path)));
    let stream = ReaderStream::new(file);
    let mut response = Response::new(Body::from_stream(stream));
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(mime));
    response.headers_mut().insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_str(&len.to_string()).unwrap(),
    );
    response
        .headers_mut()
        .insert(header::ACCEPT_RANGES, HeaderValue::from_static("none"));
    response
}

fn audio_mime(ext: &str) -> &'static str {
    match ext {
        "mp3" => "audio/mpeg",
        "flac" => "audio/flac",
        "m4a" | "mp4" => "audio/mp4",
        "ogg" | "oga" => "audio/ogg",
        "opus" => "audio/opus",
        "wav" => "audio/wav",
        "aif" | "aiff" => "audio/aiff",
        "ape" => "audio/x-ape",
        "wv" => "audio/x-wavpack",
        "aac" => "audio/aac",
        "spx" => "audio/ogg",
        _ => "application/octet-stream",
    }
}

async fn get_lyrics(
    State(state): State<AppState>,
    AxPath(id): AxPath<u64>,
) -> ApiResult<Json<Value>> {
    let track = state
        .store
        .get(id)
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found("歌曲不存在"))?;
    let content = lyrics::load(Path::new(&track.path));
    let (text, lines, meta) = match content {
        Some(value) => {
            let parsed = lyrics::parse_lrc(&value);
            (value, json!(parsed.lines), json!(parsed.meta))
        }
        None => (String::new(), json!([]), json!({})),
    };
    Ok(Json(json!({
        "lyrics": text,
        "lines": lines,
        "meta": meta,
        "has_lyrics": !text.is_empty(),
        "source": track.lyrics_source,
    })))
}

#[derive(Deserialize)]
struct LyricsSearchRequest {
    track_id: u64,
    #[serde(default)]
    sources: Vec<String>,
}

async fn search_lyrics(
    State(state): State<AppState>,
    Json(body): Json<LyricsSearchRequest>,
) -> ApiResult<Json<Value>> {
    let track = state
        .store
        .get(body.track_id)
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found("歌曲不存在"))?;
    if track.title.trim().is_empty() {
        return Err(ApiError::bad("歌曲没有标题，无法搜索歌词"));
    }
    let config = state.config();
    let sources = if body.sources.is_empty() {
        None
    } else {
        Some(body.sources.as_slice())
    };
    let candidates = state
        .providers
        .search_lyrics(&config, &track, sources)
        .await;
    Ok(Json(json!({
        "track": TrackView::new(track, &config),
        "candidates": candidates,
    })))
}

#[derive(Deserialize)]
struct LyricsApplyRequest {
    track_id: u64,
    source: String,
    source_id: String,
    #[serde(default)]
    bilingual: Option<bool>,
    #[serde(default)]
    embed: Option<bool>,
    /// False writes tags only and does not create/overwrite a `.lrc` sidecar.
    #[serde(default)]
    write_file: Option<bool>,
    #[serde(default = "default_true")]
    overwrite: bool,
}

#[derive(Deserialize)]
struct LyricsBatchRequest {
    ids: Vec<u64>,
    #[serde(default)]
    bilingual: Option<bool>,
    #[serde(default)]
    embed: Option<bool>,
    /// False writes tags only and does not create/overwrite `.lrc` sidecars.
    #[serde(default)]
    write_file: Option<bool>,
    #[serde(default = "default_true")]
    overwrite: bool,
    #[serde(default)]
    sources: Vec<String>,
}

fn default_true() -> bool {
    true
}

async fn apply_lyrics(
    State(state): State<AppState>,
    Json(body): Json<LyricsApplyRequest>,
) -> ApiResult<Json<Value>> {
    let mut track = state
        .store
        .get(body.track_id)
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found("歌曲不存在"))?;
    let config = state.config();
    if track.has_lyrics && !body.overwrite {
        return Ok(Json(json!({
            "ok": true,
            "skipped": true,
            "message": "已有歌词，未覆盖",
        })));
    }
    let candidate = state
        .providers
        .fetch_lyrics(&config, &body.source, &body.source_id)
        .await
        .map_err(ApiError::internal)?;
    if candidate.instrumental {
        return Err(ApiError::bad("该来源标记为纯音乐，没有歌词"));
    }
    if candidate.synced_lyrics.trim().is_empty()
        && candidate.plain_lyrics.trim().is_empty()
        && candidate.translated_lyrics.trim().is_empty()
    {
        return Err(ApiError::bad("该来源没有可用歌词"));
    }
    let bilingual = body.bilingual.unwrap_or(config.lyrics.bilingual);
    let embed = body.embed.unwrap_or(config.lyrics.embed);
    let write_file = body.write_file.unwrap_or(config.lyrics.write_file);
    if !write_file && !embed {
        return Err(ApiError::bad("不新建歌词文件时，必须选择嵌入音频标签"));
    }
    let content = lyrics::compose(
        &track,
        &body.source,
        &candidate.synced_lyrics,
        &candidate.plain_lyrics,
        &candidate.translated_lyrics,
        bilingual,
    );
    if content.trim().is_empty() {
        return Err(ApiError::bad("该来源没有可用歌词"));
    }
    if write_file {
        lyrics::write(Path::new(&track.path), &content).map_err(ApiError::internal)?;
    }
    if embed {
        let extra = WriteOptionsExtra {
            lyrics: Some(content.clone()),
            embed_lyrics: true,
            ..Default::default()
        };
        tagger::write_metadata(Path::new(&track.path), &track, &config, &extra)
            .map_err(ApiError::internal)?;
    }
    track.has_lyrics = write_file || embed;
    track.lyrics_source = body.source.clone();
    track.updated_at = now_secs();
    state.store.save(&track).map_err(ApiError::internal)?;
    Ok(Json(json!({
        "ok": true,
        "lyrics": content,
        "has_translation": candidate.has_translation || !candidate.translated_lyrics.is_empty(),
        "embedded": embed,
        "wrote_file": write_file,
        "source": body.source,
    })))
}

async fn batch_lyrics(
    State(state): State<AppState>,
    Json(body): Json<LyricsBatchRequest>,
) -> ApiResult<Json<Value>> {
    if body.ids.is_empty() {
        return Err(ApiError::bad("请选择歌曲"));
    }
    let config = state.config();
    let bilingual = body.bilingual.unwrap_or(config.lyrics.bilingual);
    let embed = body.embed.unwrap_or(config.lyrics.embed);
    let write_file = body.write_file.unwrap_or(config.lyrics.write_file);
    if !write_file && !embed {
        return Err(ApiError::bad("不新建歌词文件时，必须选择嵌入音频标签"));
    }
    let sources = if body.sources.is_empty() {
        None
    } else {
        Some(body.sources.as_slice())
    };
    let mut results = Vec::new();
    let mut updated = 0usize;
    for id in body.ids.iter().take(50) {
        let Some(mut track) = state.store.get(*id).map_err(ApiError::internal)? else {
            results.push(json!({ "id": id, "ok": false, "message": "歌曲不存在" }));
            continue;
        };
        if track.has_lyrics && !body.overwrite {
            results.push(json!({ "id": id, "ok": true, "skipped": true, "message": "已有歌词" }));
            continue;
        }
        if track.title.trim().is_empty() {
            results.push(json!({ "id": id, "ok": false, "message": "缺少标题" }));
            continue;
        }
        let candidates = state
            .providers
            .search_lyrics(&config, &track, sources)
            .await;
        let best = candidates
            .into_iter()
            .filter(|c| {
                c.match_score >= 0.5
                    && !c.instrumental
                    && (!c.synced_lyrics.trim().is_empty()
                        || !c.plain_lyrics.trim().is_empty()
                        || !c.translated_lyrics.trim().is_empty())
            })
            .max_by(|a, b| {
                let translation_bonus = |candidate: &crate::providers::LyricsCandidate| {
                    if bilingual
                        && (candidate.has_translation || !candidate.translated_lyrics.is_empty())
                    {
                        0.1
                    } else {
                        0.0
                    }
                };
                let a_score =
                    a.match_score + if a.has_synced { 0.08 } else { 0.0 } + translation_bonus(a);
                let b_score =
                    b.match_score + if b.has_synced { 0.08 } else { 0.0 } + translation_bonus(b);
                a_score
                    .partial_cmp(&b_score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
        let Some(candidate) = best else {
            results.push(json!({ "id": id, "ok": false, "message": "没有找到匹配歌词" }));
            continue;
        };
        let content = lyrics::compose(
            &track,
            &candidate.source,
            &candidate.synced_lyrics,
            &candidate.plain_lyrics,
            &candidate.translated_lyrics,
            bilingual,
        );
        if content.trim().is_empty() {
            results.push(json!({ "id": id, "ok": false, "message": "歌词为空" }));
            continue;
        }
        if write_file {
            if let Err(err) = lyrics::write(Path::new(&track.path), &content) {
                results.push(json!({ "id": id, "ok": false, "message": format!("{err:#}") }));
                continue;
            }
        }
        if embed {
            let extra = WriteOptionsExtra {
                lyrics: Some(content),
                embed_lyrics: true,
                ..Default::default()
            };
            if let Err(err) =
                tagger::write_metadata(Path::new(&track.path), &track, &config, &extra)
            {
                results.push(json!({ "id": id, "ok": false, "message": format!("{err:#}") }));
                continue;
            }
        }
        track.has_lyrics = write_file || embed;
        track.lyrics_source = candidate.source.clone();
        track.updated_at = now_secs();
        if let Err(err) = state.store.save(&track) {
            results.push(json!({ "id": id, "ok": false, "message": format!("{err:#}") }));
            continue;
        }
        updated += 1;
        results.push(json!({
            "id": id,
            "ok": true,
            "title": track.title,
            "source": candidate.source,
            "score": candidate.match_score,
            "has_translation": candidate.has_translation || !candidate.translated_lyrics.is_empty(),
            "has_synced": candidate.has_synced,
            "wrote_file": write_file,
        }));
    }
    Ok(Json(json!({
        "updated": updated,
        "total": body.ids.len(),
        "results": results,
        "bilingual": bilingual,
        "wrote_file": write_file,
    })))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ScrapeSearchQuery {
    q: String,
    title: String,
    artist: String,
    album: String,
    track_id: Option<u64>,
    sources: String,
}

async fn search_scrape(
    State(state): State<AppState>,
    Query(query): Query<ScrapeSearchQuery>,
) -> ApiResult<Json<Value>> {
    let config = state.config();
    let mut search = if let Some(id) = query.track_id {
        let track = state
            .store
            .get(id)
            .map_err(ApiError::internal)?
            .ok_or_else(|| ApiError::not_found("歌曲不存在"))?;
        SearchQuery::from(&track)
    } else {
        SearchQuery {
            title: if query.title.is_empty() {
                query.q.trim().to_string()
            } else {
                query.title.trim().to_string()
            },
            artist: query.artist.trim().to_string(),
            album: query.album.trim().to_string(),
            duration_ms: 0,
            isrc: String::new(),
        }
    };
    if !query.title.is_empty() {
        search.title = query.title.trim().to_string();
    }
    if !query.artist.is_empty() {
        search.artist = query.artist.trim().to_string();
    }
    if !query.album.is_empty() {
        search.album = query.album.trim().to_string();
    }
    if search.title.is_empty() {
        return Err(ApiError::bad("请输入歌曲标题或选择歌曲"));
    }
    let source_list: Vec<String> = query
        .sources
        .split(',')
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToOwned::to_owned)
        .collect();
    let sources = if source_list.is_empty() {
        None
    } else {
        Some(source_list.as_slice())
    };
    let candidates = state
        .providers
        .search_tracks(&config, &search, sources)
        .await;
    Ok(Json(json!({
        "query": {
            "title": search.title,
            "artist": search.artist,
            "album": search.album,
        },
        "candidates": candidates,
    })))
}

#[derive(Deserialize)]
struct AutoScrapeRequest {
    ids: Vec<u64>,
    #[serde(default)]
    threshold: Option<f64>,
    #[serde(default)]
    overwrite: Option<bool>,
    #[serde(default)]
    cover: Option<bool>,
    #[serde(default)]
    sources: Vec<String>,
    #[serde(default)]
    fields: Vec<String>,
}

async fn auto_scrape(
    State(state): State<AppState>,
    Json(body): Json<AutoScrapeRequest>,
) -> ApiResult<Json<Value>> {
    if body.ids.is_empty() {
        return Err(ApiError::bad("请选择歌曲"));
    }
    let config = state.config();
    let threshold = body
        .threshold
        .unwrap_or(config.auto_match_threshold)
        .clamp(0.0, 1.0);
    let overwrite = body.overwrite.unwrap_or(true);
    let fetch_cover = body.cover.unwrap_or(true);
    let sources = if body.sources.is_empty() {
        None
    } else {
        Some(body.sources.as_slice())
    };
    let fields = if body.fields.is_empty() {
        None
    } else {
        Some(
            body.fields
                .iter()
                .map(String::as_str)
                .collect::<HashSet<_>>(),
        )
    };
    let mut results = Vec::new();
    let mut updated = 0usize;
    for id in body.ids.iter().take(50) {
        let Some(mut track) = state.store.get(*id).map_err(ApiError::internal)? else {
            results.push(json!({ "id": id, "ok": false, "message": "歌曲不存在" }));
            continue;
        };
        if track.title.trim().is_empty() && track.primary_artist().is_empty() {
            results.push(json!({ "id": id, "ok": false, "message": "缺少标题和歌手" }));
            continue;
        }
        let query = SearchQuery::from(&track);
        let candidates = state
            .providers
            .search_tracks(&config, &query, sources)
            .await;
        let Some(candidate) = candidates.first().cloned() else {
            results.push(json!({ "id": id, "ok": false, "message": "没有搜索结果" }));
            continue;
        };
        if candidate.match_score < threshold {
            results.push(json!({
                "id": id,
                "ok": false,
                "message": "匹配度低于阈值",
                "score": candidate.match_score,
                "candidate": candidate,
            }));
            continue;
        }
        let cover = if fetch_cover && !candidate.cover_url.is_empty() {
            match download_cover(&state.providers.client, &candidate.cover_url, &config).await {
                Ok(bytes) => Some(bytes),
                Err(err) => {
                    tracing::warn!("下载封面失败 {}: {err:#}", candidate.cover_url);
                    None
                }
            }
        } else {
            None
        };
        merge_candidate(&mut track, &candidate, overwrite, fields.clone(), &config);
        track.updated_at = now_secs();
        let extra = WriteOptionsExtra {
            cover,
            replace_cover: overwrite,
            separator_overrides: scrape_separator_overrides(&config),
            ..Default::default()
        };
        let wrote_cover = extra.cover.is_some();
        match tagger::write_metadata(Path::new(&track.path), &track, &config, &extra) {
            Ok(()) => {
                if wrote_cover {
                    track.has_cover = tagger::read_cover(Path::new(&track.path)).is_some();
                }
                match state.store.save(&track) {
                    Ok(()) => {
                        invalidate_cover(&state.paths, *id);
                        updated += 1;
                        results.push(json!({
                            "id": id,
                            "ok": true,
                            "title": track.title,
                            "score": candidate.match_score,
                            "source": candidate.source,
                            "candidate": candidate,
                        }));
                    }
                    Err(err) => results.push(json!({
                        "id": id,
                        "ok": false,
                        "message": format!("{err:#}")
                    })),
                }
            }
            Err(err) => results.push(json!({
                "id": id,
                "ok": false,
                "message": format!("{err:#}")
            })),
        }
    }
    Ok(Json(json!({
        "updated": updated,
        "total": body.ids.len(),
        "threshold": threshold,
        "results": results,
    })))
}

#[derive(Deserialize)]
struct ApplyScrapeRequest {
    track_id: u64,
    candidate: Candidate,
    #[serde(default)]
    overwrite: Option<bool>,
    #[serde(default)]
    fields: Vec<String>,
    #[serde(default)]
    cover: Option<bool>,
}

async fn apply_scrape(
    State(state): State<AppState>,
    Json(body): Json<ApplyScrapeRequest>,
) -> ApiResult<Json<Value>> {
    let mut track = state
        .store
        .get(body.track_id)
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found("歌曲不存在"))?;
    let config = state.config();
    let overwrite = body.overwrite.unwrap_or(true);
    let fields = if body.fields.is_empty() {
        None
    } else {
        Some(
            body.fields
                .iter()
                .map(String::as_str)
                .collect::<HashSet<_>>(),
        )
    };
    let cover = if body.cover.unwrap_or(true) && !body.candidate.cover_url.is_empty() {
        match download_cover(&state.providers.client, &body.candidate.cover_url, &config).await {
            Ok(bytes) => Some(bytes),
            Err(err) => {
                tracing::warn!("下载封面失败 {}: {err:#}", body.candidate.cover_url);
                None
            }
        }
    } else {
        None
    };
    merge_candidate(&mut track, &body.candidate, overwrite, fields, &config);
    track.updated_at = now_secs();
    let extra = WriteOptionsExtra {
        cover,
        replace_cover: overwrite,
        separator_overrides: scrape_separator_overrides(&config),
        ..Default::default()
    };
    let wrote_cover = extra.cover.is_some();
    tagger::write_metadata(Path::new(&track.path), &track, &config, &extra)
        .map_err(ApiError::internal)?;
    if wrote_cover {
        track.has_cover = tagger::read_cover(Path::new(&track.path)).is_some();
    }
    state.store.save(&track).map_err(ApiError::internal)?;
    invalidate_cover(&state.paths, body.track_id);
    Ok(Json(json!({
        "ok": true,
        "track": TrackView::new(track, &config),
    })))
}

fn scrape_separator_overrides(config: &Config) -> HashMap<String, String> {
    let mut overrides = HashMap::new();
    overrides.insert("artists".to_string(), config.scrape_separator.clone());
    overrides.insert("albumartist".to_string(), config.scrape_separator.clone());
    overrides
}

fn normalized_scraped_names(
    raw: &str,
    values: &[String],
    config: &Config,
) -> (String, Vec<String>) {
    let mut names = Vec::new();
    for value in values {
        for piece in split_values(value, &config.split_separators) {
            if !names.iter().any(|existing: &String| existing == &piece) {
                names.push(piece);
            }
        }
    }
    if names.is_empty() && !raw.trim().is_empty() {
        names = split_values(raw, &config.split_separators);
    }
    let display = if names.len() > 1 {
        join_values(&names, &config.scrape_separator)
    } else if !raw.trim().is_empty() {
        raw.trim().to_string()
    } else {
        names.first().cloned().unwrap_or_default()
    };
    (display, names)
}

fn normalized_scraped_list(values: &[String], config: &Config) -> Vec<String> {
    let mut names = Vec::new();
    for value in values {
        for piece in split_values(value, &config.split_separators) {
            if !names.iter().any(|existing: &String| existing == &piece) {
                names.push(piece);
            }
        }
    }
    names
}

fn merge_candidate(
    track: &mut Track,
    candidate: &Candidate,
    overwrite: bool,
    fields: Option<HashSet<&str>>,
    config: &Config,
) {
    let wanted = |name: &str| fields.as_ref().map(|f| f.contains(name)).unwrap_or(true);
    macro_rules! set_text {
        ($field:ident, $value:expr) => {{
            if wanted(stringify!($field)) {
                let value: String = $value;
                if !value.trim().is_empty() && (overwrite || track.$field.trim().is_empty()) {
                    track.$field = value.trim().to_string();
                }
            }
        }};
    }
    macro_rules! set_vec {
        ($field:ident, $value:expr) => {{
            if wanted(stringify!($field)) {
                let value: Vec<String> = $value;
                if !value.is_empty() && (overwrite || track.$field.is_empty()) {
                    track.$field = value;
                }
            }
        }};
    }
    macro_rules! set_num {
        ($field:ident, $value:expr) => {{
            if wanted(stringify!($field)) {
                if let Some(value) = $value {
                    if overwrite || track.$field.is_none() {
                        track.$field = Some(value);
                    }
                }
            }
        }};
    }

    set_text!(title, candidate.title.clone());
    let (display_artist, artist_values) =
        normalized_scraped_names(&candidate.artist, &candidate.artists, config);
    set_text!(artist, display_artist);
    set_vec!(artists, artist_values);
    set_vec!(
        albumartist,
        normalized_scraped_list(&candidate.albumartist, config)
    );
    set_text!(album, candidate.album.clone());
    if wanted("track") {
        set_num!(track, candidate.track);
    }
    if wanted("track_total") {
        set_num!(track_total, candidate.track_total);
    }
    if wanted("disc") {
        set_num!(disc, candidate.disc);
    }
    if wanted("disc_total") {
        set_num!(disc_total, candidate.disc_total);
    }
    set_text!(date, candidate.date.clone());
    if wanted("year") && candidate.year.is_some() && (overwrite || track.year.is_none()) {
        track.year = candidate.year;
    }
    set_vec!(genres, candidate.genres.clone());
    set_text!(isrc, candidate.isrc.clone());
    set_text!(label, candidate.label.clone());
    set_text!(catalog_number, candidate.catalog_number.clone());
    set_text!(mb_track_id, candidate.mb_track_id.clone());
    set_text!(mb_album_id, candidate.mb_album_id.clone());
    set_text!(mb_artist_id, candidate.mb_artist_id.clone());
    if wanted("bpm") && candidate.bpm.is_some() && (overwrite || track.bpm.is_none()) {
        track.bpm = candidate.bpm;
    }
    if track.year.is_none() {
        track.year = year_from_date(&track.date);
    }
}

#[derive(Deserialize)]
struct RenameRequest {
    ids: Vec<u64>,
    template: String,
    /// Optional per-rename separator for `{artists}`; empty uses settings.
    #[serde(default)]
    artist_separator: Option<String>,
}

#[derive(Serialize)]
struct RenameItem {
    id: u64,
    old_path: String,
    new_path: String,
    valid: bool,
    error: String,
}

async fn preview_rename(
    State(state): State<AppState>,
    Json(body): Json<RenameRequest>,
) -> ApiResult<Json<Value>> {
    if body.ids.is_empty() {
        return Err(ApiError::bad("请选择歌曲"));
    }
    if let Err(err) = naming::validate_template(&body.template) {
        return Err(ApiError::bad(err.to_string()));
    }
    let config = state.config();
    let (items, ok) = match build_rename_items(
        &state,
        &config,
        &body.ids,
        &body.template,
        body.artist_separator.as_deref(),
    ) {
        Ok(value) => value,
        Err(err) => return Err(ApiError::internal(err)),
    };
    Ok(Json(json!({
        "ok": ok,
        "items": items,
        "template": body.template,
    })))
}

fn build_rename_items(
    state: &AppState,
    config: &Config,
    ids: &[u64],
    template: &str,
    artist_separator: Option<&str>,
) -> anyhow::Result<(Vec<RenameItem>, bool)> {
    let mut items = Vec::new();
    let mut targets: HashMap<String, u64> = HashMap::new();
    let mut ok = true;
    for id in ids.iter().take(500) {
        let Some(track) = state.store.get(*id)? else {
            items.push(RenameItem {
                id: *id,
                old_path: String::new(),
                new_path: String::new(),
                valid: false,
                error: "歌曲不存在".into(),
            });
            ok = false;
            continue;
        };
        let rendered = match naming::render(&track, template, config, artist_separator) {
            Ok(value) => value,
            Err(err) => {
                ok = false;
                items.push(RenameItem {
                    id: *id,
                    old_path: track.rel_path.clone(),
                    new_path: String::new(),
                    valid: false,
                    error: err.to_string(),
                });
                continue;
            }
        };
        let Some(target) = safe_join(Path::new(&track.root), &rendered.relative) else {
            ok = false;
            items.push(RenameItem {
                id: *id,
                old_path: track.rel_path.clone(),
                new_path: rendered.display,
                valid: false,
                error: "目标路径越界".into(),
            });
            continue;
        };
        let target_key = target.to_string_lossy().to_lowercase();
        let mut error = String::new();
        let target_lrc = sidecar_path(&target);
        if target == Path::new(&track.path) {
            error = "文件名没有变化".into();
        } else if target.exists() {
            error = "目标文件已存在".into();
        } else if target_lrc.exists() {
            error = "目标歌词文件已存在".into();
        } else if let Some(other) = targets.get(&target_key) {
            if *other != *id {
                error = "多首歌曲生成了相同目标路径".into();
            }
        }
        if !error.is_empty() {
            ok = false;
        }
        targets.insert(target_key, *id);
        items.push(RenameItem {
            id: *id,
            old_path: track.rel_path.clone(),
            new_path: target
                .strip_prefix(&track.root)
                .map(|p| p.to_string_lossy().replace('\\', "/"))
                .unwrap_or_else(|_| rendered.display),
            valid: error.is_empty(),
            error,
        });
    }
    Ok((items, ok))
}

async fn execute_rename(
    State(state): State<AppState>,
    Json(body): Json<RenameRequest>,
) -> ApiResult<Json<Value>> {
    let config = state.config();
    let (items, ok) = build_rename_items(
        &state,
        &config,
        &body.ids,
        &body.template,
        body.artist_separator.as_deref(),
    )
    .map_err(ApiError::internal)?;
    if !ok {
        return Ok(Json(json!({
            "ok": false,
            "moved": 0,
            "items": items,
            "message": "预检未通过，没有移动任何文件",
        })));
    }
    let mut moved: Vec<(
        u64,
        std::path::PathBuf,
        std::path::PathBuf,
        Option<std::path::PathBuf>,
        Option<std::path::PathBuf>,
    )> = Vec::new();
    for item in &items {
        let Some(track) = state.store.get(item.id).map_err(ApiError::internal)? else {
            return Ok(Json(
                json!({ "ok": false, "message": "歌曲不存在", "items": items }),
            ));
        };
        let source = std::path::PathBuf::from(&track.path);
        let target = safe_join(Path::new(&track.root), Path::new(&item.new_path))
            .ok_or_else(|| ApiError::bad("目标路径越界"))?;
        if let Some(parent) = target.parent() {
            if let Err(err) = std::fs::create_dir_all(parent) {
                rollback_moves(&moved);
                return Err(ApiError::internal(anyhow::anyhow!("创建目录失败: {err}")));
            }
        }
        let old_lrc = sidecar_path(&source);
        let new_lrc = sidecar_path(&target);
        let had_lrc = old_lrc.exists();
        if let Err(err) = std::fs::rename(&source, &target) {
            rollback_moves(&moved);
            return Err(ApiError::internal(anyhow::anyhow!(
                "移动失败 {}: {err}",
                source.display()
            )));
        }
        let moved_lrc = if had_lrc {
            match std::fs::rename(&old_lrc, &new_lrc) {
                Ok(()) => Some(new_lrc.clone()),
                Err(err) => {
                    let _ = std::fs::rename(&target, &source);
                    rollback_moves(&moved);
                    return Err(ApiError::internal(anyhow::anyhow!("移动歌词失败: {err}")));
                }
            }
        } else {
            None
        };
        moved.push((item.id, source, target, Some(old_lrc), moved_lrc));
    }

    let mut db_errors = Vec::new();
    for (id, _, target, _, _) in &moved {
        let Some(track) = state.store.get(*id).map_err(ApiError::internal)? else {
            continue;
        };
        let root = Path::new(&track.root);
        let rel = target
            .strip_prefix(root)
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_default();
        let filename = target
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        if let Err(err) =
            state
                .store
                .move_path(*id, &target.to_string_lossy(), &track.root, &rel, &filename)
        {
            db_errors.push(format!("{id}: {err:#}"));
        }
    }
    Ok(Json(json!({
        "ok": db_errors.is_empty(),
        "moved": moved.len(),
        "items": items,
        "errors": db_errors,
    })))
}

fn rollback_moves(
    moved: &[(
        u64,
        std::path::PathBuf,
        std::path::PathBuf,
        Option<std::path::PathBuf>,
        Option<std::path::PathBuf>,
    )],
) {
    for (_, source, target, old_lrc, new_lrc) in moved.iter().rev() {
        if let (Some(old_lrc), Some(new_lrc)) = (old_lrc, new_lrc) {
            let _ = std::fs::rename(new_lrc, old_lrc);
        }
        let _ = std::fs::rename(target, source);
    }
}

async fn get_settings(State(state): State<AppState>) -> Json<Value> {
    let config = state.config();
    Json(json!({
        "config": config,
        "version": env!("CARGO_PKG_VERSION"),
        "data_dir": state.paths.data.to_string_lossy(),
        "auth_enabled": state.auth.enabled(),
    }))
}

async fn update_settings(
    State(state): State<AppState>,
    Json(mut config): Json<Config>,
) -> ApiResult<Json<Value>> {
    config.normalize();
    if let Err(err) = config.validate() {
        return Err(ApiError::bad(err.to_string()));
    }
    config
        .save(&state.paths.config)
        .map_err(ApiError::internal)?;
    if let Ok(mut current) = state.config.write() {
        *current = config.clone();
    }
    Ok(Json(json!({
        "ok": true,
        "config": config,
    })))
}

async fn get_providers(State(state): State<AppState>) -> Json<Value> {
    let config = state.config();
    let items = config
        .sources
        .iter()
        .enumerate()
        .map(|(index, source)| {
            let metadata = crate::config::adapter_metadata(&source.adapter);
            let lyrics = crate::config::adapter_lyrics(&source.adapter);
            let kind = if metadata && lyrics {
                "metadata+lyrics"
            } else if metadata {
                "metadata"
            } else {
                "lyrics"
            };
            json!({
                "id": source.id,
                "name": source.name,
                "adapter": source.adapter,
                "kind": kind,
                "enabled": source.enabled,
                "endpoint": source.base_url,
                "priority": index,
            })
        })
        .collect::<Vec<_>>();
    Json(Value::Array(items))
}

async fn naming_variables() -> Json<Value> {
    let config = Config::default();
    Json(json!({
        "variables": naming::variables().iter().map(|(name, desc)| json!({"name": name, "description": desc})).collect::<Vec<_>>(),
        "presets": config.naming_presets,
        "default": config.naming_template,
    }))
}

async fn download_cover(
    client: &reqwest::Client,
    url: &str,
    config: &Config,
) -> anyhow::Result<Vec<u8>> {
    let parsed = url::Url::parse(url)?;
    if parsed.scheme() != "https" {
        anyhow::bail!("封面地址必须是 HTTPS");
    }
    if !allowed_cover_host(parsed.host_str().unwrap_or("")) {
        anyhow::bail!("封面域名不在允许列表");
    }
    let response = client.get(url).send().await?;
    if !response.status().is_success() {
        anyhow::bail!("封面下载返回 HTTP {}", response.status());
    }
    if let Some(len) = response.content_length() {
        if len > config.max_cover_mb * 1024 * 1024 {
            anyhow::bail!("封面超过大小限制");
        }
    }
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    if !content_type.starts_with("image/") {
        anyhow::bail!("返回内容不是图片: {content_type}");
    }
    let bytes = response.bytes().await?;
    if bytes.len() > (config.max_cover_mb * 1024 * 1024) as usize {
        anyhow::bail!("封面超过大小限制");
    }
    if bytes.len() < 100 {
        anyhow::bail!("封面内容为空");
    }
    Ok(bytes.to_vec())
}

fn allowed_cover_host(host: &str) -> bool {
    let host = host.to_lowercase();
    [
        "coverartarchive.org",
        "archive.org",
        "mzstatic.com",
        "dzcdn.net",
        "music.126.net",
        "126.net",
    ]
    .iter()
    .any(|suffix| host == *suffix || host.ends_with(&format!(".{suffix}")))
}

fn invalidate_cover(paths: &Paths, id: u64) {
    let _ = std::fs::remove_file(paths.covers.join(format!("{id}.bin")));
    let _ = std::fs::remove_file(paths.covers.join(format!("{id}.mime")));
}

async fn static_file(uri: axum::http::Uri) -> Response {
    let path = uri.path();
    let (body, mime, cache): (&[u8], &str, &str) = match path {
        "/" | "/index.html" => (
            include_str!("../web/index.html").as_bytes(),
            "text/html; charset=utf-8",
            "no-cache",
        ),
        "/app.js" => (
            include_str!("../web/app.js").as_bytes(),
            "application/javascript; charset=utf-8",
            "no-cache",
        ),
        "/sw.js" => (
            include_str!("../web/sw.js").as_bytes(),
            "application/javascript; charset=utf-8",
            "no-cache",
        ),
        "/style.css" => (
            include_str!("../web/style.css").as_bytes(),
            "text/css; charset=utf-8",
            "no-cache",
        ),
        "/favicon.svg" => (
            include_str!("../web/favicon.svg").as_bytes(),
            "image/svg+xml",
            "public, max-age=86400",
        ),
        "/manifest.webmanifest" => (
            include_str!("../web/manifest.webmanifest").as_bytes(),
            "application/manifest+json",
            "public, max-age=86400",
        ),
        "/icon-192.png" => (
            include_bytes!("../web/icon-192.png"),
            "image/png",
            "public, max-age=86400",
        ),
        "/icon-512.png" => (
            include_bytes!("../web/icon-512.png"),
            "image/png",
            "public, max-age=86400",
        ),
        "/apple-touch-icon.png" => (
            include_bytes!("../web/apple-touch-icon.png"),
            "image/png",
            "public, max-age=86400",
        ),
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    let mut response = Response::new(Body::from(body));
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(mime));
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response
        .headers_mut()
        .insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    response.headers_mut().insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("same-origin"),
    );
    response
}
