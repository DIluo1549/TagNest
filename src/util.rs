use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub const AUDIO_EXTENSIONS: &[&str] = &[
    "mp3", "flac", "m4a", "mp4", "ogg", "oga", "opus", "wav", "aif", "aiff", "ape", "wv", "mpc",
    "aac", "spx",
];

/// Convert Unix milliseconds to a calendar date without pulling in a date
/// library. Uses Howard Hinnant's civil-from-days algorithm.
pub fn date_from_unix_millis(ms: i64) -> Option<(String, u32)> {
    if ms <= 0 {
        return None;
    }
    let days = ms.div_euclid(1000).div_euclid(86_400);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let mut year = (yoe + era * 400) as i32;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    if month <= 2 {
        year += 1;
    }
    let year = u32::try_from(year).ok()?;
    Some((format!("{year:04}-{month:02}-{day:02}"), year))
}

pub fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Path text stored in the index must use the platform's native separator.
/// `PathBuf::push("a/b")` keeps the forward slash on Windows, which would make
/// the same file look like two different paths after a rename.
pub fn native_path_string(path: &Path) -> String {
    #[cfg(windows)]
    {
        path.to_string_lossy().replace('/', "\\")
    }
    #[cfg(not(windows))]
    {
        path.to_string_lossy().to_string()
    }
}

pub fn is_audio_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| AUDIO_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

pub fn extension(path: &Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
}

/// Split a raw tag value using all configured separators. Empty values and
/// duplicates are dropped.
pub fn split_values(raw: &str, separators: &[String]) -> Vec<String> {
    let mut values = vec![raw.to_string()];
    for sep in separators {
        if sep.is_empty() {
            continue;
        }
        let mut next = Vec::new();
        for value in values {
            for part in value.split(sep.as_str()) {
                next.push(part.to_string());
            }
        }
        values = next;
    }
    let mut out = Vec::new();
    for value in values {
        let value = value.trim().to_string();
        if !value.is_empty() && !out.iter().any(|v: &String| *v == value) {
            out.push(value);
        }
    }
    out
}

pub fn join_values(values: &[String], separator: &str) -> String {
    values
        .iter()
        .map(|v| v.trim())
        .filter(|v| !v.is_empty())
        .collect::<Vec<_>>()
        .join(separator)
}

/// Sanitize a single path component for Windows/SMB/Linux compatibility.
pub fn sanitize_component(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => out.push('_'),
            c if c.is_control() => out.push('_'),
            c => out.push(c),
        }
    }
    let mut out = out.trim().trim_matches('.').trim().to_string();
    if out.is_empty() {
        out = "Unknown".to_string();
    }
    let stem = out.split('.').next().unwrap_or("").to_ascii_uppercase();
    if matches!(
        stem.as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    ) {
        out = format!("_{out}");
    }
    // Keep well below common 255-byte filename limits, without cutting UTF-8.
    if out.len() > 180 {
        out = out.chars().take(80).collect::<String>();
        out = out.trim().to_string();
        if out.is_empty() {
            out = "Unknown".to_string();
        }
    }
    out
}

pub fn safe_join(root: &Path, relative: &Path) -> Option<PathBuf> {
    if relative.is_absolute() {
        return None;
    }
    for component in relative.components() {
        use std::path::Component;
        match component {
            Component::Normal(_) => {}
            _ => return None,
        }
    }
    Some(root.join(relative))
}

pub fn sidecar_path(path: &Path) -> PathBuf {
    path.with_extension("lrc")
}

pub fn normalize_for_match(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut last_space = true;
    for ch in value.chars().flat_map(char::to_lowercase) {
        if ch.is_alphanumeric() || is_cjk(ch) {
            out.push(ch);
            last_space = false;
        } else if !last_space {
            out.push(' ');
            last_space = true;
        }
    }
    out.trim().to_string()
}

fn is_cjk(ch: char) -> bool {
    matches!(ch as u32, 0x3040..=0x30FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xAC00..=0xD7AF)
}

pub fn similarity(a: &str, b: &str) -> f64 {
    let a = normalize_for_match(a);
    let b = normalize_for_match(b);
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    if a == b {
        return 1.0;
    }
    let base = strsim::jaro_winkler(&a, &b);
    // Token overlap helps with "Artist feat. X" and remaster suffixes.
    let a_tokens: std::collections::HashSet<&str> = a.split_whitespace().collect();
    let b_tokens: std::collections::HashSet<&str> = b.split_whitespace().collect();
    let overlap = if a_tokens.is_empty() || b_tokens.is_empty() {
        0.0
    } else {
        a_tokens.intersection(&b_tokens).count() as f64
            / a_tokens.union(&b_tokens).count().max(1) as f64
    };
    (base * 0.85 + overlap * 0.15).clamp(0.0, 1.0)
}
