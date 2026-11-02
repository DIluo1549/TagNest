use crate::config::Config;
use serde::{Deserialize, Serialize};

/// A track as indexed by TagNest. Multi-value fields are kept as vectors so
/// delimiters can be changed without losing the individual values.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Track {
    pub id: u64,
    pub path: String,
    pub root: String,
    pub rel_path: String,
    pub filename: String,
    pub ext: String,
    pub size: u64,
    pub mtime: i64,
    pub format: String,
    pub duration_ms: u64,
    pub bitrate: u32,
    pub sample_rate: u32,
    pub channels: u16,

    pub title: String,
    /// Display artist: the single credited artist string (`ARTIST` / `TPE1`).
    pub artist: String,
    /// Individual artists: the multi-value `ARTISTS` representation.
    pub artists: Vec<String>,
    pub albumartist: Vec<String>,
    pub album: String,
    pub track: Option<u32>,
    pub track_total: Option<u32>,
    pub disc: Option<u32>,
    pub disc_total: Option<u32>,
    pub date: String,
    pub year: Option<u32>,
    pub genres: Vec<String>,
    pub composers: Vec<String>,
    pub lyricists: Vec<String>,
    pub comment: String,
    pub bpm: Option<u32>,
    pub isrc: String,
    pub label: String,
    pub catalog_number: String,
    pub mb_track_id: String,
    pub mb_album_id: String,
    pub mb_artist_id: String,

    pub has_cover: bool,
    pub has_lyrics: bool,
    pub lyrics_source: String,
    pub scan_error: String,
    pub added_at: i64,
    pub updated_at: i64,
}

impl Track {
    pub fn primary_artist(&self) -> &str {
        if !self.artist.trim().is_empty() {
            &self.artist
        } else {
            self.artists.first().map(String::as_str).unwrap_or("")
        }
    }

    pub fn is_missing_core_tags(&self) -> bool {
        self.title.trim().is_empty()
            || (self.artist.trim().is_empty() && self.artists.is_empty())
            || self.album.trim().is_empty()
    }
}

/// Incoming edits from the UI. Numeric values are strings so `""` can clear
/// the field without needing nested `Option`s in the JSON API.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct TrackPatch {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub artists: Option<Vec<String>>,
    pub albumartist: Option<Vec<String>>,
    pub album: Option<String>,
    pub track: Option<String>,
    pub track_total: Option<String>,
    pub disc: Option<String>,
    pub disc_total: Option<String>,
    pub date: Option<String>,
    pub year: Option<String>,
    pub genres: Option<Vec<String>>,
    pub composers: Option<Vec<String>>,
    pub lyricists: Option<Vec<String>>,
    pub comment: Option<String>,
    pub bpm: Option<String>,
    pub isrc: Option<String>,
    pub label: Option<String>,
    pub catalog_number: Option<String>,
    pub mb_track_id: Option<String>,
    pub mb_album_id: Option<String>,
    pub mb_artist_id: Option<String>,
}

impl TrackPatch {
    pub fn apply(&self, track: &mut Track, now: i64) {
        if let Some(v) = &self.title {
            track.title = v.trim().to_string();
        }
        if let Some(v) = &self.artist {
            track.artist = v.trim().to_string();
        }
        if let Some(v) = &self.artists {
            track.artists = clean_values(v);
        }
        if let Some(v) = &self.albumartist {
            track.albumartist = clean_values(v);
        }
        if let Some(v) = &self.album {
            track.album = v.trim().to_string();
        }
        if let Some(v) = &self.track {
            track.track = parse_num(v);
        }
        if let Some(v) = &self.track_total {
            track.track_total = parse_num(v);
        }
        if let Some(v) = &self.disc {
            track.disc = parse_num(v);
        }
        if let Some(v) = &self.disc_total {
            track.disc_total = parse_num(v);
        }
        if let Some(v) = &self.date {
            track.date = v.trim().to_string();
            track.year = year_from_date(&track.date);
        }
        if let Some(v) = &self.year {
            track.year = parse_num(v);
        }
        if let Some(v) = &self.genres {
            track.genres = clean_values(v);
        }
        if let Some(v) = &self.composers {
            track.composers = clean_values(v);
        }
        if let Some(v) = &self.lyricists {
            track.lyricists = clean_values(v);
        }
        if let Some(v) = &self.comment {
            track.comment = v.trim().to_string();
        }
        if let Some(v) = &self.bpm {
            track.bpm = parse_num(v);
        }
        if let Some(v) = &self.isrc {
            track.isrc = v.trim().to_string();
        }
        if let Some(v) = &self.label {
            track.label = v.trim().to_string();
        }
        if let Some(v) = &self.catalog_number {
            track.catalog_number = v.trim().to_string();
        }
        if let Some(v) = &self.mb_track_id {
            track.mb_track_id = v.trim().to_string();
        }
        if let Some(v) = &self.mb_album_id {
            track.mb_album_id = v.trim().to_string();
        }
        if let Some(v) = &self.mb_artist_id {
            track.mb_artist_id = v.trim().to_string();
        }
        track.updated_at = now;
    }

