//! Subtitle discovery and `WebVTT` preparation for cast sessions.
//!
//! Google Cast receivers render sidecar text tracks in `WebVTT` only, so every
//! candidate (an `.srt` next to the file, a `Subs/` folder, or a text stream
//! inside an MKV) is converted with ffmpeg on first request and cached for the
//! session.

use std::path::{Path, PathBuf};

use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubtitleSource {
    Sidecar(PathBuf),
    Embedded { stream_index: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SubtitleTrack {
    /// 1-based, unique within one media file; doubles as the Cast track id.
    pub id: u32,
    /// Human label, e.g. "English (SDH)".
    pub label: String,
    /// Two-letter language code when known, else empty.
    pub language: String,
    #[serde(skip)]
    pub source: SubtitleSource,
}

const SIDECAR_EXTENSIONS: [&str; 4] = ["srt", "vtt", "ass", "ssa"];
const TEXT_CODECS: [&str; 7] = ["subrip", "srt", "ass", "ssa", "webvtt", "mov_text", "text"];
const SUBTITLE_DIRS: [&str; 4] = ["Subs", "subs", "Subtitles", "subtitles"];

/// Recognised language tokens: (aliases, two-letter code, English name).
const LANGUAGES: &[(&[&str], &str, &str)] = &[
    (&["en", "eng", "english"], "en", "English"),
    (
        &["es", "spa", "spanish", "espanol", "español"],
        "es",
        "Spanish",
    ),
    (
        &["fr", "fre", "fra", "french", "francais", "français"],
        "fr",
        "French",
    ),
    (&["de", "ger", "deu", "german", "deutsch"], "de", "German"),
    (&["it", "ita", "italian", "italiano"], "it", "Italian"),
    (
        &["pt", "por", "portuguese", "portugues", "português"],
        "pt",
        "Portuguese",
    ),
    (&["nl", "dut", "nld", "dutch", "nederlands"], "nl", "Dutch"),
    (&["ru", "rus", "russian"], "ru", "Russian"),
    (&["ja", "jpn", "japanese"], "ja", "Japanese"),
    (&["ko", "kor", "korean"], "ko", "Korean"),
    (&["zh", "chi", "zho", "chinese"], "zh", "Chinese"),
    (&["ar", "ara", "arabic"], "ar", "Arabic"),
    (&["hi", "hin", "hindi"], "hi", "Hindi"),
    (&["tr", "tur", "turkish"], "tr", "Turkish"),
    (&["pl", "pol", "polish"], "pl", "Polish"),
    (&["sv", "swe", "swedish"], "sv", "Swedish"),
    (&["da", "dan", "danish"], "da", "Danish"),
    (&["no", "nor", "nob", "norwegian"], "no", "Norwegian"),
    (&["fi", "fin", "finnish"], "fi", "Finnish"),
    (&["cs", "cze", "ces", "czech"], "cs", "Czech"),
    (&["el", "gre", "ell", "greek"], "el", "Greek"),
    (&["he", "heb", "hebrew"], "he", "Hebrew"),
    (&["hu", "hun", "hungarian"], "hu", "Hungarian"),
    (&["ro", "rum", "ron", "romanian"], "ro", "Romanian"),
    (&["uk", "ukr", "ukrainian"], "uk", "Ukrainian"),
    (&["bg", "bul", "bulgarian"], "bg", "Bulgarian"),
    (&["hr", "hrv", "croatian"], "hr", "Croatian"),
    (&["sr", "srp", "serbian"], "sr", "Serbian"),
    (&["sk", "slo", "slk", "slovak"], "sk", "Slovak"),
    (&["sl", "slv", "slovenian"], "sl", "Slovenian"),
    (&["th", "tha", "thai"], "th", "Thai"),
    (&["vi", "vie", "vietnamese"], "vi", "Vietnamese"),
    (&["id", "ind", "indonesian"], "id", "Indonesian"),
    (&["ms", "may", "msa", "malay"], "ms", "Malay"),
    (&["ta", "tam", "tamil"], "ta", "Tamil"),
    (&["te", "tel", "telugu"], "te", "Telugu"),
    (&["et", "est", "estonian"], "et", "Estonian"),
    (&["lv", "lav", "latvian"], "lv", "Latvian"),
    (&["lt", "lit", "lithuanian"], "lt", "Lithuanian"),
    (&["ca", "cat", "catalan"], "ca", "Catalan"),
    (&["fa", "per", "fas", "persian", "farsi"], "fa", "Persian"),
];

fn language_lookup(token: &str) -> Option<(&'static str, &'static str)> {
    let token = token.to_lowercase();
    LANGUAGES
        .iter()
        .find(|(aliases, _, _)| aliases.contains(&token.as_str()))
        .map(|(_, code, name)| (*code, *name))
}

fn is_subtitle_file(path: &Path) -> bool {
    path.is_file()
        && path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| SIDECAR_EXTENSIONS.contains(&ext.to_lowercase().as_str()))
}

fn tokens(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|part| !part.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// Derives (language code, label) from the descriptive part of a sidecar file
/// name, e.g. `en.forced` or `2_English`. `fallback` names an undescribed file.
fn describe_sidecar(descriptor: &str, fallback: &str) -> (String, String) {
    let parts = tokens(descriptor);
    let language = parts.iter().find_map(|part| language_lookup(part));
    let mut flags = Vec::new();
    for part in &parts {
        match part.as_str() {
            "forced" => flags.push("forced"),
            "sdh" | "hi" | "cc" => flags.push("SDH"),
            _ => {}
        }
    }
    flags.dedup();
    let base = language.map_or_else(|| fallback.to_string(), |(_, name)| name.to_string());
    let label = if flags.is_empty() {
        base
    } else {
        format!("{base} ({})", flags.join(", "))
    };
    (
        language.map_or_else(String::new, |(code, _)| code.to_string()),
        label,
    )
}

fn prettify(stem: &str) -> String {
    let words: Vec<String> = tokens(stem)
        .into_iter()
        .filter(|word| !word.chars().all(|c| c.is_ascii_digit()))
        .map(|word| {
            let mut chars = word.chars();
            chars.next().map_or_else(String::new, |first| {
                first.to_uppercase().collect::<String>() + chars.as_str()
            })
        })
        .collect();
    if words.is_empty() {
        "Subtitles".to_string()
    } else {
        words.join(" ")
    }
}

fn sidecars(media: &Path) -> Vec<(PathBuf, String, String)> {
    let Some(dir) = media.parent() else {
        return Vec::new();
    };
    let stem = media
        .file_stem()
        .map(|s| s.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let mut found = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        let mut paths: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| is_subtitle_file(path))
            .collect();
        paths.sort();
        for path in paths {
            let name = path
                .file_stem()
                .map(|s| s.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            if stem.is_empty() || !name.starts_with(&stem) {
                continue;
            }
            let descriptor = name[stem.len()..].trim_start_matches(['.', '_', '-', ' ']);
            let (language, label) = describe_sidecar(descriptor, "Subtitles");
            found.push((path, label, language));
        }
    }
    for folder in SUBTITLE_DIRS {
        let Ok(entries) = std::fs::read_dir(dir.join(folder)) else {
            continue;
        };
        let mut paths: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| is_subtitle_file(path))
            .collect();
        paths.sort();
        for path in paths {
            let name = path
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            let (language, label) = describe_sidecar(&name, &prettify(&name));
            found.push((path, label, language));
        }
    }
    found
}

struct EmbeddedStream {
    index: u32,
    language: String,
    title: String,
}

fn embedded(media: &Path) -> Vec<EmbeddedStream> {
    let output = std::process::Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "s",
            "-show_entries",
            "stream=index,codec_name:stream_tags=language,title",
            "-of",
            "json",
        ])
        .arg(media)
        .output();
    let Ok(output) = output else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&output.stdout) else {
        return Vec::new();
    };
    let text = |v: &serde_json::Value, key: &str| {
        v.get(key)
            .and_then(|t| t.as_str())
            .unwrap_or_default()
            .trim()
            .to_string()
    };
    value
        .get("streams")
        .and_then(|streams| streams.as_array())
        .map(|streams| {
            streams
                .iter()
                .filter(|stream| {
                    TEXT_CODECS.contains(&text(stream, "codec_name").to_lowercase().as_str())
                })
                .filter_map(|stream| {
                    let index = u32::try_from(stream.get("index")?.as_u64()?).ok()?;
                    let tags = stream.get("tags").cloned().unwrap_or_default();
                    Some(EmbeddedStream {
                        index,
                        language: text(&tags, "language"),
                        title: text(&tags, "title"),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Every text subtitle usable for `media`: sidecar files first, then embedded
/// streams in container order. Ids are assigned 1..n.
pub fn discover(media: &Path) -> Vec<SubtitleTrack> {
    let mut tracks = Vec::new();
    for (path, label, language) in sidecars(media) {
        tracks.push(SubtitleTrack {
            id: 0,
            label,
            language,
            source: SubtitleSource::Sidecar(path),
        });
    }
    for stream in embedded(media) {
        let language = language_lookup(&stream.language);
        let base = language.map_or_else(
            || format!("Track {}", stream.index),
            |(_, name)| name.to_string(),
        );
        let label = if stream.title.is_empty() || stream.title.eq_ignore_ascii_case(&base) {
            base
        } else {
            format!("{base} ({})", stream.title)
        };
        tracks.push(SubtitleTrack {
            id: 0,
            label,
            language: language.map_or_else(String::new, |(code, _)| code.to_string()),
            source: SubtitleSource::Embedded {
                stream_index: stream.index,
            },
        });
    }
    for (n, track) in tracks.iter_mut().enumerate() {
        track.id = u32::try_from(n + 1).unwrap_or(u32::MAX);
    }
    tracks
}

/// Path of the `WebVTT` file for `track`, converting it into `dir` on first use.
pub async fn ensure_vtt(
    track: &SubtitleTrack,
    media: &Path,
    dir: &Path,
) -> Result<PathBuf, String> {
    let target = dir.join(format!("{}.vtt", track.id));
    if tokio::fs::metadata(&target).await.is_ok() {
        return Ok(target);
    }
    tokio::fs::create_dir_all(dir)
        .await
        .map_err(|error| format!("cannot create subtitle cache: {error}"))?;
    let part = dir.join(format!("{}.vtt.part", track.id));
    let attempts: Vec<Vec<String>> = match &track.source {
        SubtitleSource::Sidecar(path) => vec![
            ffmpeg_args(path, None, None),
            // Older .srt files are often Latin-1; ffmpeg rejects them as invalid UTF-8.
            ffmpeg_args(path, None, Some("ISO-8859-1")),
        ],
        SubtitleSource::Embedded { stream_index } => {
            vec![ffmpeg_args(media, Some(*stream_index), None)]
        }
    };
    for args in attempts {
        let _ = tokio::fs::remove_file(&part).await;
        let status = tokio::process::Command::new("ffmpeg")
            .args(&args)
            .arg(&part)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .await;
        if status.is_ok_and(|status| status.success())
            && tokio::fs::metadata(&part)
                .await
                .is_ok_and(|meta| meta.len() > 0)
            && tokio::fs::rename(&part, &target).await.is_ok()
        {
            return Ok(target);
        }
    }
    let _ = tokio::fs::remove_file(&part).await;
    Err("ffmpeg could not convert the subtitle track".to_string())
}

fn ffmpeg_args(source: &Path, stream_index: Option<u32>, charenc: Option<&str>) -> Vec<String> {
    let mut args = vec![
        "-nostdin".to_string(),
        "-y".to_string(),
        "-loglevel".to_string(),
        "error".to_string(),
    ];
    if let Some(charenc) = charenc {
        args.push("-sub_charenc".to_string());
        args.push(charenc.to_string());
    }
    args.push("-i".to_string());
    args.push(source.to_string_lossy().to_string());
    if let Some(index) = stream_index {
        args.push("-map".to_string());
        args.push(format!("0:{index}"));
    }
    args.push("-f".to_string());
    args.push("webvtt".to_string());
    args
}

fn parse_timestamp(text: &str) -> Option<f64> {
    let parts: Vec<&str> = text.trim().split(':').collect();
    if parts.len() < 2 || parts.len() > 3 {
        return None;
    }
    let mut seconds = 0.0;
    for part in &parts {
        let value: f64 = part.parse().ok()?;
        seconds = seconds * 60.0 + value;
    }
    Some(seconds)
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn format_timestamp(seconds: f64) -> String {
    let total_ms = (seconds.max(0.0) * 1000.0).round() as u64;
    let hours = total_ms / 3_600_000;
    let minutes = (total_ms % 3_600_000) / 60_000;
    let secs = (total_ms % 60_000) / 1000;
    let millis = total_ms % 1000;
    format!("{hours:02}:{minutes:02}:{secs:02}.{millis:03}")
}

/// Splits a cue timing line into (start, end, trailing settings).
fn parse_cue_timing(line: &str) -> Option<(f64, f64, &str)> {
    let (start, rest) = line.split_once("-->")?;
    let rest = rest.trim_start();
    let (end, settings) = match rest.split_once(char::is_whitespace) {
        Some((end, settings)) => (end, settings.trim()),
        None => (rest.trim_end(), ""),
    };
    Some((parse_timestamp(start)?, parse_timestamp(end)?, settings))
}

/// Rewrites a `WebVTT` document so its cues line up with a stream that started
/// `seconds` into the file: cues that end before that point are dropped, one
/// straddling it is clamped, the rest are shifted earlier.
pub fn shift_vtt(text: &str, seconds: f64) -> String {
    if seconds <= 0.0 {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut pending: Vec<&str> = Vec::new();
    let mut dropping = false;
    for line in text.lines() {
        if line.trim().is_empty() {
            if !dropping {
                for kept in &pending {
                    out.push_str(kept);
                    out.push('\n');
                }
                out.push('\n');
            }
            pending.clear();
            dropping = false;
            continue;
        }
        if dropping {
            continue;
        }
        if let Some((start, end, settings)) = parse_cue_timing(line) {
            let end = end - seconds;
            if end <= 0.0 {
                dropping = true;
                pending.clear();
                continue;
            }
            let start = (start - seconds).max(0.0);
            let mut timing = format!("{} --> {}", format_timestamp(start), format_timestamp(end));
            if !settings.is_empty() {
                timing.push(' ');
                timing.push_str(settings);
            }
            for kept in &pending {
                out.push_str(kept);
                out.push('\n');
            }
            pending.clear();
            out.push_str(&timing);
            out.push('\n');
            continue;
        }
        pending.push(line);
    }
    if !dropping {
        for kept in &pending {
            out.push_str(kept);
            out.push('\n');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sidecar_descriptions() {
        assert_eq!(
            describe_sidecar("", "Subtitles"),
            (String::new(), "Subtitles".to_string())
        );
        assert_eq!(
            describe_sidecar("en", "Subtitles"),
            ("en".to_string(), "English".to_string())
        );
        assert_eq!(
            describe_sidecar("eng.forced", "Subtitles"),
            ("en".to_string(), "English (forced)".to_string())
        );
        assert_eq!(
            describe_sidecar("2_English.SDH", "x"),
            ("en".to_string(), "English (SDH)".to_string())
        );
        assert_eq!(
            describe_sidecar("Director Commentary", &prettify("Director Commentary")),
            (String::new(), "Director Commentary".to_string())
        );
    }

    #[test]
    fn timestamps() {
        assert_eq!(parse_timestamp("01:02:03.500"), Some(3723.5));
        assert_eq!(parse_timestamp("02:03.250"), Some(123.25));
        assert_eq!(parse_timestamp("nope"), None);
        assert_eq!(format_timestamp(3723.5), "01:02:03.500");
        assert_eq!(format_timestamp(0.0), "00:00:00.000");
    }

    #[test]
    fn shifting_cues() {
        let vtt = "WEBVTT\n\nNOTE made up\n\n1\n00:00:05.000 --> 00:00:08.000\nGone\n\n2\n00:00:08.000 --> 00:00:12.000 line:90%\nClamped\n\n00:00:20.000 --> 00:00:25.000\nShifted\n";
        let shifted = shift_vtt(vtt, 10.0);
        assert!(shifted.starts_with("WEBVTT\n\nNOTE made up\n\n"));
        assert!(!shifted.contains("Gone"));
        assert!(!shifted.contains("\n1\n"));
        assert!(shifted.contains("2\n00:00:00.000 --> 00:00:02.000 line:90%\nClamped\n"));
        assert!(shifted.contains("00:00:10.000 --> 00:00:15.000\nShifted\n"));
        assert_eq!(shift_vtt(vtt, 0.0), vtt);
    }

    #[test]
    fn discovers_sidecars() {
        let dir = std::env::temp_dir().join(format!("omarchy-cast-subs-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("Subs")).expect("temp dir");
        let media = dir.join("Movie.2024.mkv");
        std::fs::write(&media, b"").expect("media");
        std::fs::write(dir.join("Movie.2024.srt"), b"1\n").expect("srt");
        std::fs::write(dir.join("Movie.2024.es.srt"), b"1\n").expect("srt");
        std::fs::write(dir.join("Other.srt"), b"1\n").expect("srt");
        std::fs::write(dir.join("Subs").join("3_French.srt"), b"1\n").expect("srt");
        let found = sidecars(&media);
        let labels: Vec<&str> = found.iter().map(|(_, label, _)| label.as_str()).collect();
        assert_eq!(labels, vec!["Spanish", "Subtitles", "French"]);
        assert_eq!(found[0].2, "es");
        assert_eq!(found[2].2, "fr");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
