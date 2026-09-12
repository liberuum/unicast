use serde::{Deserialize, Serialize};

use crate::subtitles::SubtitleTrack;

#[derive(Debug, Clone, Deserialize)]
pub struct Request {
    #[serde(default)]
    pub cmd: String,
    #[serde(default)]
    pub ip: Option<String>,
    #[serde(default)]
    pub file: Option<String>,
    #[serde(default)]
    pub value: Option<String>,
    #[serde(default)]
    pub position: Option<f64>,
    #[serde(default)]
    pub force: bool,
}

impl Request {
    pub fn ip(&self) -> &str {
        self.ip.as_deref().unwrap_or("")
    }

    pub fn file(&self) -> &str {
        self.file.as_deref().unwrap_or("")
    }

    pub fn value(&self) -> &str {
        self.value.as_deref().unwrap_or("")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Device {
    pub name: String,
    pub host: String,
    pub ip: String,
    pub port: u16,
    pub protocol: String,
    pub model: String,
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub control_url: Option<String>,
    /// `UPnP` `RenderingControl` endpoint (DLNA volume/mute), when the renderer has one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rendering_url: Option<String>,
    pub alternates: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[allow(clippy::struct_excessive_bools)]
pub struct StreamInfo {
    pub device: String,
    pub device_ip: String,
    pub protocol: String,
    pub state: String,
    pub position: f64,
    pub duration: f64,
    pub paused: bool,
    /// Receiver volume, 0-100; -1 while unknown.
    pub volume: i32,
    pub muted: bool,
    #[serde(rename = "volumeSupported")]
    pub volume_supported: bool,
    /// Audio boost (dB) baked into the stream currently being served.
    pub boost: i32,
    /// Text subtitle tracks found for this file (sidecar files and embedded streams).
    pub subtitles: Vec<SubtitleTrack>,
    /// Active subtitle track id, -1 when off.
    pub subtitle: i32,
    #[serde(rename = "subtitlesSupported")]
    pub subtitles_supported: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Status {
    pub ok: bool,
    pub state: String,
    pub streams: Vec<StreamInfo>,
    #[serde(rename = "lastReceiver")]
    pub last_receiver: String,
    #[serde(rename = "multicastDiscovery")]
    pub multicast_discovery: bool,
    /// Configured audio boost (dB) for casts.
    #[serde(rename = "audioBoost")]
    pub audio_boost: i32,
    pub error: String,
}

pub fn error_response(message: &str) -> serde_json::Value {
    serde_json::json!({"ok": false, "error": message})
}