    pub fn is_empty(&self) -> bool {
        self.title.is_none()
            && self.artist.is_none()
            && self.artists.is_none()
            && self.albumartist.is_none()
            && self.album.is_none()
            && self.track.is_none()
            && self.track_total.is_none()
            && self.disc.is_none()
            && self.disc_total.is_none()
            && self.date.is_none()
            && self.year.is_none()
            && self.genres.is_none()
            && self.composers.is_none()
            && self.lyricists.is_none()
            && self.comment.is_none()
            && self.bpm.is_none()
            && self.isrc.is_none()
            && self.label.is_none()
            && self.catalog_number.is_none()
            && self.mb_track_id.is_none()
            && self.mb_album_id.is_none()
            && self.mb_artist_id.is_none()
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct TrackView {
    #[serde(flatten)]
    pub track: Track,
    pub album_artist: String,
    pub genre: String,
    pub duration: String,
    pub missing_core: bool,
}

impl TrackView {
    pub fn new(track: Track, config: &Config) -> Self {
        let album_artist = track.albumartist.join(config.separator_for("albumartist"));
        let genre = track.genres.join(config.separator_for("genres"));
        let duration = format_duration(track.duration_ms);
        let missing_core = track.is_missing_core_tags();
        Self {
            track,
            album_artist,
            genre,
            duration,
            missing_core,
        }
    }
}

fn clean_values(values: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for value in values {
        let value = value.trim();
        if !value.is_empty() && !out.iter().any(|v: &String| v == value) {
            out.push(value.to_string());
        }
    }
    out
}

fn parse_num(value: &str) -> Option<u32> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    // Accept "3/12" and "3" alike.
    value
        .split(['/', '-'])
        .next()
        .and_then(|v| v.trim().parse::<u32>().ok())
}

pub fn year_from_date(date: &str) -> Option<u32> {
    let digits: String = date.chars().take(4).collect();
    digits
        .parse::<u32>()
        .ok()
        .filter(|y| (1000..=9999).contains(y))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artist_and_artists_are_edited_independently() {
        let mut track = Track::default();
        TrackPatch {
            artist: Some("A & B".into()),
            artists: Some(vec!["A".into(), "B".into()]),
            ..Default::default()
        }
        .apply(&mut track, 1);
        assert_eq!(track.artist, "A & B");
        assert_eq!(track.artists, vec!["A", "B"]);
        assert_eq!(track.primary_artist(), "A & B");

        TrackPatch {
            artists: Some(vec!["One".into(), "Two".into()]),
            ..Default::default()
        }
        .apply(&mut track, 2);
        assert_eq!(track.artist, "A & B");
        assert_eq!(track.artists, vec!["One", "Two"]);
    }
}

pub fn format_duration(ms: u64) -> String {
    let total = ms / 1000;
    let minutes = total / 60;
    let seconds = total % 60;
    format!("{minutes}:{seconds:02}")
}

#[derive(Clone, Debug, Serialize)]
pub struct Stats {
    pub total: usize,
    pub artists: usize,
    pub albums: usize,
    pub missing_core: usize,
    pub with_lyrics: usize,
    pub with_cover: usize,
    pub total_size: u64,
    pub total_duration_ms: u64,
    pub formats: Vec<FormatStat>,
}

#[derive(Clone, Debug, Serialize)]
pub struct FormatStat {
    pub format: String,
    pub count: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub total: usize,
    pub offset: usize,
    pub limit: usize,
}
