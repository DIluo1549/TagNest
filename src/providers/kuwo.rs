use super::{Candidate, LyricsCandidate, SearchQuery};
use anyhow::{Context, Result};
use serde_json::Value;

const DETAIL_BASE: &str = "http://m.kuwo.cn";

pub async fn search_metadata(
    client: &reqwest::Client,
    query: &SearchQuery,
    base_url: &str,
) -> Result<Vec<Candidate>> {
    let songs = search_songs(client, base_url, query).await?;
    let mut out = Vec::new();
    for song in songs.iter().take(6) {
        let Some(id) = song_id(song) else {
            continue;
        };
        let Some(title) = song.get("SONGNAME").and_then(|v| v.as_str()) else {
            continue;
        };
        let artists = split_names(
            &html_unescape(song.get("ARTIST").and_then(|v| v.as_str()).unwrap_or("")),
        );
        let mut album = html_unescape(
            song.get("ALBUM")
                .and_then(|v| v.as_str())
                .unwrap_or(""),
        );
        let mut cover_url = String::new();
        let mut duration_ms = song
            .get("DURATION")
            .and_then(|v| v.as_str())
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0)
            * 1000;
        if let Ok(detail) = fetch_detail(client, &id).await {
            album = text_at(&detail, "/songinfo/album").unwrap_or(album);
            cover_url = text_at(&detail, "/songinfo/pic").unwrap_or_default();
            if let Some(seconds) = text_at(&detail, "/songinfo/duration")
                .and_then(|v| v.parse::<u64>().ok())
            {
                duration_ms = seconds * 1000;
            }
        }
        out.push(Candidate {
            source: "kuwo".into(),
            source_id: id,
            title: html_unescape(title),
            artist: artists.join(" / "),
            artists,
            album,
            duration_ms,
            cover_url,
            provider_score: 0.76,
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
    for song in songs.iter().take(8) {
        let Some(id) = song_id(song) else {
            continue;
        };
        let mut candidate = match fetch(client, &id, base_url).await {
            Ok(value) => value,
            Err(err) => {
                tracing::debug!("酷我歌词获取失败 {id}: {err:#}");
                continue;
            }
        };
        if candidate.synced_lyrics.trim().is_empty() && candidate.plain_lyrics.trim().is_empty() {
            continue;
        }
        candidate.title = html_unescape(
            song.get("SONGNAME")
                .and_then(|v| v.as_str())
                .unwrap_or(&candidate.title),
        );
        candidate.artist = split_names(&html_unescape(
            song.get("ARTIST").and_then(|v| v.as_str()).unwrap_or(""),
        ))
        .join(" / ");
        candidate.album = html_unescape(
            song.get("ALBUM")
                .and_then(|v| v.as_str())
                .unwrap_or(""),
        );
        candidate.duration_ms = song
            .get("DURATION")
            .and_then(|v| v.as_str())
            .and_then(|v| v.parse::<u64>().ok())
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
    _base_url: &str,
) -> Result<LyricsCandidate> {
    let detail = fetch_detail(client, id).await?;
    let Some(lines) = detail
        .pointer("/lrclist")
        .and_then(|v| v.as_array())
    else {
        anyhow::bail!("酷我音乐没有歌词");
    };
    let mut synced = String::new();
    for line in lines {
        let Some(text) = line.get("lineLyric").and_then(|v| v.as_str()) else {
            continue;
        };
        let text = html_unescape(text);
        if text.trim().is_empty() {
            continue;
        }
        let seconds = line
            .get("time")
            .and_then(|v| v.as_str())
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(0.0);
        synced.push_str(&format!("[{}{}]{}\n", time_minutes(seconds), time_seconds(seconds), text));
    }
    if synced.trim().is_empty() || is_pure_music(&synced) {
        anyhow::bail!("酷我音乐没有歌词");
    }
    Ok(LyricsCandidate {
        source: "kuwo".into(),
        source_id: id.to_string(),
        title: text_at(&detail, "/songinfo/songName").unwrap_or_default(),
        artist: text_at(&detail, "/songinfo/artist").unwrap_or_default(),
        album: text_at(&detail, "/songinfo/album").unwrap_or_default(),
        duration_ms: text_at(&detail, "/songinfo/duration")
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0)
            * 1000,
        instrumental: false,
        has_synced: true,
        has_translation: false,
        plain_lyrics: String::new(),
        synced_lyrics: synced,
        translated_lyrics: String::new(),
        preview: String::new(),
        match_score: 0.0,
    })
}

async fn fetch_detail(client: &reqwest::Client, id: &str) -> Result<Value> {
    let mut last_error = None;
    for base in [DETAIL_BASE, "https://m.kuwo.cn"] {
        let response = client
            .get(format!("{base}/newh5/singles/songinfoandlrc"))
            .query(&[("musicId", id)])
            .header("User-Agent", "Mozilla/5.0 (TagNest)")
            .send()
            .await;
        let Ok(response) = response else {
            last_error = Some("连接酷我音乐失败".to_string());
            continue;
        };
        if !response.status().is_success() {
            last_error = Some(format!("酷我音乐返回 HTTP {}", response.status()));
            continue;
        }
        let Ok(body) = response.json::<Value>().await else {
            last_error = Some("解析酷我音乐响应失败".to_string());
            continue;
        };
        let data = body.get("data").cloned().unwrap_or(Value::Null);
        if !data.is_null() {
            return Ok(data);
        }
        last_error = Some("酷我音乐没有这首歌".to_string());
    }
    anyhow::bail!(last_error.unwrap_or_else(|| "酷我音乐没有这首歌".to_string()))
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
        .get(format!("{base_url}/r.s"))
        .query(&[
            ("all", term.as_str()),
            ("ft", "music"),
            ("itemset", "web_2013"),
            ("client", "kt"),
            ("pn", "0"),
            ("rn", "15"),
            ("rformat", "json"),
            ("encoding", "utf8"),
        ])
        .header("User-Agent", "Mozilla/5.0 (TagNest)")
        .send()
        .await
        .context("连接酷我音乐失败")?;
    if !response.status().is_success() {
        anyhow::bail!("酷我音乐返回 HTTP {}", response.status());
    }
    let text = response.text().await.context("读取酷我音乐搜索失败")?;
    let fixed = text.replace('\'', "\"");
    let body: Value = serde_json::from_str(&fixed).context("解析酷我音乐搜索失败")?;
    Ok(body
        .get("abslist")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default())
}

fn song_id(song: &Value) -> Option<String> {
    if let Some(id) = song.get("DC_TARGETID").map(json_string) {
        if !id.is_empty() {
            return Some(id);
        }
    }
    let rid = song.get("MUSICRID").and_then(|v| v.as_str()).unwrap_or("");
    let id = rid.trim_start_matches("MUSIC_");
    (!id.is_empty()).then(|| id.to_string())
}

fn json_string(value: &Value) -> String {
    value
        .as_str()
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| value.to_string())
}

fn text_at(value: &Value, pointer: &str) -> Option<String> {
    value
        .pointer(pointer)
        .and_then(|v| v.as_str())
        .map(html_unescape)
        .filter(|v| !v.is_empty())
}

fn split_names(value: &str) -> Vec<String> {
    value
        .split(|c| matches!(c, '/' | '、' | ';' | '；'))
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn html_unescape(value: &str) -> String {
    value
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&#39;", "'")
        .replace("&quot;", "\"")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .trim()
        .to_string()
}

fn time_minutes(seconds: f64) -> String {
    let minutes = (seconds / 60.0).floor() as u64;
    format!("{minutes:02}:")
}

fn time_seconds(seconds: f64) -> String {
    format!("{:05.2}", seconds % 60.0)
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
