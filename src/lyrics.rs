use crate::models::Track;
use crate::tagger;
use crate::util::sidecar_path;
use anyhow::{Context, Result};
use regex::Regex;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::OnceLock;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct LrcLine {
    pub ms: u64,
    pub text: String,
}

#[derive(Clone, Debug, Default)]
pub struct ParsedLrc {
    pub lines: Vec<LrcLine>,
    pub meta: BTreeMap<String, String>,
}

pub fn parse_lrc(value: &str) -> ParsedLrc {
    let mut lines = Vec::new();
    let mut meta = BTreeMap::new();
    for raw in value.lines() {
        let raw = raw.trim_end_matches('\r');
        let mut rest = raw.trim_start();
        let mut timestamps = Vec::new();
        loop {
            let Some(caps) = timestamp_regex().captures(rest) else {
                break;
            };
            let Some(m) = caps.get(0) else { break };
            if m.start() != 0 {
                break;
            }
            let minutes: u64 = caps
                .get(1)
                .and_then(|v| v.as_str().parse().ok())
                .unwrap_or(0);
            let seconds: u64 = caps
                .get(2)
                .and_then(|v| v.as_str().parse().ok())
                .unwrap_or(0);
            let fraction = caps.get(3).map(|v| v.as_str()).unwrap_or("");
            let centis = match fraction.len() {
                0 => 0,
                1 => fraction.parse::<u64>().unwrap_or(0) * 10,
                2 => fraction.parse::<u64>().unwrap_or(0),
                _ => fraction[..2].parse::<u64>().unwrap_or(0),
            };
            timestamps.push((minutes * 60 + seconds) * 1000 + centis * 10);
            rest = &rest[m.end()..];
        }
        if timestamps.is_empty() {
            if let Some(caps) = meta_regex().captures(raw) {
                if let (Some(key), Some(value)) = (caps.get(1), caps.get(2)) {
                    meta.insert(
                        key.as_str().trim().to_string(),
                        value.as_str().trim().to_string(),
                    );
                }
            }
            continue;
        }
        let text = rest.trim().to_string();
        for ms in timestamps {
            lines.push(LrcLine {
                ms,
                text: text.clone(),
            });
        }
    }
    lines.sort_by_key(|line| line.ms);
    ParsedLrc { lines, meta }
}

pub fn compose(
    track: &Track,
    source: &str,
    original_synced: &str,
    original_plain: &str,
    translation: &str,
    bilingual: bool,
) -> String {
    let header = header(track, source);
    let original = parse_lrc(original_synced);
    let translated = parse_lrc(translation);
    let translation_is_synced = !translated.lines.is_empty();

    if !original.lines.is_empty() && bilingual && translation_is_synced {
        let mut translated_by_time: BTreeMap<u64, Vec<String>> = BTreeMap::new();
        for line in &translated.lines {
            if line.text.trim().is_empty() {
                continue;
            }
            translated_by_time
                .entry(line.ms)
                .or_default()
                .push(line.text.trim().to_string());
        }
        let mut body = String::new();
        for line in &original.lines {
            let text = line.text.trim();
            if text.is_empty() {
                continue;
            }
            body.push_str(&format_lrc_line(line.ms, text));
            body.push('\n');
            if let Some(translations) = translation_at(&translated_by_time, line.ms) {
                for translation in translations {
                    body.push_str(&format_lrc_line(line.ms, translation));
                    body.push('\n');
                }
            }
        }
        return format!("{header}{body}");
    }

    if !original.lines.is_empty() {
        let mut body = String::new();
        for line in &original.lines {
            let text = line.text.trim();
            if text.is_empty() {
                continue;
            }
            body.push_str(&format_lrc_line(line.ms, text));
            body.push('\n');
        }
        return format!("{header}{body}");
    }

    // Plain lyrics. If both sides are available, interleave them.
    let original_plain = if original_plain.trim().is_empty() {
        strip_timestamps(original_synced)
    } else {
        original_plain.to_string()
    };
    if original_plain.trim().is_empty() && !translation.trim().is_empty() {
        let fallback = strip_timestamps(translation);
        return format!("{header}{}\n", fallback.trim());
    }
    let mut body = String::new();
    if bilingual && !translation.trim().is_empty() {
        let originals: Vec<&str> = original_plain.lines().collect();
        let translations: Vec<&str> = translation.lines().collect();
        for (index, line) in originals.iter().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            body.push_str(line.trim());
            body.push('\n');
            if let Some(translated) = translations.get(index) {
                if !translated.trim().is_empty() {
                    body.push_str(translated.trim());
                    body.push('\n');
                }
            }
        }
    } else {
        body = original_plain.trim().to_string();
        body.push('\n');
    }
    format!("{header}{body}")
}

