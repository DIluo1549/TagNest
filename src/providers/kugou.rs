use super::{Candidate, LyricsCandidate, SearchQuery};
use crate::util::similarity;
use anyhow::{Context, Result};
use serde_json::Value;

const LYRIC_BASE: &str = "https://m.kugou.com";

pub async fn search_metadata(
    client: &reqwest::Client,
    query: &SearchQuery,
    base_url: &str,
) -> Result<Vec<Candidate>> {
    let songs = search_songs(client, base_url, query).await?;
    let mut out = Vec::new();
    for song in songs.iter().take(10) {
        let Some(hash) = song.get("hash").and_then(|v| v.as_str()) else {
            continue;
        };
        let Some(title) = song.get("songname").and_then(|v| v.as_str()) else {
            continue;
        };
        let artists = split_names(song.get("singername").and_then(|v| v.as_str()).unwrap_or(""));
        let display = artists.join(" / ");
        let album = song
            .get("album_name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let album_id = song
            .get("album_id")
            .map(json_string)
            .unwrap_or_default();
        let duration_ms = song
            .get("duration")
            .and_then(|v| v.as_u64())
            .unwrap_or(0)
            * 1000;
        let cover_url = if album_id.is_empty() {
            String::new()
        } else {
            format!("https://imge.kugou.com/stdmusic/240/{album_id}.jpg")
        };
        out.push(Candidate {
            source: "kugou".into(),
            source_id: hash.to_string(),
            title: clean_text(title),
            artist: display,
            artists,
            album: clean_text(&album),
            duration_ms,
            cover_url,
            provider_score: 0.78,
            ..Default::default()
        });
    }
    Ok(out)
}

pub async fn search(
    client: &reqwest::Client,
    query: &SearchQuery,
    base_url: &str,
) -> Result<Vec<LyricsCandidate>> {
    let songs = search_songs(client, base_url, query).await?;
    let mut out = Vec::new();
    for song in songs.iter().take(10) {
        let Some(hash) = song.get("hash").and_then(|v| v.as_str()) else {
            continue;
        };
        let Some(title) = song.get("songname").and_then(|v| v.as_str()) else {
            continue;
        };
        let duration_ms = song
            .get("duration")
            .and_then(|v| v.as_u64())
            .unwrap_or(0)
            * 1000;
        let source_id = format!("{hash}|{duration_ms}");
        let mut candidate = match fetch(client, &source_id, base_url).await {
            Ok(value) => value,
            Err(err) => {
                tracing::debug!("酷狗歌词获取失败 {hash}: {err:#}");
                continue;
            }
        };
        if candidate.synced_lyrics.trim().is_empty() && candidate.plain_lyrics.trim().is_empty() {
            continue;
        }
        let artist = split_names(
            song.get("singername")
                .and_then(|v| v.as_str())
                .unwrap_or(""),
        )
        .join(" / ");
        let expected_title = clean_text(title);
        if !candidate.title.is_empty()
            && similarity(&candidate.title, &expected_title) < 0.18
        {
            // Some Kugou hashes point at an unrelated lyric file.
            continue;
        }
        candidate.source_id = source_id;
        candidate.title = expected_title;
        candidate.artist = artist;
        candidate.album = clean_text(
            song.get("album_name")
                .and_then(|v| v.as_str())
                .unwrap_or(""),
        );
        candidate.duration_ms = duration_ms;
        candidate.preview = preview(&candidate.synced_lyrics, &candidate.plain_lyrics);
        out.push(candidate);
    }
    Ok(out)
}

pub async fn fetch(
    client: &reqwest::Client,
    id: &str,
    _base_url: &str,
) -> Result<LyricsCandidate> {
    let (hash, duration_ms) = id.split_once('|').unwrap_or((id, "0"));
    let duration_ms: u64 = duration_ms.parse().unwrap_or(0);
    let response = client
        .get(format!("{LYRIC_BASE}/app/i/krc.php"))
        .query(&[
            ("cmd", "100".to_string()),
            ("hash", hash.to_string()),
            ("timelength", duration_ms.to_string()),
        ])
        .header("User-Agent", "Mozilla/5.0 (TagNest)")
        .send()
        .await
        .context("连接酷狗歌词失败")?;
    if !response.status().is_success() {
        anyhow::bail!("酷狗歌词返回 HTTP {}", response.status());
    }
    let synced = response
        .text()
        .await
        .context("读取酷狗歌词失败")?
        .trim_start_matches('\u{feff}')
        .trim()
        .to_string();
    if synced.is_empty() || is_pure_music(&synced) {
        anyhow::bail!("酷狗没有歌词");
    }
    Ok(LyricsCandidate {
        source: "kugou".into(),
        source_id: id.to_string(),
        title: lrc_tag(&synced, "ti"),
        artist: lrc_tag(&synced, "ar"),
        album: lrc_tag(&synced, "al"),
        duration_ms,
        instrumental: false,
        has_synced: synced.contains("[0"),
        has_translation: false,
        plain_lyrics: if synced.contains("[0") {
            String::new()
        } else {
            strip_timestamps(&synced)
        },
        synced_lyrics: synced,
        translated_lyrics: String::new(),
        preview: String::new(),
        match_score: 0.0,
    })
}

async fn search_songs(
    client: &reqwest::Client,
    base_url: &str,
    query: &SearchQuery,
) -> Result<Vec<Value>> {
    let mut term = String::new();
    for part in [&query.artist, &query.title] {
        if !part.trim().is_empty() {
            if !term.is_empty() {
                term.push(' ');
            }
            term.push_str(part.trim());
        }
    }
    if term.is_empty() {
        return Ok(Vec::new());
    }
    let response = client
        .get(format!("{base_url}/api/v3/search/song"))
        .query(&[
            ("format", "json"),
            ("keyword", term.as_str()),
            ("page", "1"),
            ("pagesize", "15"),
        ])
        .header("User-Agent", "Mozilla/5.0 (TagNest)")
        .send()
        .await
        .context("连接酷狗音乐失败")?;
    if !response.status().is_success() {
        anyhow::bail!("酷狗音乐返回 HTTP {}", response.status());
    }
    let body: Value = response.json().await.context("解析酷狗音乐搜索失败")?;
    Ok(body
        .pointer("/data/info")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default())
}

fn json_string(value: &Value) -> String {
    value
        .as_str()
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| value.to_string())
}

fn split_names(value: &str) -> Vec<String> {
    value
        .split(|ch| matches!(ch, '/' | '、' | ';' | '；'))
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn clean_text(value: &str) -> String {
    value
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .trim()
        .to_string()
}

fn lrc_tag(text: &str, key: &str) -> String {
    let prefix = format!("[{key}:");
    text.lines()
        .find_map(|line| line.strip_prefix(&prefix))
        .and_then(|line| line.strip_suffix(']'))
        .unwrap_or("")
        .trim()
        .to_string()
}

fn is_pure_music(text: &str) -> bool {
    let plain = strip_timestamps(text);
    plain.len() < 80 && (plain.contains("纯音乐") || plain.contains("純音樂"))
}

fn strip_timestamps(text: &str) -> String {
    text.lines()
        .map(|line| {
            let mut value = line;
            while value.starts_with('[') {
                if let Some(end) = value.find(']') {
                    value = &value[end + 1..];
                } else {
                    break;
                }
            }
            value.trim().to_string()
        })
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn preview(synced: &str, plain: &str) -> String {
    let source = if synced.is_empty() { plain } else { synced };
    source.lines().take(4).collect::<Vec<_>>().join("\n")
}
