use super::{LyricsCandidate, SearchQuery};
use anyhow::{Context, Result};
use serde_json::Value;

pub async fn search(
    client: &reqwest::Client,
    query: &SearchQuery,
    base_url: &str,
) -> Result<Vec<LyricsCandidate>> {
    if query.title.is_empty() {
        return Ok(Vec::new());
    }
    let duration = (query.duration_ms / 1000).to_string();
    let exact = send_with_retry(
        client,
        &format!("{base_url}/api/get"),
        &[
            ("artist_name", query.artist.as_str()),
            ("track_name", query.title.as_str()),
            ("album_name", query.album.as_str()),
            ("duration", duration.as_str()),
        ],
    )
    .await?;
    if exact.status().is_success() {
        let body: Value = exact.json().await.context("解析 LRCLIB 响应失败")?;
        return Ok(vec![parse_item(&body)]);
    }
    let exact = send_with_retry(
        client,
        &format!("{base_url}/api/search"),
        &[
            ("track_name", query.title.as_str()),
            ("artist_name", query.artist.as_str()),
            ("album_name", query.album.as_str()),
        ],
    )
    .await?;
    if !exact.status().is_success() {
        anyhow::bail!("LRCLIB 返回 HTTP {}", exact.status());
    }
    let body: Value = exact.json().await.context("解析 LRCLIB 搜索响应失败")?;
    let Some(items) = body.as_array() else {
        return Ok(Vec::new());
    };
    Ok(items.iter().take(12).map(parse_item).collect())
}

pub async fn fetch(client: &reqwest::Client, id: &str, base_url: &str) -> Result<LyricsCandidate> {
    let response = send_with_retry(client, &format!("{base_url}/api/get/{id}"), &[]).await?;
    if !response.status().is_success() {
        anyhow::bail!("LRCLIB 没有这条歌词 (HTTP {})", response.status());
    }
    let body: Value = response.json().await.context("解析 LRCLIB 歌词失败")?;
    Ok(parse_item(&body))
}

async fn send_with_retry(
    client: &reqwest::Client,
    url: &str,
    params: &[(&str, &str)],
) -> Result<reqwest::Response> {
    let mut last_status = None;
    for attempt in 0..2 {
        let response = client
            .get(url)
            .query(params)
            .send()
            .await
            .context("连接 LRCLIB 失败")?;
        if response.status().is_success()
            || (!response.status().is_server_error() && response.status().as_u16() != 429)
        {
            return Ok(response);
        }
        last_status = Some(response.status());
        if attempt == 0 {
            tokio::time::sleep(std::time::Duration::from_millis(700)).await;
        }
    }
    anyhow::bail!(
        "LRCLIB 返回 HTTP {}",
        last_status
            .map(|s| s.to_string())
            .unwrap_or_else(|| "unknown".into())
    )
}

fn strip_timestamp(line: &str) -> String {
    let mut rest = line.trim_start();
    while rest.starts_with('[') {
        if let Some(end) = rest.find(']') {
            rest = rest[end + 1..].trim_start();
        } else {
            break;
        }
    }
    rest.trim().to_string()
}

fn parse_item(item: &Value) -> LyricsCandidate {
    let synced = item
        .get("syncedLyrics")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let plain = item
        .get("plainLyrics")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let preview_source = if synced.is_empty() { &plain } else { &synced };
    let preview = preview_source
        .lines()
        .map(strip_timestamp)
        .filter(|line| !line.is_empty())
        .take(4)
        .collect::<Vec<_>>()
        .join("\n");
    LyricsCandidate {
        source: "lrclib".into(),
        source_id: item
            .get("id")
            .map(|v| {
                v.as_str()
                    .map(ToOwned::to_owned)
                    .unwrap_or_else(|| v.to_string())
            })
            .unwrap_or_default(),
        title: item
            .get("trackName")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        artist: item
            .get("artistName")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        album: item
            .get("albumName")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        duration_ms: item
            .get("duration")
            .and_then(|v| v.as_f64())
            .map(|v| (v * 1000.0) as u64)
            .unwrap_or(0),
        instrumental: item
            .get("instrumental")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        has_synced: !synced.is_empty(),
        has_translation: false,
        plain_lyrics: plain,
        synced_lyrics: synced,
        translated_lyrics: String::new(),
        preview,
        match_score: 0.0,
    }
}
