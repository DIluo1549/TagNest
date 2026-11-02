use super::{cover_for_source, parse_year, Candidate, SearchQuery};
use anyhow::{Context, Result};
use serde_json::Value;

pub async fn search(
    client: &reqwest::Client,
    query: &SearchQuery,
    base_url: &str,
) -> Result<Vec<Candidate>> {
    let mut term = String::new();
    for part in [&query.artist, &query.title, &query.album] {
        if !part.is_empty() {
            term.push_str(part);
            term.push(' ');
        }
    }
    if term.trim().is_empty() {
        return Ok(Vec::new());
    }
    let response = client
        .get(format!("{base_url}/search"))
        .query(&[("q", term.trim()), ("limit", "15")])
        .send()
        .await
        .context("连接 Deezer 失败")?;
    if !response.status().is_success() {
        anyhow::bail!("Deezer 返回 HTTP {}", response.status());
    }
    let body: Value = response.json().await.context("解析 Deezer 响应失败")?;
    let Some(items) = body.get("data").and_then(|v| v.as_array()) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for item in items {
        let Some(title) = item.get("title").and_then(|v| v.as_str()) else {
            continue;
        };
        let artist = item
            .pointer("/artist/name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        let album = item
            .pointer("/album/title")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        let cover = item
            .pointer("/album/cover_xl")
            .and_then(|v| v.as_str())
            .or_else(|| item.pointer("/album/cover_big").and_then(|v| v.as_str()))
            .unwrap_or("");
        let source_id = item
            .get("id")
            .map(|v| {
                v.as_str()
                    .map(ToOwned::to_owned)
                    .unwrap_or_else(|| v.to_string())
            })
            .unwrap_or_default();
        let release_date = item
            .get("release_date")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let album_ref = item.get("album");
        let album_id = album_ref
            .and_then(|v| v.get("id"))
            .map(|v| {
                v.as_str()
                    .map(ToOwned::to_owned)
                    .unwrap_or_else(|| v.to_string())
            })
            .unwrap_or_default();
        let _ = album_id;
        out.push(Candidate {
            source: "deezer".into(),
            source_id,
            title: title.trim().to_string(),
            artist: artist.clone(),
            artists: if artist.is_empty() {
                Vec::new()
            } else {
                vec![artist.clone()]
            },
            albumartist: if artist.is_empty() {
                Vec::new()
            } else {
                vec![artist]
            },
            album,
            track: item
                .get("track_position")
                .and_then(|v| v.as_u64())
                .map(|v| v as u32),
            track_total: None,
            disc: item
                .get("disk_number")
                .and_then(|v| v.as_u64())
                .map(|v| v as u32),
            disc_total: None,
            year: parse_year(&release_date),
            date: release_date,
            genres: Vec::new(),
            label: String::new(),
            catalog_number: String::new(),
            isrc: item
                .get("isrc")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            bpm: None,
            duration_ms: item.get("duration").and_then(|v| v.as_u64()).unwrap_or(0) * 1000,
            cover_url: cover_for_source("deezer", cover),
            mb_track_id: String::new(),
            mb_album_id: String::new(),
            mb_artist_id: String::new(),
            provider_score: 0.7,
            match_score: 0.0,
        });
    }
    Ok(out)
}
