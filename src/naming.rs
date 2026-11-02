use crate::config::Config;
use crate::models::Track;
use crate::util::{join_values, sanitize_component, split_values};
use regex::Regex;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

#[derive(Debug, thiserror::Error)]
pub enum NamingError {
    #[error("模板为空")]
    Empty,
    #[error("模板不能生成绝对路径")]
    Absolute,
    #[error("未知变量: {{{0}}}")]
    UnknownVariable(String),
    #[error("格式说明无效: {{{0}}}")]
    BadFormat(String),
    #[error("模板没有生成有效路径")]
    EmptyPath,
    #[error("目标路径越界")]
    OutsideRoot,
}

pub fn variables() -> &'static [(&'static str, &'static str)] {
    &[
        ("artist", "显示艺术家（单值 ARTIST）"),
        ("artists", "艺术家列表（多值 ARTISTS，用重命名连接符连接）"),
        ("albumartist", "专辑艺术家"),
        ("album", "专辑名"),
        ("title", "歌曲标题"),
        ("track", "音轨号，可写 {track:02d}"),
        ("track_total", "音轨总数"),
        ("disc", "碟号"),
        ("disc_total", "碟总数"),
        ("year", "年份"),
        ("date", "完整日期"),
        ("genre", "流派"),
        ("composer", "作曲"),
        ("label", "唱片公司"),
        ("format", "文件格式，如 FLAC"),
        ("ext", "原扩展名（不含点）"),
        ("filename", "原文件名（不含扩展名）"),
    ]
}

pub struct RenderedName {
    pub relative: PathBuf,
    pub display: String,
}

pub fn render(
    track: &Track,
    template: &str,
    config: &Config,
    artist_separator: Option<&str>,
) -> Result<RenderedName, NamingError> {
    let template = template.trim();
    if template.is_empty() {
        return Err(NamingError::Empty);
    }
    if template.starts_with('/') || template.starts_with('\\') {
        return Err(NamingError::Absolute);
    }
    let ext = if track.ext.is_empty() {
        Path::new(&track.filename)
            .extension()
            .and_then(|v| v.to_str())
            .unwrap_or("")
            .to_string()
    } else {
        track.ext.clone()
    };
    let stem = Path::new(&track.filename)
        .file_stem()
        .and_then(|v| v.to_str())
        .unwrap_or("Unknown");
    let artist_separator = artist_separator
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| config.naming_separator());

    let components: Vec<&str> = template
        .split(['/', '\\'])
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect();
    if components.is_empty() {
        return Err(NamingError::EmptyPath);
    }

    let append_ext = !template.contains("{ext}") && !ext.is_empty();
    let mut relative = PathBuf::new();
    for (index, raw_component) in components.iter().enumerate() {
        let mut component = raw_component.to_string();
        for capture in template_regex().captures_iter(raw_component) {
            let whole = capture.get(0).map(|m| m.as_str()).unwrap_or("");
            let name = capture.get(1).map(|m| m.as_str()).unwrap_or("");
            let spec = capture.get(2).map(|m| m.as_str());
            let value = match name {
                "artist" => artist_for_naming(track, config, artist_separator),
                "artists" => fallback(
                    &join_values(&all_artist_names(track, config), artist_separator),
                    "未知艺术家",
                ),
                "albumartist" => first_or(&track.albumartist, "未知专辑艺术家"),
                "album" => fallback(&track.album, "未知专辑"),
                "title" => fallback(&track.title, stem),
                "track" => format_number(track.track, spec, name)?,
                "track_total" => format_number(track.track_total, spec, name)?,
                "disc" => format_number(track.disc, spec, name)?,
                "disc_total" => format_number(track.disc_total, spec, name)?,
                "year" => track
                    .year
                    .or_else(|| track.date.get(0..4).and_then(|v| v.parse().ok()))
                    .map(|v| v.to_string())
                    .unwrap_or_default(),
                "date" => track.date.clone(),
                "genre" => join_values(&track.genres, config.separator_for("genres")),
                "composer" => join_values(&track.composers, config.separator_for("composers")),
                "label" => track.label.clone(),
                "format" => track.format.clone(),
                "ext" => ext.clone(),
                "filename" => stem.to_string(),
                _ => return Err(NamingError::UnknownVariable(name.to_string())),
            };
            component = component.replace(whole, &value);
        }
        if index + 1 == components.len()
            && append_ext
            && !component
                .to_lowercase()
                .ends_with(&format!(".{}", ext.to_lowercase()))
        {
            component.push('.');
            component.push_str(&ext);
        }
        if component == "." || component == ".." {
            return Err(NamingError::OutsideRoot);
        }
        // A slash inside a tag value must not become an extra directory.
        relative.push(sanitize_component(&component));
    }
    if relative.as_os_str().is_empty() {
        return Err(NamingError::EmptyPath);
    }
    let display = relative.to_string_lossy().replace('\\', "/");
    Ok(RenderedName { relative, display })
}

