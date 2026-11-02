use crate::config::Config;
use crate::models::{year_from_date, Track};
use crate::util::{join_values, sidecar_path, split_values};
use anyhow::{Context, Result};
use lofty::config::WriteOptions;
use lofty::file::{AudioFile, FileType, TaggedFileExt};
use lofty::picture::{Picture, PictureType};
use lofty::prelude::{Accessor, ItemKey, TagExt};
use lofty::tag::Tag;
use std::io::Cursor;
use std::path::Path;

#[derive(Default)]
pub struct WriteOptionsExtra {
    pub cover: Option<Vec<u8>>,
    pub lyrics: Option<String>,
    pub embed_lyrics: bool,
    pub replace_cover: bool,
    /// Optional per-field delimiter overrides, used by batch normalization so
    /// choosing "artists" does not silently change the genre delimiter too.
    pub separator_overrides: std::collections::HashMap<String, String>,
    /// When set, only these tag fields are touched. Used by batch delimiter
    /// normalization to leave unselected fields byte-for-byte untouched.
    pub only_fields: Option<std::collections::HashSet<String>>,
}

pub fn read_metadata(path: &Path, config: &Config) -> Result<Track> {
    let tagged = lofty::read_from_path(path)
        .with_context(|| format!("无法读取音频标签: {}", path.display()))?;
    let props = tagged.properties();
    let mut track = Track {
        path: path.to_string_lossy().to_string(),
        filename: path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default(),
        format: format_name(tagged.file_type()),
        duration_ms: props.duration().as_millis() as u64,
        bitrate: props.audio_bitrate().unwrap_or(0),
        sample_rate: props.sample_rate().unwrap_or(0),
        channels: props.channels().unwrap_or(0) as u16,
        ..Default::default()
    };
    if let Ok(meta) = std::fs::metadata(path) {
        track.size = meta.len();
        track.mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
    }

    let tag = tagged.primary_tag().or_else(|| tagged.first_tag());
    if let Some(tag) = tag {
        let display_artist = tag
            .get_string(&ItemKey::TrackArtist)
            .unwrap_or("")
            .trim()
            .to_string();
        let raw_artists: Vec<String> = tag
            .get_strings(&ItemKey::TrackArtists)
            .map(ToOwned::to_owned)
            .collect();
        let artist_refs: Vec<&str> = raw_artists.iter().map(String::as_str).collect();
        track.artists = split_field(&artist_refs, config, "artists");
        track.artist = if display_artist.is_empty() {
            track.artists.join(config.separator_for("artists"))
        } else {
            display_artist
        };

        let album_artists: Vec<&str> = tag.get_strings(&ItemKey::AlbumArtist).collect();
        track.albumartist = split_field(&album_artists, config, "albumartist");

        track.title = tag
            .get_string(&ItemKey::TrackTitle)
            .unwrap_or("")
            .trim()
            .to_string();
        track.album = tag
            .get_string(&ItemKey::AlbumTitle)
            .unwrap_or("")
            .trim()
            .to_string();
        track.track = tag.track().or_else(|| {
            tag.get_string(&ItemKey::TrackNumber)
                .and_then(|v| v.parse().ok())
        });
        track.track_total = tag.track_total().or_else(|| {
            tag.get_string(&ItemKey::TrackTotal)
                .and_then(|v| v.parse().ok())
        });
        track.disc = tag.disk().or_else(|| {
            tag.get_string(&ItemKey::DiscNumber)
                .and_then(|v| v.parse().ok())
        });
        track.disc_total = tag.disk_total().or_else(|| {
            tag.get_string(&ItemKey::DiscTotal)
                .and_then(|v| v.parse().ok())
        });
        track.date = tag
            .get_string(&ItemKey::RecordingDate)
            .or_else(|| tag.get_string(&ItemKey::ReleaseDate))
            .unwrap_or("")
            .trim()
            .to_string();
        track.year = year_from_date(&track.date).or_else(|| tag.year());
        if track.date.is_empty() {
            if let Some(year) = track.year {
                track.date = year.to_string();
            }
        }

        track.genres = split_field(
            &tag.get_strings(&ItemKey::Genre).collect::<Vec<_>>(),
            config,
            "genres",
        );
        track.composers = split_field(
            &tag.get_strings(&ItemKey::Composer).collect::<Vec<_>>(),
            config,
            "composers",
        );
        track.lyricists = split_field(
            &tag.get_strings(&ItemKey::Lyricist).collect::<Vec<_>>(),
            config,
            "lyricists",
        );
        track.comment = tag
            .get_string(&ItemKey::Comment)
            .unwrap_or("")
            .trim()
            .to_string();
        track.bpm = tag
            .get_string(&ItemKey::IntegerBpm)
            .and_then(|v| v.parse().ok())
            .or_else(|| tag.get_string(&ItemKey::Bpm).and_then(|v| v.parse().ok()));
        track.isrc = get_text(tag, &ItemKey::Isrc);
        track.label = get_text(tag, &ItemKey::Label);
        track.catalog_number = get_text(tag, &ItemKey::CatalogNumber);
        track.mb_track_id = get_text(tag, &ItemKey::MusicBrainzRecordingId);
        if track.mb_track_id.is_empty() {
            track.mb_track_id = get_text(tag, &ItemKey::MusicBrainzTrackId);
        }
        track.mb_album_id = get_text(tag, &ItemKey::MusicBrainzReleaseId);
        track.mb_artist_id = get_text(tag, &ItemKey::MusicBrainzArtistId);

        track.has_cover = !tag.pictures().is_empty();
        track.has_lyrics = tag
            .get_string(&ItemKey::Lyrics)
            .map(|v| !v.trim().is_empty())
            .unwrap_or(false);
    }

    if !track.has_lyrics {
        let lrc = sidecar_path(path);
        if let Ok(value) = std::fs::read_to_string(&lrc) {
            if !value.trim().is_empty() {
                track.has_lyrics = true;
            }
        }
    }
    if track.has_lyrics {
        track.lyrics_source = read_lyrics_source(path);
    }

    Ok(track)
}

