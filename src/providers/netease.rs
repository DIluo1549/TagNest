use super::{Candidate, LyricsCandidate, SearchQuery};
use crate::util::date_from_unix_millis;
use anyhow::{Context, Result};
use serde_json::Value;

fn referer(base_url: &str) -> String {
    format!("{base_url}/")
}

/// NetEase's search result has enough for matching, while song detail adds
/// cover, track number, label and release date. Detail is best-effort so a
/// partial API response still yields a usable candidate.
pub async fn search_metadata(
    client: &reqwest::Client,
    query: &SearchQuery,
    base_url: &str,
) -> Result<Vec<Candidate>> {
    if query.title.is_empty() {
        return Ok(Vec::new());
    }
    let mut term = String::new();
    if !query.artist.is_empty() {
        term.push_str(&query.artist);
        term.push(' ');
    }
    term.push_str(&query.title);
    let response = client
        .get(format!("{base_url}/api/search/get"))
        .query(&[
            ("s", term.trim()),
            ("type", "1"),
            ("limit", "10"),
            ("offset", "0"),
        ])
        .header("Referer", referer(base_url))
        .send()
        .await
        .context("连接网易云音乐失败")?;
    if !response.status().is_success() {
        anyhow::bail!("网易云音乐返回 HTTP {}", response.status());
    }
    let body: Value = response.json().await.context("解析网易云音乐响应失败")?;
    let songs = body
        .pointer("/result/songs")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let mut out = Vec::new();
    for song in songs.iter().take(6) {
        let Some(title) = song.get("name").and_then(|v| v.as_str()) else {
            continue;
        };
        let source_id = song
            .get("id")
            .map(|v| {
                v.as_str()
                    .map(ToOwned::to_owned)
                    .unwrap_or_else(|| v.to_string())
            })
            .unwrap_or_default();
        if source_id.is_empty() {
            continue;
        }
        let detail = fetch_detail(client, &source_id, base_url).await.ok();
        let detail_song = detail.as_ref().and_then(|value| value.pointer("/songs/0"));
        let artists = names_from(
            detail_song
                .and_then(|value| value.get("artists"))
                .or_else(|| song.get("artists")),
        );
        let artist = artists.join(" / ");
        let album = text_from(
            detail_song
                .and_then(|value| value.pointer("/album/name"))
                .or_else(|| song.pointer("/album/name")),
        );
        let cover = text_from(
            detail_song
                .and_then(|value| value.pointer("/album/picUrl"))
                .or_else(|| song.pointer("/album/picUrl")),
        )
        .replace("http://", "https://");
        let publish_time = detail_song
            .and_then(|value| value.pointer("/album/publishTime"))
            .or_else(|| song.pointer("/album/publishTime"))
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let (date, year) = date_from_unix_millis(publish_time)
            .map(|(date, year)| (date, Some(year)))
            .unwrap_or_default();
        let duration_ms = detail_song
            .and_then(|value| value.get("duration"))
            .or_else(|| song.get("duration"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let track = detail_song
            .and_then(|value| value.get("no"))
            .and_then(|v| v.as_u64())
            .map(|v| v as u32);
        let disc = detail_song
            .and_then(|value| value.get("disc"))
            .and_then(|v| v.as_str())
            .and_then(|v| v.parse().ok())
            .or_else(|| {
                detail_song
                    .and_then(|value| value.get("disc"))
                    .and_then(|v| v.as_u64())
                    .map(|v| v as u32)
            });
        let track_total = detail_song
            .and_then(|value| value.pointer("/album/size"))
            .and_then(|v| v.as_u64())
            .map(|v| v as u32);
        let label = text_from(
            detail_song
                .and_then(|value| value.pointer("/album/company"))
                .or_else(|| song.pointer("/album/company")),
        );
        let albumartist = {
            let names = names_from(
                detail_song
                    .and_then(|value| value.pointer("/album/artists"))
                    .or_else(|| song.pointer("/album/artists")),
            );
            if names.is_empty() {
                artists.clone()
            } else {
                names
            }
        };
        out.push(Candidate {
            source: "netease".into(),
            source_id,
            title: title.trim().to_string(),
            artist,
            artists,
            albumartist,
            album,
            track,
            track_total,
            disc,
            disc_total: None,
            date,
            year,
            genres: Vec::new(),
            label,
            catalog_number: String::new(),
            isrc: String::new(),
            bpm: None,
            duration_ms,
            cover_url: cover,
            mb_track_id: String::new(),
            mb_album_id: String::new(),
            mb_artist_id: String::new(),
            provider_score: 0.72,
            match_score: 0.0,
        });
    }
    Ok(out)
}

async fn fetch_detail(client: &reqwest::Client, id: &str, base_url: &str) -> Result<Value> {
    let response = client
        .get(format!("{base_url}/api/song/detail"))
        .query(&[("ids", format!("[{id}]"))])
        .header("Referer", referer(base_url))
        .send()
        .await
        .context("连接网易云音乐失败")?;
    if !response.status().is_success() {
        anyhow::bail!("网易云音乐详情返回 HTTP {}", response.status());
    }
    response.json().await.context("解析网易云音乐详情失败")
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

fn text_from(value: Option<&Value>) -> String {
    value
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string()
}

pub async fn search(
    client: &reqwest::Client,
    query: &SearchQuery,
    base_url: &str,
) -> Result<Vec<LyricsCandidate>> {
    let mut term = String::new();
    if !query.artist.is_empty() {
        term.push_str(&query.artist);
        term.push(' ');
    }
    if !query.title.is_empty() {
        term.push_str(&query.title);
    }
    if term.trim().is_empty() {
        return Ok(Vec::new());
    }
    let response = client
        .get(format!("{base_url}/api/search/get"))
        .query(&[
            ("s", term.trim()),
            ("type", "1"),
            ("limit", "10"),
            ("offset", "0"),
        ])
        .header("Referer", referer(base_url))
        .send()
        .await
        .context("连接网易云音乐失败")?;
    if !response.status().is_success() {
        anyhow::bail!("网易云音乐返回 HTTP {}", response.status());
    }
    let body: Value = response.json().await.context("解析网易云音乐响应失败")?;
    let songs = body
        .pointer("/result/songs")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let mut out = Vec::new();
    for song in songs.iter().take(6) {
        let id = song
            .get("id")
            .map(|v| {
                v.as_str()
                    .map(ToOwned::to_owned)
                    .unwrap_or_else(|| v.to_string())
            })
            .unwrap_or_default();
        if id.is_empty() {
            continue;
        }
        let mut candidate = match fetch(client, &id, base_url).await {
            Ok(value) => value,
            Err(err) => {
                tracing::debug!("网易云歌词获取失败 {id}: {err:#}");
                continue;
            }
        };
        if candidate.synced_lyrics.is_empty() && candidate.plain_lyrics.is_empty() {
            continue;
        }
        candidate.title = song
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        candidate.artist = song
            .get("artists")
            .and_then(|v| v.as_array())
            .map(|items| {
                items
                    .iter()
                    .filter_map(|a| a.get("name").and_then(|v| v.as_str()))
                    .collect::<Vec<_>>()
                    .join(" / ")
            })
            .unwrap_or_default();
        candidate.album = song
            .pointer("/album/name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        candidate.duration_ms = song.get("duration").and_then(|v| v.as_u64()).unwrap_or(0);
        candidate.preview = make_preview(&candidate.synced_lyrics, &candidate.plain_lyrics);
        out.push(candidate);
    }
    Ok(out)
}

pub async fn fetch(client: &reqwest::Client, id: &str, base_url: &str) -> Result<LyricsCandidate> {
    let response = client
        .get(format!("{base_url}/api/song/lyric"))
        .query(&[("id", id), ("lv", "1"), ("tv", "1"), ("kv", "0")])
        .header("Referer", referer(base_url))
        .send()
        .await
        .context("连接网易云音乐失败")?;
    if !response.status().is_success() {
        anyhow::bail!("网易云音乐歌词返回 HTTP {}", response.status());
    }
    let body: Value = response.json().await.context("解析网易云音乐歌词失败")?;
    let synced = body
        .pointer("/lrc/lyric")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let translated = body
        .pointer("/tlyric/lyric")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let karaoke = body
        .pointer("/klyric/lyric")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let plain = if synced.is_empty() {
        strip_lrc(&karaoke)
    } else {
        String::new()
    };
    Ok(LyricsCandidate {
        source: "netease".into(),
        source_id: id.to_string(),
        title: String::new(),
        artist: String::new(),
        album: String::new(),
        duration_ms: 0,
        instrumental: false,
        has_synced: !synced.is_empty() || !karaoke.is_empty(),
        has_translation: has_meaningful_lyrics(&translated),
        plain_lyrics: plain,
        synced_lyrics: if synced.is_empty() { karaoke } else { synced },
        translated_lyrics: translated,
        preview: String::new(),
        match_score: 0.0,
    })
}

fn make_preview(synced: &str, plain: &str) -> String {
    let source = if synced.is_empty() { plain } else { synced };
    source
        .lines()
        .map(strip_lrc)
        .filter(|line| !line.is_empty())
        .take(4)
        .collect::<Vec<_>>()
        .join("\n")
}

fn has_meaningful_lyrics(value: &str) -> bool {
    value
        .lines()
        .map(strip_lrc)
        .any(|line| !line.trim().is_empty())
}

fn strip_lrc(value: &str) -> String {
    value
        .lines()
        .map(|line| {
            // Drop one or more leading [mm:ss.xx] timestamps.
            let mut rest = line.trim_start();
            while rest.starts_with('[') {
                if let Some(end) = rest.find(']') {
                    rest = rest[end + 1..].trim_start();
                } else {
                    break;
                }
            }
            rest.to_string()
        })
        .filter(|line| !line.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}
