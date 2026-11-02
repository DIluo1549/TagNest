use super::{
    artist_credit_display, artists_from_credit, cover_for_source, json_text, json_u32, parse_year,
    Candidate, SearchQuery,
};
use crate::util::similarity;
use anyhow::{Context, Result};
use serde_json::Value;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

pub struct MusicBrainz {
    last_request: Arc<Mutex<Instant>>,
}

impl MusicBrainz {
    pub fn new() -> Self {
        Self {
            last_request: Arc::new(Mutex::new(Instant::now() - Duration::from_secs(2))),
        }
    }

    async fn throttle(&self) {
        let mut last = self.last_request.lock().await;
        let elapsed = last.elapsed();
        if elapsed < Duration::from_millis(1100) {
            tokio::time::sleep(Duration::from_millis(1100) - elapsed).await;
        }
        *last = Instant::now();
    }

    pub async fn search(
        &self,
        client: &reqwest::Client,
        query: &SearchQuery,
        base_url: &str,
        coverart_base: &str,
    ) -> Result<Vec<Candidate>> {
        let mut terms = Vec::new();
        if !query.title.is_empty() {
            terms.push(format!("recording:\"{}\"", escape(&query.title)));
        }
        if !query.artist.is_empty() {
            terms.push(format!("artist:\"{}\"", escape(&query.artist)));
        }
        if !query.album.is_empty() {
            terms.push(format!("release:\"{}\"", escape(&query.album)));
        }
        if terms.is_empty() {
            return Ok(Vec::new());
        }
        let mut response = None;
        for attempt in 0..3 {
            self.throttle().await;
            let current = client
                .get(format!("{base_url}/ws/2/recording"))
                .query(&[
                    ("query", terms.join(" AND ")),
                    ("fmt", "json".to_string()),
                    ("limit", "12".to_string()),
                ])
                .send()
                .await
                .context("连接 MusicBrainz 失败")?;
            if current.status().is_success() {
                response = Some(current);
                break;
            }
            if attempt < 2 && (current.status().as_u16() == 503 || current.status().as_u16() == 429)
            {
                tokio::time::sleep(Duration::from_millis(1500 * (attempt + 1) as u64)).await;
                continue;
            }
            anyhow::bail!("MusicBrainz 返回 HTTP {}", current.status());
        }
        let response = response.context("MusicBrainz 没有返回响应")?;
        let body: Value = response.json().await.context("解析 MusicBrainz 响应失败")?;
        let Some(recordings) = body.get("recordings").and_then(|v| v.as_array()) else {
            return Ok(Vec::new());
        };

        let mut out = Vec::new();
        for recording in recordings {
            let Some(title) = recording.get("title").and_then(|v| v.as_str()) else {
                continue;
            };
            let artist = artist_credit_display(recording.get("artist-credit"));
            let artists = artists_from_credit(recording.get("artist-credit"));
            let releases = recording
                .get("releases")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            let release = choose_release(&releases, &query.album);
            let (album, albumartist, date, track, track_total, disc, disc_total, release_id) =
                release
                    .map(|release| parse_release(release))
                    .unwrap_or_default();
            let mut genres = Vec::new();
            for key in ["genres", "tags"] {
                for item in recording
                    .get(key)
                    .and_then(|v| v.as_array())
                    .into_iter()
                    .flatten()
                {
                    if let Some(name) = item.get("name").and_then(|v| v.as_str()) {
                        if !genres.iter().any(|g: &String| g == name) {
                            genres.push(name.to_string());
                        }
                    }
                }
            }
            let isrc = recording
                .get("isrcs")
                .and_then(|v| v.as_array())
                .and_then(|v| v.first())
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let artist_id = recording
                .pointer("/artist-credit/0/artist/id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let duration_ms = recording
                .get("length")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let cover = if release_id.is_empty() {
                String::new()
            } else {
                cover_for_source(
                    "musicbrainz",
                    &format!("{coverart_base}/release/{release_id}/front-500"),
                )
            };
            out.push(Candidate {
                source: "musicbrainz".into(),
                source_id: recording
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                title: title.to_string(),
                artist,
                artists,
                albumartist,
                album,
                track,
                track_total,
                disc,
                disc_total,
                year: parse_year(&date),
                date,
                genres,
                label: String::new(),
                catalog_number: String::new(),
                isrc,
                bpm: None,
                duration_ms,
                cover_url: cover,
                mb_track_id: recording
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                mb_album_id: release_id,
                mb_artist_id: artist_id,
                provider_score: recording
                    .get("score")
                    .and_then(|v| v.as_u64())
                    .map(|v| v as f64 / 100.0)
                    .unwrap_or(0.5),
                match_score: 0.0,
            });
        }
        Ok(out)
    }
}

fn choose_release<'a>(releases: &'a [Value], album: &str) -> Option<&'a Value> {
    releases.iter().max_by(|a, b| {
        release_score(a, album)
            .partial_cmp(&release_score(b, album))
            .unwrap_or(std::cmp::Ordering::Equal)
    })
}

fn release_score(release: &Value, album: &str) -> f64 {
    let title = release.get("title").and_then(|v| v.as_str()).unwrap_or("");
    let mut score = if album.is_empty() {
        0.5
    } else {
        similarity(album, title)
    };
    if release.get("status").and_then(|v| v.as_str()) == Some("Official") {
        score += 0.12;
    }
    let primary = release
        .pointer("/release-group/primary-type")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if primary == "Album" {
        score += 0.08;
    }
    if release.get("date").and_then(|v| v.as_str()).is_some() {
        score += 0.03;
    }
    if release
        .pointer("/release-group/secondary-types")
        .and_then(|v| v.as_array())
        .map(|v| v.iter().any(|x| x.as_str() == Some("Compilation")))
        .unwrap_or(false)
        && album.to_lowercase().contains("compilation")
    {
        score += 0.1;
    }
    score
}

type ReleaseInfo = (
    String,
    Vec<String>,
    String,
    Option<u32>,
    Option<u32>,
    Option<u32>,
    Option<u32>,
    String,
);

fn parse_release(release: &Value) -> ReleaseInfo {
    let album = release
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let albumartist = artists_from_credit(release.get("artist-credit"));
    let date = release
        .get("date")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let media = release
        .get("media")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let mut track = None;
    let mut track_total = None;
    let mut disc = None;
    let mut disc_total = None;
    for (index, medium) in media.iter().enumerate() {
        if let Some(tracks) = medium.get("track").and_then(|v| v.as_array()) {
            if let Some(first) = tracks.first() {
                track = first
                    .get("number")
                    .and_then(|v| v.as_str())
                    .and_then(|v| v.parse().ok())
                    .or_else(|| json_u32(first, "/number"));
            }
            if track.is_some() {
                disc = Some((index + 1) as u32);
                track_total = medium
                    .get("track-count")
                    .and_then(|v| v.as_u64())
                    .map(|v| v as u32);
                disc_total = Some(media.len() as u32);
                break;
            }
        }
    }
    let release_id = release
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    (
        album,
        albumartist,
        date,
        track,
        track_total,
        disc,
        disc_total,
        release_id,
    )
}

fn escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

#[allow(dead_code)]
fn text(value: &Value, key: &str) -> String {
    json_text(value, &format!("/{key}"))
}