fn template_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX
        .get_or_init(|| Regex::new(r"\{([a-zA-Z_]+)(?::([^}]+))?\}").expect("valid template regex"))
}

fn all_artist_names(track: &Track, config: &Config) -> Vec<String> {
    let mut names = track.artists.clone();
    if names.len() < 2 {
        let split = split_values(&track.artist, &config.split_separators);
        if split.len() > 1 {
            names = split;
        } else if names.is_empty() {
            names = split;
        }
    }
    names
}

fn artist_for_naming(track: &Track, config: &Config, separator: &str) -> String {
    let names = all_artist_names(track, config);
    if names.len() > 1 {
        join_values(&names, separator)
    } else {
        fallback(track.primary_artist(), "未知艺术家")
    }
}

fn first_or(values: &[String], fallback: &str) -> String {
    values
        .first()
        .map(|v| v.trim())
        .filter(|v| !v.is_empty())
        .unwrap_or(fallback)
        .to_string()
}

fn fallback(value: &str, fallback: &str) -> String {
    if value.trim().is_empty() {
        fallback.to_string()
    } else {
        value.trim().to_string()
    }
}

fn format_number(
    value: Option<u32>,
    spec: Option<&str>,
    name: &str,
) -> Result<String, NamingError> {
    let Some(value) = value else {
        return Ok(String::new());
    };
    let Some(spec) = spec else {
        return Ok(value.to_string());
    };
    let spec = spec.trim();
    if let Some(width) = spec.strip_suffix('d') {
        let width = width.trim_start_matches('0');
        let width = if width.is_empty() {
            1
        } else {
            width
                .parse::<usize>()
                .map_err(|_| NamingError::BadFormat(name.to_string()))?
                .min(12)
        };
        return Ok(format!("{value:0width$}"));
    }
    let width = spec
        .parse::<usize>()
        .map_err(|_| NamingError::BadFormat(name.to_string()))?
        .min(12);
    Ok(format!("{value:0width$}"))
}

pub fn validate_template(template: &str) -> Result<(), NamingError> {
    if template.trim().is_empty() {
        return Err(NamingError::Empty);
    }
    for capture in template_regex().captures_iter(template) {
        let name = capture.get(1).map(|m| m.as_str()).unwrap_or("");
        if !variables().iter().any(|(key, _)| *key == name) {
            return Err(NamingError::UnknownVariable(name.to_string()));
        }
        if let Some(spec) = capture.get(2) {
            let spec = spec.as_str();
            if !spec.trim().ends_with('d') {
                if spec.parse::<usize>().is_err() {
                    return Err(NamingError::BadFormat(name.to_string()));
                }
            } else if spec
                .trim()
                .trim_end_matches('d')
                .trim_start_matches('0')
                .parse::<usize>()
                .is_err()
                && spec.trim().trim_end_matches('d').len() > 0
            {
                return Err(NamingError::BadFormat(name.to_string()));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track() -> Track {
        Track {
            title: "夜曲".into(),
            artists: vec!["周杰伦".into()],
            album: "十一月的萧邦".into(),
            track: Some(1),
            disc: Some(1),
            year: Some(2005),
            date: "2005-11-01".into(),
            ext: "flac".into(),
            filename: "01.flac".into(),
            format: "FLAC".into(),
            ..Default::default()
        }
    }

    #[test]
    fn renders_nested_template() {
        let rendered = render(
            &track(),
            "{artist}/{album}/{track:02d} - {title}",
            &Config::default(),
            None,
        )
        .unwrap();
        assert_eq!(rendered.display, "周杰伦/十一月的萧邦/01 - 夜曲.flac");
    }

    #[test]
    fn preserves_explicit_ext() {
        let rendered = render(
            &track(),
            "{artist} - {title}.{ext}",
            &Config::default(),
            None,
        )
        .unwrap();
        assert_eq!(rendered.display, "周杰伦 - 夜曲.flac");
    }

    #[test]
    fn rejects_unknown_variable() {
        assert!(matches!(
            render(&track(), "{moon}", &Config::default(), None),
            Err(NamingError::UnknownVariable(_))
        ));
    }

    #[test]
    fn multi_artist_in_artist_uses_safe_rename_separator() {
        let mut t = track();
        t.artists = vec!["DJ Snake/Lauv".into()];
        t.artist = "DJ Snake/Lauv".into();
        let rendered = render(&t, "{artist}/{album}/{title}", &Config::default(), None).unwrap();
        assert_eq!(rendered.display, "DJ Snake, Lauv/十一月的萧邦/夜曲.flac");
    }

    #[test]
    fn sanitizes_illegal_characters() {
        let mut t = track();
        t.title = "A B: C?".into();
        let rendered = render(&t, "{title}", &Config::default(), None).unwrap();
        assert_eq!(rendered.display, "A B_ C_.flac");
    }
}
