use super::{cover_for_source, parse_year, Candidate, SearchQuery};
use anyhow::{Context, Result};
use serde_json::Value;

pub async fn search(
    client: &reqwest::Client,
    query: &SearchQuery,
    country: &str,
    base_url: &str,
) -> Result<Vec<Candidate>> {
    let mut term = String::new();
    if !query.artist.is_empty() {
        term.push_str(&query.artist);
        term.push(' ');
    }
    if !query.title.is_empty() {
        term.push_str(&query.title);
        term.push(' ');
    }
    if !query.album.is_empty() {
        term.push_str(&query.album);
    }
    if term.trim().is_empty() {
        return Ok(Vec::new());
    }
    // Apple's catalog differs per storefront. A track that is absent in the
    // configured store (common for Chinese releases in `cn`) can still be
    // present in Hong Kong/Taiwan or the US store.
    let mut countries = vec![country.to_string()];
    for fallback in ["hk", "tw", "us"] {
        if !countries.iter().any(|c| c == fallback) {
            countries.push(fallback.to_string());
        }
    }
    for store in countries {
        let items = query_country(client, term.trim(), &store, base_url).await?;
        if !items.is_empty() {
            return Ok(items);
        }
    }
    Ok(Vec::new())
}

async fn query_country(
    client: &reqwest::Client,
    term: &str,
    country: &str,
    base_url: &str,
) -> Result<Vec<Candidate>> {
    let response = client
        .get(format!("{base_url}/search"))
        .query(&[
            ("term", term),
            ("entity", "song"),
            ("limit", "15"),
            ("country", country),
        ])
        .send()
        .await
        .context("连接 iTunes 失败")?;
    if !response.status().is_success() {
        anyhow::bail!("iTunes 返回 HTTP {}", response.status());
    }
    let body: Value = response.json().await.context("解析 iTunes 响应失败")?;
    let Some(results) = body.get("results").and_then(|v| v.as_array()) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for item in results {
        let Some(title) = item.get("trackName").and_then(|v| v.as_str()) else {
            continue;
        };
        if title.trim().is_empty() {
            continue;
        }
        let artist = item
            .get("artistName")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        let album = item
            .get("collectionName")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        let albumartist = item
            .get("collectionArtistName")
            .and_then(|v| v.as_str())
            .or_else(|| item.get("artistName").and_then(|v| v.as_str()))
            .unwrap_or("")
            .trim()
            .to_string();
        let release_date = item
            .get("releaseDate")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let cover = item
            .get("artworkUrl100")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let source_id = item
            .get("trackId")
            .map(|v| {
                v.as_str()
                    .map(ToOwned::to_owned)
                    .unwrap_or_else(|| v.to_string())
            })
            .unwrap_or_default();
        let mut genres = Vec::new();
        if let Some(genre) = item.get("primaryGenreName").and_then(|v| v.as_str()) {
            if !genre.trim().is_empty() {
                genres.push(genre.trim().to_string());
            }
        }
        out.push(Candidate {
            source: "itunes".into(),
            source_id,
            title: title.trim().to_string(),
            artist: artist.clone(),
            artists: if artist.is_empty() {
                Vec::new()
            } else {
                vec![artist]
            },
            albumartist: if albumartist.is_empty() {
                Vec::new()
            } else {
                vec![albumartist]
            },
            album,
            track: item
                .get("trackNumber")
                .and_then(|v| v.as_u64())
                .map(|v| v as u32),
            track_total: item
                .get("trackCount")
                .and_then(|v| v.as_u64())
                .map(|v| v as u32),
            disc: item
                .get("discNumber")
                .and_then(|v| v.as_u64())
                .map(|v| v as u32),
            disc_total: item
                .get("discCount")
                .and_then(|v| v.as_u64())
                .map(|v| v as u32),
            year: parse_year(&release_date),
            date: release_date,
            genres,
            label: String::new(),
            catalog_number: String::new(),
            isrc: item
                .get("isrc")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            bpm: item.get("bpm").and_then(|v| v.as_u64()).map(|v| v as u32),
            duration_ms: item
                .get("trackTimeMillis")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
            cover_url: cover_for_source("itunes", cover),
            mb_track_id: String::new(),
            mb_album_id: String::new(),
            mb_artist_id: String::new(),
            provider_score: 0.75,
            match_score: 0.0,
        });
    }
    Ok(out)
}
