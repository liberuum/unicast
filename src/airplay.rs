use std::time::Duration;

use plist::{Dictionary, Value};

use crate::util;

const USER_AGENT: &str = "MediaControl/1.0";
const BINARY_PLIST: &str = "application/x-apple-binary-plist";

#[derive(Debug, Clone, Copy)]
pub struct PlaybackInfo {
    pub position: f64,
    pub duration: f64,
    pub playing: bool,
}

fn client() -> &'static reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap_or_default()
    })
}

fn plist_body(fields: Vec<(&str, Value)>) -> Vec<u8> {
    let mut dictionary = Dictionary::new();
    for (key, value) in fields {
        dictionary.insert(key.to_string(), value);
    }
    let mut buffer = Vec::new();
    let _ = plist::to_writer_binary(&mut buffer, &Value::Dictionary(dictionary));
    buffer
}

pub async fn play(ip: &str, port: u16, url: &str, position: f64) -> Result<(), String> {
    let body = plist_body(vec![
        ("Content-Location", Value::String(url.to_string())),
        ("Start-Position", Value::Real(position)),
        (
            "X-Apple-Session-ID",
            Value::String(uuid::Uuid::new_v4().to_string()),
        ),
    ]);
    let response = client()
        .post(format!("http://{ip}:{port}/play"))
        .header(reqwest::header::USER_AGENT, USER_AGENT)
        .header(reqwest::header::CONTENT_TYPE, BINARY_PLIST)
        .body(body)
        .timeout(Duration::from_secs(8))
        .send()
        .await
        .map_err(|error| format!("AirPlay request failed: {error}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "AirPlay receiver rejected the stream (HTTP {})",
            response.status().as_u16()
        ));
    }
    let _ = client()
        .post(format!("http://{ip}:{port}/rate?value=1.000000"))
        .header(reqwest::header::USER_AGENT, USER_AGENT)
        .timeout(Duration::from_secs(4))
        .send()
        .await;
    Ok(())
}

pub async fn stop(ip: &str, port: u16) {
    let _ = client()
        .post(format!("http://{ip}:{port}/stop"))
        .header(reqwest::header::USER_AGENT, USER_AGENT)
        .timeout(Duration::from_secs(4))
        .send()
        .await;
}

pub async fn pause(ip: &str, port: u16) {
    let _ = client()
        .post(format!("http://{ip}:{port}/rate?value=0.000000"))
        .header(reqwest::header::USER_AGENT, USER_AGENT)
        .timeout(Duration::from_secs(4))
        .send()
        .await;
}

pub async fn resume(ip: &str, port: u16) {
    let _ = client()
        .post(format!("http://{ip}:{port}/rate?value=1.000000"))
        .header(reqwest::header::USER_AGENT, USER_AGENT)
        .timeout(Duration::from_secs(4))
        .send()
        .await;
}

pub async fn seek(ip: &str, port: u16, seconds: f64) {
    let _ = client()
        .post(format!("http://{ip}:{port}/scrub?position={seconds:.3}"))
        .header(reqwest::header::USER_AGENT, USER_AGENT)
        .timeout(Duration::from_secs(4))
        .send()
        .await;
}

/// `/playback-info` is a small plist (position, duration, rate, a few flags).
const MAX_PLAYBACK_INFO_BYTES: usize = 64 * 1024;

pub async fn playback_info(ip: &str, port: u16) -> Option<PlaybackInfo> {
    let response = client()
        .get(format!("http://{ip}:{port}/playback-info"))
        .header(reqwest::header::USER_AGENT, USER_AGENT)
        .timeout(Duration::from_secs(3))
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    let bytes = util::read_body_capped(response, MAX_PLAYBACK_INFO_BYTES).await?;
    parse_playback_info(&bytes)
}

/// Events read from one `/playback-info` reply before giving up. A real one
/// has a few dozen; the cap is what bounds the work, since binary plist
/// objects are shared by reference and a small body can describe an
/// exponentially large tree (which `plist::Value` would build in memory).
const MAX_PLAYBACK_INFO_EVENTS: usize = 4096;

