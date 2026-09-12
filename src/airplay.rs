use std::time::Duration;

use plist::{Dictionary, Value};

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
    let bytes = response.bytes().await.ok()?;
    let value = Value::from_reader(std::io::Cursor::new(bytes.as_ref())).ok()?;
    let dictionary = value.as_dictionary()?;
    if dictionary.get("error").is_some() {
        return None;
    }
    let position = real(dictionary, "position");
    let duration = real(dictionary, "duration");
    let rate = real(dictionary, "rate");
    Some(PlaybackInfo {
        position,
        duration,
        playing: rate > 0.0,
    })
}

#[allow(clippy::cast_precision_loss)]
fn real(dictionary: &Dictionary, key: &str) -> f64 {
    dictionary
        .get(key)
        .and_then(|value| {
            value
                .as_real()
                .or_else(|| value.as_signed_integer().map(|v| v as f64))
        })
        .unwrap_or(0.0)
}