pub fn write_metadata(
    path: &Path,
    track: &Track,
    config: &Config,
    extra: &WriteOptionsExtra,
) -> Result<()> {
    let tagged = lofty::read_from_path(path)
        .with_context(|| format!("无法读取音频标签: {}", path.display()))?;
    let tag_type = tagged.primary_tag_type();
    let mut tag = tagged
        .primary_tag()
        .cloned()
        .unwrap_or_else(|| Tag::new(tag_type));

    let wants = |field: &str| {
        extra
            .only_fields
            .as_ref()
            .map(|fields| fields.contains(field))
            .unwrap_or(true)
    };

    if wants("title") {
        set_text(&mut tag, ItemKey::TrackTitle, &track.title);
    }
    if wants("artist") {
        set_text(&mut tag, ItemKey::TrackArtist, &track.artist);
    }
    if wants("artists") {
        set_text(
            &mut tag,
            ItemKey::TrackArtists,
            &join_values(&track.artists, field_separator(extra, config, "artists")),
        );
    }
    if wants("albumartist") {
        set_text(
            &mut tag,
            ItemKey::AlbumArtist,
            &join_values(
                &track.albumartist,
                field_separator(extra, config, "albumartist"),
            ),
        );
    }
    if wants("album") {
        set_text(&mut tag, ItemKey::AlbumTitle, &track.album);
    }
    if wants("genres") {
        set_text(
            &mut tag,
            ItemKey::Genre,
            &join_values(&track.genres, field_separator(extra, config, "genres")),
        );
    }
    if wants("composers") {
        set_text(
            &mut tag,
            ItemKey::Composer,
            &join_values(
                &track.composers,
                field_separator(extra, config, "composers"),
            ),
        );
    }
    if wants("lyricists") {
        set_text(
            &mut tag,
            ItemKey::Lyricist,
            &join_values(
                &track.lyricists,
                field_separator(extra, config, "lyricists"),
            ),
        );
    }
    if wants("comment") {
        set_text(&mut tag, ItemKey::Comment, &track.comment);
    }

    if wants("track") {
        set_number(&mut tag, NumberField::Track, track.track);
    }
    if wants("track_total") {
        set_number(&mut tag, NumberField::TrackTotal, track.track_total);
    }
    if wants("disc") {
        set_number(&mut tag, NumberField::Disc, track.disc);
    }
    if wants("disc_total") {
        set_number(&mut tag, NumberField::DiscTotal, track.disc_total);
    }

    if wants("date") {
        set_text(&mut tag, ItemKey::RecordingDate, &track.date);
    }
    if wants("date") || wants("year") {
        if let Some(year) = track.year.or_else(|| year_from_date(&track.date)) {
            tag.set_year(year);
        } else {
            tag.remove_year();
        }
    }
    if wants("isrc") {
        set_text(&mut tag, ItemKey::Isrc, &track.isrc);
    }
    if wants("label") {
        set_text(&mut tag, ItemKey::Label, &track.label);
    }
    if wants("catalog_number") {
        set_text(&mut tag, ItemKey::CatalogNumber, &track.catalog_number);
    }
    if wants("mb_track_id") {
        set_text(
            &mut tag,
            ItemKey::MusicBrainzRecordingId,
            &track.mb_track_id,
        );
        tag.remove_key(&ItemKey::MusicBrainzTrackId);
    }
    if wants("mb_album_id") {
        set_text(&mut tag, ItemKey::MusicBrainzReleaseId, &track.mb_album_id);
    }
    if wants("mb_artist_id") {
        set_text(&mut tag, ItemKey::MusicBrainzArtistId, &track.mb_artist_id);
    }
    if wants("bpm") {
        if let Some(bpm) = track.bpm {
            tag.insert_text(ItemKey::IntegerBpm, bpm.to_string());
        } else {
            tag.remove_key(&ItemKey::IntegerBpm);
            tag.remove_key(&ItemKey::Bpm);
        }
    }

    if let Some(cover) = &extra.cover {
        let has_cover = !tag.pictures().is_empty();
        if extra.replace_cover || !has_cover {
            match Picture::from_reader(&mut Cursor::new(cover)) {
                Ok(mut picture) => {
                    picture.set_pic_type(PictureType::CoverFront);
                    tag.remove_picture_type(PictureType::CoverFront);
                    tag.push_picture(picture);
                }
                Err(err) => tracing::warn!("封面图片无法解析，已跳过写入: {err}"),
            }
        }
    }

    if let Some(lyrics) = &extra.lyrics {
        if extra.embed_lyrics {
            set_text(&mut tag, ItemKey::Lyrics, lyrics);
        }
    }

    tag.save_to_path(path, WriteOptions::default())
        .with_context(|| format!("写入音频标签失败: {}", path.display()))?;
    Ok(())
}