pub fn load(path: &Path) -> Option<String> {
    let sidecar = sidecar_path(path);
    if let Ok(value) = std::fs::read_to_string(&sidecar) {
        if !value.trim().is_empty() {
            return Some(value);
        }
    }
    tagger::read_embedded_lyrics(path)
}

pub fn write(path: &Path, content: &str) -> Result<()> {
    let sidecar = sidecar_path(path);
    let tmp = sidecar.with_extension("lrc.tmp");
    std::fs::write(&tmp, content.as_bytes())
        .with_context(|| format!("写入歌词失败: {}", sidecar.display()))?;
    std::fs::rename(&tmp, &sidecar)
        .with_context(|| format!("保存歌词失败: {}", sidecar.display()))?;
    Ok(())
}

/// Translation sources often differ by a few hundred milliseconds from the
/// original. Prefer an exact timestamp, then the closest nearby line.
fn translation_at<'a>(map: &'a BTreeMap<u64, Vec<String>>, ms: u64) -> Option<&'a Vec<String>> {
    if let Some(value) = map.get(&ms) {
        return Some(value);
    }
    let before = map.range(..ms).next_back();
    let after = map.range(ms..).next();
    let (distance, value) = match (before, after) {
        (Some((a, left)), Some((b, right))) => {
            if ms - *a <= *b - ms {
                (ms - *a, left)
            } else {
                (*b - ms, right)
            }
        }
        (Some((a, left)), None) => (ms - *a, left),
        (None, Some((b, right))) => (*b - ms, right),
        (None, None) => return None,
    };
    (distance <= 350).then_some(value)
}

fn header(track: &Track, source: &str) -> String {
    let mut out = String::new();
    if !track.title.is_empty() {
        out.push_str(&format!("[ti:{}]\n", track.title));
    }
    if !track.primary_artist().is_empty() {
        out.push_str(&format!("[ar:{}]\n", track.primary_artist()));
    }
    if !track.album.is_empty() {
        out.push_str(&format!("[al:{}]\n", track.album));
    }
    out.push_str("[by:TagNest]\n");
    if !source.is_empty() {
        out.push_str(&format!("[re:{source}]\n"));
    }
    out
}

fn format_lrc_line(ms: u64, text: &str) -> String {
    let minutes = ms / 60_000;
    let seconds = (ms % 60_000) / 1000;
    let centis = (ms % 1000) / 10;
    format!("[{minutes:02}:{seconds:02}.{centis:02}]{text}")
}

fn strip_timestamps(value: &str) -> String {
    value
        .lines()
        .map(|line| {
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

fn timestamp_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r"^\[(\d{1,3}):(\d{2})(?:[.:](\d{1,3}))?\]").expect("valid timestamp regex")
    })
}

fn meta_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| Regex::new(r"^\[([a-zA-Z]+):(.*)\]$").expect("valid metadata regex"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track() -> Track {
        Track {
            title: "Song".into(),
            artists: vec!["Artist".into()],
            album: "Album".into(),
            ..Default::default()
        }
    }

    #[test]
    fn parses_centiseconds() {
        let parsed = parse_lrc("[ti:Song]\n[00:12.34]Hello\n[01:02.50]World");
        assert_eq!(parsed.lines.len(), 2);
        assert_eq!(parsed.lines[0].ms, 12_340);
        assert_eq!(parsed.meta.get("ti").map(String::as_str), Some("Song"));
    }

    #[test]
    fn merges_translation_at_same_timestamp() {
        let output = compose(
            &track(),
            "netease",
            "[00:01.00]Hello\n[00:02.00]World",
            "",
            "[00:01.00]你好\n[00:02.00]世界",
            true,
        );
        assert!(output.contains("[00:01.00]Hello\n[00:01.00]你好"));
        assert!(output.contains("[00:02.00]World\n[00:02.00]世界"));
    }

    #[test]
    fn leaves_original_when_no_translation() {
        let output = compose(&track(), "lrclib", "[00:01.00]Hello", "", "", true);
        assert!(output.contains("[00:01.00]Hello"));
        assert!(!output.contains("你好"));
    }
}
