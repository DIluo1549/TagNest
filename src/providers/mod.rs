pub mod deezer;
pub mod itunes;
pub mod lrclib;
pub mod musicbrainz;
pub mod netease;

use crate::config::Config;
use crate::models::Track;
use crate::util::similarity;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Clone, Debug)]
pub struct SearchQuery {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration_ms: u64,
    pub isrc: String,
}

impl From<&Track> for SearchQuery {
    fn from(track: &Track) -> Self {
        Self {
            title: track.title.clone(),
            artist: track.primary_artist().to_string(),
            album: track.album.clone(),
            duration_ms: track.duration_ms,
            isrc: track.isrc.clone(),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Candidate {
    pub source: String,
    pub source_id: String,
    pub title: String,
    /// Single display credit, e.g. "Artist A & Artist B".
    pub artist: String,
    pub artists: Vec<String>,
    pub albumartist: Vec<String>,
    pub album: String,
    pub track: Option<u32>,
    pub track_total: Option<u32>,
    pub disc: Option<u32>,
    pub disc_total: Option<u32>,
    pub date: String,
    pub year: Option<u32>,
    pub genres: Vec<String>,
    pub label: String,
    pub catalog_number: String,
    pub isrc: String,
    pub bpm: Option<u32>,
    pub duration_ms: u64,
    pub cover_url: String,
    pub mb_track_id: String,
    pub mb_album_id: String,
    pub mb_artist_id: String,
    pub provider_score: f64,
    pub match_score: f64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct LyricsCandidate {
    pub source: String,
    pub source_id: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration_ms: u64,
    pub instrumental: bool,
    pub has_synced: bool,
    pub has_translation: bool,
    pub plain_lyrics: String,
    pub synced_lyrics: String,
    pub translated_lyrics: String,
    pub preview: String,
    pub match_score: f64,
}

pub struct Providers {
    pub client: reqwest::Client,
    pub musicbrainz: musicbrainz::MusicBrainz,
}

impl Providers {
    pub fn new() -> Result<Self> {
        let client = reqwest::Client::builder()
            .user_agent(format!(
                "TagNest/{} (NAS music tagger)",
                env!("CARGO_PKG_VERSION")
            ))
            .timeout(Duration::from_secs(18))
            .connect_timeout(Duration::from_secs(8))
            .gzip(true)
            .build()
            .context("创建 HTTP 客户端失败")?;
        Ok(Self {
            client,
            musicbrainz: musicbrainz::MusicBrainz::new(),
        })
    }

    pub async fn search_tracks(
        &self,
        config: &Config,
        query: &SearchQuery,
        sources: Option<&[String]>,
    ) -> Vec<Candidate> {
        let wanted = |name: &str| sources.map(|s| s.iter().any(|v| v == name)).unwrap_or(true);
        let mut jobs: Vec<futures::future::BoxFuture<'_, (&'static str, Result<Vec<Candidate>>)>> =
            Vec::new();
        if config.providers.musicbrainz && wanted("musicbrainz") {
            jobs.push(Box::pin(async {
                let result = tokio::time::timeout(
                    Duration::from_secs(13),
                    self.musicbrainz.search(
                        &self.client,
                        query,
                        &config.endpoints.musicbrainz,
                        &config.endpoints.coverart,
                    ),
                )
                .await
                .unwrap_or_else(|_| Err(anyhow::anyhow!("请求超时")));
                ("musicbrainz", result)
            }));
        }
        if config.providers.itunes && wanted("itunes") {
            jobs.push(Box::pin(async {
                let result = tokio::time::timeout(
                    Duration::from_secs(11),
                    itunes::search(
                        &self.client,
                        query,
                        &config.itunes_country,
                        &config.endpoints.itunes,
                    ),
                )
                .await
                .unwrap_or_else(|_| Err(anyhow::anyhow!("请求超时")));
                ("itunes", result)
            }));
        }
        if config.providers.deezer && wanted("deezer") {
            jobs.push(Box::pin(async {
                let result = tokio::time::timeout(
                    Duration::from_secs(6),
                    deezer::search(&self.client, query, &config.endpoints.deezer),
                )
                .await
                .unwrap_or_else(|_| Err(anyhow::anyhow!("请求超时")));
                ("deezer", result)
            }));
        }
        if config.providers.netease && wanted("netease") {
            jobs.push(Box::pin(async {
                let result = tokio::time::timeout(
                    Duration::from_secs(12),
                    netease::search_metadata(&self.client, query, &config.endpoints.netease),
                )
                .await
                .unwrap_or_else(|_| Err(anyhow::anyhow!("请求超时")));
                ("netease", result)
            }));
        }
        let results = futures::future::join_all(jobs).await;
        let mut candidates = Vec::new();
        for (source, result) in results {
            match result {
                Ok(mut items) => candidates.append(&mut items),
                Err(err) => tracing::warn!("{source} 搜索失败: {err:#}"),
            }
        }
        rank(query, &mut candidates);
        candidates
    }

    pub async fn search_lyrics(
        &self,
        config: &Config,
        track: &Track,
        sources: Option<&[String]>,
    ) -> Vec<LyricsCandidate> {
        let wanted = |name: &str| sources.map(|s| s.iter().any(|v| v == name)).unwrap_or(true);
        let query = SearchQuery::from(track);
        let mut jobs: Vec<
            futures::future::BoxFuture<'_, (&'static str, Result<Vec<LyricsCandidate>>)>,
        > = Vec::new();
        if config.providers.lrclib && wanted("lrclib") {
            jobs.push(Box::pin(async {
                let result = tokio::time::timeout(
                    Duration::from_secs(10),
                    lrclib::search(&self.client, &query, &config.endpoints.lrclib),
                )
                .await
                .unwrap_or_else(|_| Err(anyhow::anyhow!("请求超时")));
                ("lrclib", result)
            }));
        }
        if config.providers.netease && wanted("netease") {
            jobs.push(Box::pin(async {
                let result = tokio::time::timeout(
                    Duration::from_secs(14),
                    netease::search(&self.client, &query, &config.endpoints.netease),
                )
                .await
                .unwrap_or_else(|_| Err(anyhow::anyhow!("请求超时")));
                ("netease", result)
            }));
        }
        let results = futures::future::join_all(jobs).await;
        let mut candidates = Vec::new();
        for (source, result) in results {
            match result {
                Ok(mut items) => candidates.append(&mut items),
                Err(err) => tracing::warn!("{source} 歌词搜索失败: {err:#}"),
            }
        }
        for item in &mut candidates {
            item.match_score = lyrics_score(track, item);
        }
        candidates.sort_by(|a, b| {
            b.has_synced
                .cmp(&a.has_synced)
                .then_with(|| b.has_translation.cmp(&a.has_translation))
                .then_with(|| {
                    b.match_score
                        .partial_cmp(&a.match_score)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
        });
        candidates
    }

    pub async fn fetch_lyrics(
        &self,
        config: &Config,
        source: &str,
        id: &str,
    ) -> Result<LyricsCandidate> {
        match source {
            "lrclib" => lrclib::fetch(&self.client, id, &config.endpoints.lrclib).await,
            "netease" => netease::fetch(&self.client, id, &config.endpoints.netease).await,
            other => anyhow::bail!("未知歌词源: {other}"),
        }
    }
}

pub fn rank(query: &SearchQuery, candidates: &mut [Candidate]) {
    for candidate in candidates.iter_mut() {
        candidate.match_score = score_candidate(query, candidate);
    }
    candidates.sort_by(|a, b| {
        b.match_score
            .partial_cmp(&a.match_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
}

pub fn score_candidate(query: &SearchQuery, candidate: &Candidate) -> f64 {
    let title = similarity(&query.title, &candidate.title);
    let artist = if query.artist.is_empty()
        || (candidate.artists.is_empty() && candidate.artist.is_empty())
    {
        0.5
    } else if candidate.artists.is_empty() {
        similarity(&query.artist, &candidate.artist)
    } else {
        candidate
            .artists
            .iter()
            .map(|a| similarity(&query.artist, a))
            .fold(0.0f64, f64::max)
    };
    let album = if query.album.is_empty() || candidate.album.is_empty() {
        0.5
    } else {
        similarity(&query.album, &candidate.album)
    };
    let duration = if query.duration_ms == 0 || candidate.duration_ms == 0 {
        0.5
    } else {
        let diff = query.duration_ms.abs_diff(candidate.duration_ms);
        (1.0 - diff as f64 / 20_000.0).clamp(0.0, 1.0)
    };
    let mut score = title * 0.54 + artist * 0.27 + album * 0.12 + duration * 0.07;
    if !query.isrc.is_empty() && !candidate.isrc.is_empty() {
        if query.isrc.eq_ignore_ascii_case(&candidate.isrc) {
            score = score.max(0.98);
        }
    }
    (score * 100.0).round() / 100.0
}

pub fn lyrics_score(track: &Track, candidate: &LyricsCandidate) -> f64 {
    let title = similarity(&track.title, &candidate.title);
    let artist = if track.primary_artist().is_empty() || candidate.artist.is_empty() {
        0.5
    } else {
        similarity(track.primary_artist(), &candidate.artist)
    };
    let mut score = title * 0.7 + artist * 0.3;
    if track.duration_ms > 0 && candidate.duration_ms > 0 {
        let diff = track.duration_ms.abs_diff(candidate.duration_ms);
        if diff < 3_000 {
            score += 0.08;
        } else if diff > 15_000 {
            score -= 0.12;
        }
    }
    (score.clamp(0.0, 1.0) * 100.0).round() / 100.0
}

pub fn parse_year(date: &str) -> Option<u32> {
    date.get(0..4).and_then(|v| v.parse().ok())
}

/// Build the display credit from MusicBrainz's artist-credit array, including
/// join phrases ("A & B", "A feat. B").
pub fn artist_credit_display(value: Option<&serde_json::Value>) -> String {
    let Some(items) = value.and_then(|v| v.as_array()) else {
        return String::new();
    };
    let mut out = String::new();
    for item in items {
        if let Some(name) = item
            .get("name")
            .and_then(|v| v.as_str())
            .or_else(|| item.pointer("/artist/name").and_then(|v| v.as_str()))
        {
            if !out.is_empty() {
                if let Some(join) = item.get("joinphrase").and_then(|v| v.as_str()) {
                    out.push_str(join);
                } else {
                    out.push_str(" / ");
                }
            }
            out.push_str(name.trim());
        }
    }
    out.trim().to_string()
}

pub fn artists_from_credit(value: Option<&serde_json::Value>) -> Vec<String> {
    let Some(items) = value.and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for item in items {
        if let Some(name) = item
            .get("name")
            .and_then(|v| v.as_str())
            .or_else(|| item.pointer("/artist/name").and_then(|v| v.as_str()))
        {
            let name = name.trim();
            if !name.is_empty() && !out.iter().any(|v: &String| v == name) {
                out.push(name.to_string());
            }
        }
    }
    out
}

pub fn json_text(value: &serde_json::Value, pointer: &str) -> String {
    value
        .pointer(pointer)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string()
}

pub fn json_u32(value: &serde_json::Value, pointer: &str) -> Option<u32> {
    value
        .pointer(pointer)
        .and_then(|v| v.as_u64())
        .and_then(|v| u32::try_from(v).ok())
}

pub fn cover_for_source(_source: &str, url: &str) -> String {
    if url.is_empty() {
        return String::new();
    }
    match _source {
        "itunes" => url.replace("100x100bb", "1200x1200bb"),
        _ => url.to_string(),
    }
}