/// Reads `position`, `duration` and `rate` from the top-level dictionary as
/// a stream of events, never materialising the document.
#[allow(clippy::cast_precision_loss)]
fn parse_playback_info(bytes: &[u8]) -> Option<PlaybackInfo> {
    use plist::stream::{Event, Reader};

    let mut events = Reader::new(std::io::Cursor::new(bytes));
    if !matches!(events.next()?.ok()?, Event::StartDictionary(_)) {
        return None;
    }
    let (mut position, mut duration, mut rate) = (0.0, 0.0, 0.0);
    let mut depth = 1usize;
    let mut key: Option<String> = None;
    for (count, event) in events.enumerate() {
        if count >= MAX_PLAYBACK_INFO_EVENTS {
            return None;
        }
        let event = event.ok()?;
        if depth > 1 {
            match event {
                Event::StartArray(_) | Event::StartDictionary(_) => depth += 1,
                Event::EndCollection => depth -= 1,
                _ => {}
            }
            continue;
        }
        let Some(name) = key.take() else {
            match event {
                Event::String(name) => key = Some(name.into_owned()),
                Event::EndCollection => break,
                _ => return None,
            }
            continue;
        };
        if name == "error" {
            return None;
        }
        let number = match event {
            Event::Real(value) => Some(value),
            Event::Integer(value) => value.as_signed().map(|v| v as f64),
            Event::StartArray(_) | Event::StartDictionary(_) => {
                depth += 1;
                None
            }
            _ => None,
        };
        if let Some(number) = number {
            match name.as_str() {
                "position" => position = number,
                "duration" => duration = number,
                "rate" => rate = number,
                _ => {}
            }
        }
    }
    Some(PlaybackInfo {
        position: util::media_seconds(position),
        duration: util::media_seconds(duration),
        playing: rate > 0.0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_playback_info() {
        let body = plist_body(vec![
            ("duration", Value::Real(120.0)),
            ("position", Value::Real(30.5)),
            ("rate", Value::Integer(1.into())),
            ("loadedTimeRanges", Value::Array(vec![Value::Boolean(true)])),
        ]);
        let info = parse_playback_info(&body).expect("info");
        assert!((info.position - 30.5).abs() < 1e-9);
        assert!((info.duration - 120.0).abs() < 1e-9);
        assert!(info.playing);
        let error = plist_body(vec![("error", Value::String("x".into()))]);
        assert!(parse_playback_info(&error).is_none());
    }

    /// A 2 KB binary plist whose top-level dictionary holds five levels of
    /// 200 shared references: 200^5 nodes if built as a `Value`.
    #[test]
    fn shared_reference_bomb_is_refused_quickly() {
        let (levels, width) = (5usize, 200usize);
        let key = levels + 2;
        let mut out = b"bplist00".to_vec();
        let mut offsets = vec![out.len()];
        out.extend_from_slice(&[0xd1]);
        out.extend_from_slice(&u16::try_from(key).expect("ref").to_be_bytes());
        out.extend_from_slice(&1u16.to_be_bytes());
        for level in 1..=levels {
            offsets.push(out.len());
            out.extend_from_slice(&[0xaf, 0x11]);
            out.extend_from_slice(&u16::try_from(width).expect("width").to_be_bytes());
            for _ in 0..width {
                out.extend_from_slice(&u16::try_from(level + 1).expect("ref").to_be_bytes());
            }
        }
        offsets.push(out.len());
        out.push(0x09);
        offsets.push(out.len());
        out.extend_from_slice(&[0x51, b'x']);
        let table = out.len();
        for offset in &offsets {
            out.extend_from_slice(&u32::try_from(*offset).expect("offset").to_be_bytes());
        }
        out.extend_from_slice(&[0; 6]);
        out.extend_from_slice(&[4, 2]);
        out.extend_from_slice(&(offsets.len() as u64).to_be_bytes());
        out.extend_from_slice(&0u64.to_be_bytes());
        out.extend_from_slice(&(table as u64).to_be_bytes());
        assert!(out.len() < MAX_PLAYBACK_INFO_BYTES);
        let started = std::time::Instant::now();
        assert!(parse_playback_info(&out).is_none());
        assert!(started.elapsed() < Duration::from_secs(1));
    }
}