pub fn read_cover(path: &Path) -> Option<(Vec<u8>, String)> {
    let tagged = lofty::read_from_path(path).ok()?;
    let tag = tagged.primary_tag().or_else(|| tagged.first_tag())?;
    let picture = tag
        .get_picture_type(PictureType::CoverFront)
        .or_else(|| tag.pictures().first())?;
    let mime = picture
        .mime_type()
        .map(|m| m.as_str().to_string())
        .unwrap_or_else(|| "image/jpeg".to_string());
    Some((picture.data().to_vec(), mime))
}

pub fn read_embedded_lyrics(path: &Path) -> Option<String> {
    let tagged = lofty::read_from_path(path).ok()?;
    let tag = tagged.primary_tag().or_else(|| tagged.first_tag())?;
    tag.get_string(&ItemKey::Lyrics).map(ToOwned::to_owned)
}

pub fn format_name(file_type: FileType) -> String {
    match file_type {
        FileType::Aac => "AAC",
        FileType::Aiff => "AIFF",
        FileType::Ape => "APE",
        FileType::Flac => "FLAC",
        FileType::Mpeg => "MP3",
        FileType::Mp4 => "M4A",
        FileType::Mpc => "MPC",
        FileType::Opus => "OPUS",
        FileType::Vorbis => "OGG",
        FileType::Speex => "SPEEX",
        FileType::Wav => "WAV",
        FileType::WavPack => "WV",
        _ => "AUDIO",
    }
    .to_string()
}

fn field_separator<'a>(extra: &'a WriteOptionsExtra, config: &'a Config, field: &str) -> &'a str {
    extra
        .separator_overrides
        .get(field)
        .map(String::as_str)
        .unwrap_or_else(|| config.separator_for(field))
}

fn get_text(tag: &Tag, key: &ItemKey) -> String {
    tag.get_string(key).unwrap_or("").trim().to_string()
}

fn set_text(tag: &mut Tag, key: ItemKey, value: &str) {
    let value = value.trim();
    if value.is_empty() {
        tag.remove_key(&key);
    } else {
        tag.insert_text(key, value.to_string());
    }
}

/// Split with the global separators plus the field's current write separator,
/// so a value written as " / " can be read back even when `/` is not globally
/// enabled.
fn split_field(raw: &[&str], config: &Config, field: &str) -> Vec<String> {
    let mut separators = config.split_separators.clone();
    let write_separator = config.separator_for(field).to_string();
    if !write_separator.is_empty() && !separators.iter().any(|s| s == &write_separator) {
        separators.push(write_separator);
    }
    if matches!(field, "artists" | "albumartist")
        && !config.scrape_separator.is_empty()
        && !separators.iter().any(|s| s == &config.scrape_separator)
    {
        separators.push(config.scrape_separator.clone());
    }
    split_many(raw, &separators)
}

fn split_many(raw: &[&str], separators: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for value in raw {
        for piece in split_values(value, separators) {
            if !out.iter().any(|v: &String| *v == piece) {
                out.push(piece);
            }
        }
    }
    out
}

enum NumberField {
    Track,
    TrackTotal,
    Disc,
    DiscTotal,
}

fn set_number(tag: &mut Tag, field: NumberField, value: Option<u32>) {
    match field {
        NumberField::Track => match value {
            Some(v) => tag.set_track(v),
            None => tag.remove_track(),
        },
        NumberField::TrackTotal => match value {
            Some(v) => tag.set_track_total(v),
            None => tag.remove_track_total(),
        },
        NumberField::Disc => match value {
            Some(v) => tag.set_disk(v),
            None => tag.remove_disk(),
        },
        NumberField::DiscTotal => match value {
            Some(v) => tag.set_disk_total(v),
            None => tag.remove_disk_total(),
        },
    }
}

fn read_lyrics_source(path: &Path) -> String {
    let Ok(value) = std::fs::read_to_string(sidecar_path(path)) else {
        return String::new();
    };
    for line in value.lines().take(20) {
        if let Some(rest) = line.strip_prefix("[re:") {
            return rest.trim_end_matches(']').trim().to_string();
        }
    }
    String::new()
}
