use super::{Candidate, LyricsCandidate, SearchQuery};
use anyhow::{Context, Result};
use serde_json::Value;

const REFERER: &str = "https://y.qq.com/";

pub async fn search_metadata(
    client: &reqwest::Client,
    query: &SearchQuery,
    base_url: &str,
) -> Result<Vec<Candidate>> {
    let songs = search_songs(client, base_url, query).await?;
    let mut out = Vec::new();
    for song in songs.iter().take(10) {
        let Some(songmid) = song.get("songmid").and_then(|v| v.as_str()) else {
            continue;
        };
        let Some(title) = song.get("songname").and_then(|v| v.as_str()) else {
            continue;
        };
        let artists = names_from(song.get("singer"));
        let display = artists.join(" / ");
        let album = song
            .get("albumname")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let album_mid = song
            .get("albummid")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let duration_ms = song
            .get("interval")
            .and_then(|v| v.as_u64())
            .unwrap_or(0)
            * 1000;
        let cover_url = if album_mid.is_empty() {
            String::new()
        } else {
            format!("https://y.qq.com/music/photo_new/T002R300x300M000{album_mid}.jpg")
        };
        out.push(Candidate {
            source: "qq".into(),
            source_id: songmid.to_string(),
            title: title.to_string(),
            artist: display,
            artists,
            album,
            duration_ms,
            cover_url,
            provider_score: 0.8,
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
        let Some(songmid) = song.get("songmid").and_then(|v| v.as_str()) else {
            continue;
        };
        let Some(title) = song.get("songname").and_then(|v| v.as_str()) else {
            continue;
        };
        let mut candidate = match fetch(client, songmid, base_url).await {
            Ok(value) => value,
            Err(err) => {
                tracing::debug!("QQ 音乐歌词获取失败 {songmid}: {err:#}");
                continue;
            }
        };
        if candidate.synced_lyrics.trim().is_empty() && candidate.plain_lyrics.trim().is_empty() {
            continue;
        }
        candidate.title = title.to_string();
        candidate.artist = names_from(song.get("singer")).join(" / ");
        candidate.album = song
            .get("albumname")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        candidate.duration_ms = song
            .get("interval")
            .and_then(|v| v.as_u64())
            .unwrap_or(0)
            * 1000;
        candidate.preview = preview(&candidate.synced_lyrics, &candidate.plain_lyrics);
        out.push(candidate);
    }
    Ok(out)
}

pub async fn fetch(
    client: &reqwest::Client,
    id: &str,
    base_url: &str,
) -> Result<LyricsCandidate> {
    let response = client
        .get(format!("{base_url}/lyric/fcgi-bin/fcg_query_lyric_new.fcg"))
        .query(&[
            ("songmid", id),
            ("format", "json"),
            ("nobase64", "1"),
        ])
        .header("Referer", REFERER)
        .send()
        .await
        .context("连接 QQ 音乐失败")?;
    if !response.status().is_success() {
        anyhow::bail!("QQ 音乐返回 HTTP {}", response.status());
    }
    let body: Value = response.json().await.context("解析 QQ 音乐歌词失败")?;
    if body.get("retcode").and_then(|v| v.as_i64()).unwrap_or(0) != 0 {
        anyhow::bail!("QQ 音乐歌词不可用");
    }
    let synced = body
        .get("lyric")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let translated = body
        .get("trans")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if synced.is_empty() && translated.is_empty() {
        anyhow::bail!("QQ 音乐没有歌词");
    }
    if is_pure_music(&synced) {
        anyhow::bail!("QQ 音乐标记为纯音乐");
    }
    Ok(LyricsCandidate {
        source: "qq".into(),
        source_id: id.to_string(),
        title: lrc_tag(&synced, "ti"),
        artist: lrc_tag(&synced, "ar"),
        album: lrc_tag(&synced, "al"),
        duration_ms: 0,
        instrumental: false,
        has_synced: synced.contains("[0"),
        has_translation: !translated.is_empty(),
        plain_lyrics: if synced.is_empty() {
            strip_timestamps(&translated)
        } else {
            String::new()
        },
        synced_lyrics: synced,
        translated_lyrics: translated,
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
    for part in [&query.artist, &query.title, &query.album] {
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
        .get(format!("{base_url}/soso/fcgi-bin/client_search_cp"))
        .query(&[
            ("w", term.as_str()),
            ("format", "json"),
            ("n", "15"),
            ("p", "1"),
        ])
        .header("Referer", REFERER)
        .send()
        .await
        .context("连接 QQ 音乐失败")?;
    if !response.status().is_success() {
        anyhow::bail!("QQ 音乐搜索返回 HTTP {}", response.status());
    }
    let body: Value = response.json().await.context("解析 QQ 音乐搜索失败")?;
    Ok(body
        .pointer("/data/song/list")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default())
}

fn names_from(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("name").and_then(|v| v.as_str()))
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default()
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
    plain.len() < 80
        && (plain.contains("纯音乐") || plain.contains("請欣賞") || plain.contains("请欣赏"))
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

