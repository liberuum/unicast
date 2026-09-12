use std::net::TcpStream;
use std::path::Path;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use rust_cast::ChannelMessage;
use rust_cast::channels::connection::ConnectionChannel;
use rust_cast::channels::heartbeat::HeartbeatChannel;
use rust_cast::channels::media::{
    IdleReason, MediaChannel, MediaResponse, PlayerState, ResumeState,
};
use rust_cast::channels::receiver::{CastDeviceApp, ReceiverChannel, ReceiverResponse};
use rust_cast::message_manager::{CastMessage, CastMessagePayload, MessageManager};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, ClientConnection, DigitallySignedStruct, SignatureScheme, StreamOwned};
use serde_json::json;

use crate::daemon::Event;
use crate::state::{Session, SessionState};

#[derive(Debug)]
struct AcceptAnyCertificate;

impl ServerCertVerifier for AcceptAnyCertificate {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        rustls::crypto::aws_lc_rs::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

const CAST_PORT: u16 = 8009;
const MEDIA_NAMESPACE: &str = "urn:x-cast:com.google.cast.media";
const SETUP_TIMEOUT: Duration = Duration::from_secs(10);
const POLL_TIMEOUT: Duration = Duration::from_millis(250);
const GET_STATUS_TIMEOUT: Duration = Duration::from_secs(3);
const POLL_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Debug, Clone)]
pub struct CodecPlan {
    pub vcodec: String,
    pub acodec: String,
    /// Channel count of the first audio stream (0 = unknown).
    pub channels: u32,
    pub reason: String,
}

const CAST_AUDIO_OK: [&str; 2] = ["aac", "mp3"];
const DONGLE_HINTS: [&str; 5] = ["ultra", "google tv", "hd", "4k", "android"];

fn hevc_capable(model: &str, name: &str) -> bool {
    let text = format!("{model} {name}").to_lowercase();
    if text.contains("chromecast") && !DONGLE_HINTS.iter().any(|hint| text.contains(hint)) {
        return false;
    }
    true
}

struct ProbeStream {
    codec_type: String,
    codec_name: String,
    level: i64,
    channels: u32,
}

fn probe(path: &Path) -> Vec<ProbeStream> {
    let output = std::process::Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "stream=codec_type,codec_name,profile,level,channels",
            "-of",
            "json",
        ])
        .arg(path)
        .output();
    let Ok(output) = output else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&output.stdout) else {
        return Vec::new();
    };
    value
        .get("streams")
        .and_then(|streams| streams.as_array())
        .map(|streams| {
            streams
                .iter()
                .map(|stream| ProbeStream {
                    codec_type: stream
                        .get("codec_type")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string(),
                    codec_name: stream
                        .get("codec_name")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_lowercase(),
                    level: stream.get("level").map_or(0, |v| {
                        v.as_i64()
                            .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
                            .unwrap_or(0)
                    }),
                    channels: stream
                        .get("channels")
                        .and_then(serde_json::Value::as_u64)
                        .and_then(|v| u32::try_from(v).ok())
                        .unwrap_or(0),
                })
                .collect()
        })
        .unwrap_or_default()
}

pub fn media_duration(path: &Path) -> Option<f64> {
    let output = std::process::Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "default=noprint_wrappers=1:nokey=1",
        ])
        .arg(path)
        .output()
        .ok()?;
    String::from_utf8_lossy(&output.stdout).trim().parse().ok()
}

pub fn keyframe_at_or_before(path: &Path, time: f64) -> Option<f64> {
    if time <= 0.0 {
        return Some(0.0);
    }
    let start = (time - 30.0).max(0.0);
    let interval = format!("{start:.3}%{time:.3}");
    let output = std::process::Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "packet=pts_time,flags",
            "-of",
            "csv=p=0",
            "-read_intervals",
            &interval,
        ])
        .arg(path)
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let mut best: Option<f64> = None;
    for line in text.lines() {
        let mut fields = line.split(',');
        let Some(pts) = fields.next() else { continue };
        let Some(flags) = fields.next() else { continue };
        if !flags.contains('K') {
            continue;
        }
        if let Ok(pts) = pts.trim().parse::<f64>()
            && pts <= time
        {
            best = Some(best.map_or(pts, |current: f64| current.max(pts)));
        }
    }
    best.or(Some(time))
}

pub fn plan_transcode(path: &Path, model: &str, name: &str) -> CodecPlan {
    let hevc_ok = hevc_capable(model, name);
    let streams = probe(path);
    let video = streams.iter().find(|s| s.codec_type == "video");
    let audio = streams.iter().find(|s| s.codec_type == "audio");

    let (vcodec, vreason) = match video.map(|v| (v.codec_name.as_str(), v.level)) {
        Some(("h264", level)) if level > 41 && !hevc_ok => (
            "libx264".to_string(),
            format!("h264 level {level} too high"),
        ),
        Some(("h264", _)) => ("copy".to_string(), "h264 passthrough".to_string()),
        Some(("vp8" | "vp9", _)) => {
            let name = video.map_or("", |v| v.codec_name.as_str());
            ("copy".to_string(), format!("{name} passthrough"))
        }
        Some(("hevc", _)) if hevc_ok => (
            "copy".to_string(),
            "hevc passthrough (device supports it)".to_string(),
        ),
        Some((other, _)) => ("libx264".to_string(), format!("{other} not supported")),
        None => ("libx264".to_string(), "unknown not supported".to_string()),
    };

    let audio_name = audio.map_or(String::new(), |a| a.codec_name.clone());
    let acodec = if !audio_name.is_empty() && !CAST_AUDIO_OK.contains(&audio_name.as_str()) {
        "aac".to_string()
    } else {
        "copy".to_string()
    };
    let shown_audio = if audio_name.is_empty() {
        "none"
    } else {
        audio_name.as_str()
    };
    CodecPlan {
        vcodec,
        acodec,
        channels: audio.map_or(0, |a| a.channels),
        reason: format!("{vreason}; audio {shown_audio}"),
    }
}

/// Plan for receivers that decode the file natively (DLNA, AirPlay): everything
/// is passed through. Only the channel count is probed, so a boosted stream can
/// downmix sensibly.
pub fn passthrough_plan(path: &Path) -> CodecPlan {
    let streams = probe(path);
    let audio = streams.iter().find(|s| s.codec_type == "audio");
    CodecPlan {
        vcodec: "copy".to_string(),
        acodec: "copy".to_string(),
        channels: audio.map_or(0, |a| a.channels),
        reason: "receiver decodes natively".to_string(),
    }
}

#[derive(Debug)]
pub enum CastCommand {
    Pause,
    Resume,
    Shutdown,
    Seek(f64),
    /// Receiver volume, 0.0-1.0.
    SetVolume(f32),
    SetMute(bool),
    /// Point the receiver at a different stream URL (the media server was
    /// rebuilt, e.g. with a new audio boost) and resume at a position.
    Reload(ReloadRequest),
    /// Show one of the declared subtitle tracks (`None` = off).
    SetSubtitle(Option<u32>),
}

/// A sidecar `WebVTT` subtitle track offered to the receiver.
#[derive(Debug, Clone)]
pub struct TextTrack {
    pub id: u32,
    /// Base URL of the `WebVTT` file; `?start=` is appended for ffmpeg streams
    /// that begin part-way through the file.
    pub url: String,
    pub name: String,
    /// RFC 5646 language code, or empty when unknown.
    pub language: String,
}

#[derive(Debug, Clone)]
pub struct ReloadRequest {
    pub url: String,
    pub transcode: bool,
    pub exact_seek: bool,
    pub position: f64,
}

pub struct CastSession {
    pub tx: tokio::sync::mpsc::UnboundedSender<CastCommand>,
}

impl CastSession {
    pub fn pause(&self) {
        let _ = self.tx.send(CastCommand::Pause);
    }

    pub fn resume(&self) {
        let _ = self.tx.send(CastCommand::Resume);
    }

    pub fn shutdown(&self) {
        let _ = self.tx.send(CastCommand::Shutdown);
    }

    pub fn seek(&self, position: f64) {
        let _ = self.tx.send(CastCommand::Seek(position));
    }

    pub fn set_volume(&self, level: f32) {
        let _ = self.tx.send(CastCommand::SetVolume(level));
    }

    pub fn set_mute(&self, muted: bool) {
        let _ = self.tx.send(CastCommand::SetMute(muted));
    }

    pub fn reload(&self, request: ReloadRequest) {
        let _ = self.tx.send(CastCommand::Reload(request));
    }

    pub fn set_subtitle(&self, track: Option<u32>) {
        let _ = self.tx.send(CastCommand::SetSubtitle(track));
    }
}

#[derive(Clone)]
pub struct CastOptions {
    pub duration: Option<f64>,
    pub transcode: bool,
    pub exact_seek: bool,
    pub source: std::path::PathBuf,
    /// Subtitle tracks declared with every load.
    pub tracks: Vec<TextTrack>,
    /// The track the receiver should show, if any.
    pub active_track: Option<u32>,
}

pub async fn connect(
    ip: &str,
    url: &str,
    title: &str,
    session_id: u64,
    options: CastOptions,
    state: Arc<RwLock<Session>>,
    events: tokio::sync::mpsc::UnboundedSender<Event>,
) -> Result<CastSession, String> {
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
    let thread_tx = cmd_tx.clone();
    let host = ip.to_string();
    let media_url = url.to_string();
    let media_title = title.to_string();
    std::thread::Builder::new()
        .name("castd-cast".to_string())
        .spawn(move || {
            let result = run_session(
                &host,
                &media_url,
                &media_title,
                session_id,
                &options,
                &state,
                &events,
                cmd_rx,
                ready_tx,
            );
            if let Err(error) = result {
                tracing::warn!("cast session ended: {error}");
            }
        })
        .map_err(|error| format!("could not start cast thread: {error}"))?;

    match tokio::time::timeout(Duration::from_secs(25), ready_rx).await {
        Ok(Ok(Ok(()))) => Ok(CastSession { tx: cmd_tx }),
        Ok(Ok(Err(error))) => Err(error),
        Ok(Err(_)) => Err("The cast session terminated unexpectedly".to_string()),
        Err(_) => {
            let _ = thread_tx.send(CastCommand::Shutdown);
            Err("Timed out connecting to the receiver".to_string())
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn handshake(
    connection_channel: &Connection<'_>,
    heartbeat: &Heartbeat<'_>,
    receiver: &Receiver<'_>,
    media: &MediaChan<'_>,
    manager: &Manager,
    content_url: &str,
    title: &str,
    options: &CastOptions,
) -> Result<
    (
        rust_cast::channels::receiver::Application,
        rust_cast::channels::media::Status,
    ),
    String,
> {
    connection_channel
        .connect("receiver-0")
        .map_err(|error| format!("cast handshake failed: {error}"))?;
    heartbeat
        .ping()
        .map_err(|error| format!("cast handshake failed: {error}"))?;
    let app = receiver
        .launch_app(&CastDeviceApp::DefaultMediaReceiver)
        .map_err(|error| format!("the receiver rejected the session: {error}"))?;
    connection_channel
        .connect(app.transport_id.as_str())
        .map_err(|error| format!("cast handshake failed: {error}"))?;
    let status = load_media(manager, media, &app, content_url, title, options, 0.0, 0.0)
        .map_err(|error| format!("the receiver could not load the media: {error}"))?;
    Ok((app, status))
}

/// Sends a LOAD for `content_id` with the session's subtitle tracks declared
/// and waits for the resulting media status. Built by hand because the
/// library's media type has no track support. `track_start` re-times the
/// subtitle URLs when the stream itself begins part-way through the file.
#[allow(clippy::too_many_arguments)]
fn load_media(
    manager: &Manager,
    media: &MediaChan<'_>,
    app: &rust_cast::channels::receiver::Application,
    content_id: &str,
    title: &str,
    options: &CastOptions,
    current_time: f64,
    track_start: f64,
) -> Result<rust_cast::channels::media::Status, String> {
    let request_id = manager.generate_request_id().get();
    let media_json = media_payload(content_id, title, options, track_start);
    let mut request = json!({
        "type": "LOAD",
        "requestId": request_id,
        "sessionId": app.session_id,
        "media": media_json,
        "autoplay": true,
        "currentTime": current_time,
    });
    if let Some(active) = options
        .active_track
        .filter(|id| options.tracks.iter().any(|track| track.id == *id))
    {
        request["activeTrackIds"] = json!([active]);
    }
    manager
        .send(CastMessage {
            namespace: MEDIA_NAMESPACE.to_string(),
            source: "sender-0".to_string(),
            destination: app.transport_id.clone(),
            payload: CastMessagePayload::String(request.to_string()),
        })
        .map_err(|error| format!("could not send the load request: {error}"))?;
    manager
        .receive_find_map(|message| {
            if !media.can_handle(message) {
                return Ok(None);
            }
            match media.parse(message)? {
                MediaResponse::Status(status) => {
                    // Some receivers answer with a status that lacks our request
                    // id; accept one that already carries the media we asked for.
                    let ours = status.request_id == request_id
                        || status.entries.iter().any(|entry| {
                            entry
                                .media
                                .as_ref()
                                .is_some_and(|loaded| loaded.content_id == content_id)
                        });
                    Ok(ours.then_some(status))
                }
                MediaResponse::LoadFailed(error) if error.request_id == request_id => {
                    Err(rust_cast::errors::Error::Internal(
                        "the receiver failed to load the media".to_string(),
                    ))
                }
                MediaResponse::LoadCancelled(error) if error.request_id == request_id => {
                    Err(rust_cast::errors::Error::Internal(
                        "the load was cancelled by another request".to_string(),
                    ))
                }
                _ => Ok(None),
            }
        })
        .map_err(|error| error.to_string())
}

/// The `media` object of a LOAD request. The receiver echoes it back in every
/// `MEDIA_STATUS`, and the library's parser insists on `metadata.images`, so it
/// is always present even when empty.
fn media_payload(
    content_id: &str,
    title: &str,
    options: &CastOptions,
    track_start: f64,
) -> serde_json::Value {
    let mut media_json = json!({
        "contentId": content_id,
        "contentType": "video/mp4",
        "streamType": "BUFFERED",
        "metadata": {"metadataType": 0, "title": title, "images": []},
    });
    if let Some(duration) = options.duration {
        media_json["duration"] = json!(duration);
    }
    if !options.tracks.is_empty() {
        let tracks: Vec<serde_json::Value> = options
            .tracks
            .iter()
            .map(|track| {
                let url = if track_start > 0.0 {
                    format!("{}?start={track_start:.3}", track.url)
                } else {
                    track.url.clone()
                };
                let mut entry = json!({
                    "trackId": track.id,
                    "type": "TEXT",
                    "subtype": "SUBTITLES",
                    "trackContentId": url,
                    "trackContentType": "text/vtt",
                    "name": track.name,
                });
                if !track.language.is_empty() {
                    entry["language"] = json!(track.language);
                }
                entry
            })
            .collect();
        media_json["tracks"] = json!(tracks);
        media_json["textTrackStyle"] = json!({
            "backgroundColor": "#00000066",
            "foregroundColor": "#FFFFFFFF",
            "edgeType": "OUTLINE",
            "edgeColor": "#000000FF",
            "fontScale": 1.0,
            "fontGenericFamily": "SANS_SERIF",
        });
    }
    media_json
}

/// Switches the receiver's active subtitle track without reloading the media.
fn edit_tracks(
    manager: &Manager,
    media: &MediaChan<'_>,
    app: &rust_cast::channels::receiver::Application,
    media_session_id: i32,
    active: Option<u32>,
) -> Result<(), String> {
    let request_id = manager.generate_request_id().get();
    let request = json!({
        "type": "EDIT_TRACKS_INFO",
        "requestId": request_id,
        "mediaSessionId": media_session_id,
        "activeTrackIds": active.map_or_else(Vec::new, |id| vec![id]),
    });
    manager
        .send(CastMessage {
            namespace: MEDIA_NAMESPACE.to_string(),
            source: "sender-0".to_string(),
            destination: app.transport_id.clone(),
            payload: CastMessagePayload::String(request.to_string()),
        })
        .map_err(|error| format!("could not send the track change: {error}"))?;
    manager
        .receive_find_map(|message| {
            if !media.can_handle(message) {
                return Ok(None);
            }
            match media.parse(message)? {
                MediaResponse::Status(status) if status.request_id == request_id => Ok(Some(())),
                MediaResponse::InvalidRequest(error) => Err(rust_cast::errors::Error::Internal(
                    format!("the receiver rejected the track change: {error:?}"),
                )),
                _ => Ok(None),
            }
        })
        .map_err(|error| error.to_string())
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    clippy::cast_possible_truncation
)]
fn run_session(
    host: &str,
    url: &str,
    title: &str,
    session_id: u64,
    initial_options: &CastOptions,
    state: &Arc<RwLock<Session>>,
    events: &tokio::sync::mpsc::UnboundedSender<Event>,
    mut commands: tokio::sync::mpsc::UnboundedReceiver<CastCommand>,
    ready: tokio::sync::oneshot::Sender<Result<(), String>>,
) -> Result<(), String> {
    // A Reload swaps the stream URL and its seek behaviour mid-session.
    let mut options = initial_options.clone();
    let tcp = TcpStream::connect((host, CAST_PORT))
        .map_err(|error| format!("Chromecast at {host} not reachable: {error}"))?;
    let timeout_handle = tcp
        .try_clone()
        .map_err(|error| format!("could not prepare cast socket: {error}"))?;
    timeout_handle
        .set_read_timeout(Some(SETUP_TIMEOUT))
        .map_err(|error| format!("could not configure cast socket: {error}"))?;
    timeout_handle.set_write_timeout(Some(SETUP_TIMEOUT)).ok();

    let config = ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptAnyCertificate))
        .with_no_client_auth();
    let server_name = ServerName::try_from(host)
        .map_err(|error| format!("invalid cast host: {error}"))?
        .to_owned();
    let connection = ClientConnection::new(Arc::new(config), server_name)
        .map_err(|error| format!("TLS setup failed: {error}"))?;
    let stream = StreamOwned::new(connection, tcp);

    let manager = Arc::new(MessageManager::new(stream));
    let connection_channel = ConnectionChannel::new("sender-0", Arc::clone(&manager));
    let heartbeat = HeartbeatChannel::new("sender-0", "receiver-0", Arc::clone(&manager));
    let media = MediaChannel::new("sender-0", Arc::clone(&manager));
    let receiver = ReceiverChannel::new("sender-0", "receiver-0", Arc::clone(&manager));

    // The URL the receiver currently plays (without any `?start=`); a Reload
    // swaps it when the media server is rebuilt.
    let mut content_url = url.to_string();
    let setup = handshake(
        &connection_channel,
        &heartbeat,
        &receiver,
        &media,
        &manager,
        &content_url,
        title,
        &options,
    );
    let (app, initial_status) = match setup {
        Ok(value) => {
            let _ = ready.send(Ok(()));
            value
        }
        Err(error) => {
            let _ = ready.send(Err(error.clone()));
            return Err(error);
        }
    };
    let mut position_offset = 0.0_f64;
    publish_status(state, session_id, &initial_status, position_offset, -1);
    match receiver.get_status() {
        Ok(status) => publish_volume(state, session_id, &status.volume),
        Err(error) => tracing::debug!("cast receiver status unavailable: {error}"),
    }

    timeout_handle
        .set_read_timeout(Some(POLL_TIMEOUT))
        .map_err(|error| format!("could not configure cast socket: {error}"))?;

    let mut media_session_id = 0_i32;
    let mut consecutive_errors = 0_u32;
    let mut running = true;
    let mut last_poll = Instant::now();
    let mut polls = 0_u32;
    while running {
        while let Ok(command) = commands.try_recv() {
            match command {
                CastCommand::Pause => {
                    if media_session_id != 0
                        && media
                            .pause(app.transport_id.as_str(), media_session_id)
                            .is_ok()
                    {
                        update(state, session_id, |session| {
                            if session.state == SessionState::Playing {
                                session.state = SessionState::Paused;
                            }
                        });
                    }
                }
                CastCommand::Resume => {
                    if media_session_id != 0
                        && media
                            .play(app.transport_id.as_str(), media_session_id)
                            .is_ok()
                    {
                        update(state, session_id, |session| {
                            if session.state == SessionState::Paused {
                                session.state = SessionState::Playing;
                            }
                        });
                    }
                }
                CastCommand::Shutdown => {
                    running = false;
                }
                CastCommand::Seek(position) => {
                    if media_session_id != 0 {
                        seek_cast(
                            &manager,
                            &media,
                            &app,
                            &content_url,
                            title,
                            &options,
                            &timeout_handle,
                            &mut media_session_id,
                            position,
                            &mut position_offset,
                            state,
                            session_id,
                        );
                    }
                }
                CastCommand::SetSubtitle(track) => {
                    options.active_track = track;
                    if media_session_id != 0 {
                        let _ = timeout_handle.set_read_timeout(Some(GET_STATUS_TIMEOUT));
                        match edit_tracks(&manager, &media, &app, media_session_id, track) {
                            Ok(()) => update(state, session_id, |session| session.subtitle = track),
                            Err(error) => tracing::warn!("cast subtitle change failed: {error}"),
                        }
                        let _ = timeout_handle.set_read_timeout(Some(POLL_TIMEOUT));
                    }
                }
                CastCommand::SetVolume(level) => {
                    let _ = timeout_handle.set_read_timeout(Some(GET_STATUS_TIMEOUT));
                    match receiver.set_volume(level.clamp(0.0, 1.0)) {
                        Ok(volume) => publish_volume(state, session_id, &volume),
                        Err(error) => tracing::warn!("cast set volume failed: {error}"),
                    }
                    let _ = timeout_handle.set_read_timeout(Some(POLL_TIMEOUT));
                }
                CastCommand::SetMute(muted) => {
                    let _ = timeout_handle.set_read_timeout(Some(GET_STATUS_TIMEOUT));
                    match receiver.set_volume(muted) {
                        Ok(volume) => publish_volume(state, session_id, &volume),
                        Err(error) => tracing::warn!("cast set mute failed: {error}"),
                    }
                    let _ = timeout_handle.set_read_timeout(Some(POLL_TIMEOUT));
                }
                CastCommand::Reload(request) => {
                    content_url = request.url;
                    options.transcode = request.transcode;
                    options.exact_seek = request.exact_seek;
                    if request.transcode {
                        seek_cast(
                            &manager,
                            &media,
                            &app,
                            &content_url,
                            title,
                            &options,
                            &timeout_handle,
                            &mut media_session_id,
                            request.position,
                            &mut position_offset,
                            state,
                            session_id,
                        );
                    } else {
                        position_offset = 0.0;
                        let _ = timeout_handle.set_read_timeout(Some(SETUP_TIMEOUT));
                        let load = load_media(
                            &manager,
                            &media,
                            &app,
                            &content_url,
                            title,
                            &options,
                            request.position,
                            0.0,
                        );
                        let _ = timeout_handle.set_read_timeout(Some(POLL_TIMEOUT));
                        match load {
                            Ok(status) => {
                                media_session_id = status
                                    .entries
                                    .first()
                                    .map_or(media_session_id, |entry| entry.media_session_id);
                                update(state, session_id, |session| {
                                    session.position = request.position;
                                    session.state = SessionState::Buffering;
                                });
                            }
                            Err(error) => tracing::warn!("cast reload failed: {error}"),
                        }
                    }
                }
            }
        }
        if !running {
            break;
        }
        if is_stale(state, session_id) {
            return Ok(());
        }
        match receive(&manager, &connection_channel, &heartbeat, &media, &receiver) {
            Ok(ChannelMessage::Heartbeat(_)) => {
                let _ = heartbeat.pong();
            }
            Ok(ChannelMessage::Receiver(ReceiverResponse::Status(status))) => {
                publish_volume(state, session_id, &status.volume);
            }
            Ok(ChannelMessage::Media(MediaResponse::Status(status))) => {
                let ended = publish_status(
                    state,
                    session_id,
                    &status,
                    position_offset,
                    media_session_id,
                );
                if let Some(entry) = status.entries.first()
                    && (media_session_id == 0 || entry.media_session_id == media_session_id)
                {
                    media_session_id = entry.media_session_id;
                }
                if ended {
                    teardown(&media, &receiver, &app, media_session_id);
                    let _ = events.send(Event::SessionEnded { session_id });
                    return Ok(());
                }
            }
            Ok(ChannelMessage::Media(MediaResponse::LoadFailed(_))) => {
                set_error(state, session_id, "Load failed.");
                teardown(&media, &receiver, &app, media_session_id);
                let _ = events.send(Event::SessionEnded { session_id });
                return Ok(());
            }
            Ok(ChannelMessage::Media(MediaResponse::LoadCancelled(_))) => {
                tracing::debug!("a cast load request was superseded");
            }
            Ok(ChannelMessage::Media(MediaResponse::Error(error))) => {
                set_error(
                    state,
                    session_id,
                    &format!("The receiver reported: {error:?}"),
                );
                teardown(&media, &receiver, &app, media_session_id);
                let _ = events.send(Event::SessionEnded { session_id });
                return Ok(());
            }
            Ok(_) => {}
            Err(error) if is_timeout(&error) => {}
            Err(error) => {
                consecutive_errors += 1;
                tracing::debug!("cast receive error: {error}");
                if consecutive_errors > 5 {
                    set_error(state, session_id, "Lost the connection to the receiver.");
                    let _ = events.send(Event::SessionEnded { session_id });
                    return Err(error.to_string());
                }
            }
        }
        if matches!(
            session_state(state, session_id),
            Some(SessionState::Playing | SessionState::Paused)
        ) && last_poll.elapsed() >= POLL_INTERVAL
        {
            last_poll = Instant::now();
            polls = polls.wrapping_add(1);
            if polls % 5 == 0 {
                let _ = timeout_handle.set_read_timeout(Some(GET_STATUS_TIMEOUT));
                if let Ok(status) = receiver.get_status() {
                    publish_volume(state, session_id, &status.volume);
                }
                let _ = timeout_handle.set_read_timeout(Some(POLL_TIMEOUT));
            }
            if media_session_id != 0 {
                let _ = timeout_handle.set_read_timeout(Some(GET_STATUS_TIMEOUT));
                if let Ok(status) =
                    media.get_status(app.transport_id.as_str(), Some(media_session_id))
                    && publish_status(
                        state,
                        session_id,
                        &status,
                        position_offset,
                        media_session_id,
                    )
                {
                    teardown(&media, &receiver, &app, media_session_id);
                    let _ = events.send(Event::SessionEnded { session_id });
                    return Ok(());
                }
                let _ = timeout_handle.set_read_timeout(Some(POLL_TIMEOUT));
            }
        }
    }
    teardown(&media, &receiver, &app, media_session_id);
    Ok(())
}

type Manager = Arc<MessageManager<StreamOwned<ClientConnection, TcpStream>>>;
type Connection<'a> = ConnectionChannel<'a, StreamOwned<ClientConnection, TcpStream>>;
type MediaChan<'a> =
    rust_cast::channels::media::MediaChannel<'a, StreamOwned<ClientConnection, TcpStream>>;
type Receiver<'a> = ReceiverChannel<'a, StreamOwned<ClientConnection, TcpStream>>;
type Heartbeat<'a> = HeartbeatChannel<'a, StreamOwned<ClientConnection, TcpStream>>;

fn receive(
    manager: &Manager,
    connection: &Connection<'_>,
    heartbeat: &Heartbeat<'_>,
    media: &MediaChan<'_>,
    receiver: &Receiver<'_>,
) -> Result<ChannelMessage, rust_cast::errors::Error> {
    let message = manager.receive()?;
    if connection.can_handle(&message) {
        return Ok(ChannelMessage::Connection(connection.parse(&message)?));
    }
    if heartbeat.can_handle(&message) {
        return Ok(ChannelMessage::Heartbeat(heartbeat.parse(&message)?));
    }
    if media.can_handle(&message) {
        return Ok(ChannelMessage::Media(media.parse(&message)?));
    }
    if receiver.can_handle(&message) {
        return Ok(ChannelMessage::Receiver(receiver.parse(&message)?));
    }
    Ok(ChannelMessage::Raw(message))
}

#[allow(clippy::too_many_arguments, clippy::cast_possible_truncation)]
fn seek_cast(
    manager: &Manager,
    media: &MediaChan<'_>,
    app: &rust_cast::channels::receiver::Application,
    content_url: &str,
    title: &str,
    options: &CastOptions,
    timeout_handle: &TcpStream,
    media_session_id: &mut i32,
    position: f64,
    position_offset: &mut f64,
    state: &Arc<RwLock<Session>>,
    session_id: u64,
) {
    if options.transcode {
        let start = if options.exact_seek {
            position
        } else {
            keyframe_at_or_before(&options.source, position).unwrap_or(position)
        };
        *position_offset = start;
        let seek_url = format!("{content_url}?start={start:.3}");
        let _ = timeout_handle.set_read_timeout(Some(SETUP_TIMEOUT));
        // Subtitles are re-timed to the new stream start too.
        let load = load_media(manager, media, app, &seek_url, title, options, 0.0, start);
        let _ = timeout_handle.set_read_timeout(Some(POLL_TIMEOUT));
        match load {
            Ok(status) => {
                *media_session_id = status
                    .entries
                    .first()
                    .map_or(*media_session_id, |entry| entry.media_session_id);
                update(state, session_id, |session| {
                    session.position = start;
                    session.state = SessionState::Buffering;
                });
            }
            Err(error) => tracing::warn!("cast seek failed: {error}"),
        }
    } else {
        match media.seek(
            app.transport_id.as_str(),
            *media_session_id,
            Some(position as f32),
            Some(ResumeState::PlaybackStart),
        ) {
            Ok(_) => update(state, session_id, |session| {
                session.position = position;
                if session.state == SessionState::Paused {
                    session.state = SessionState::Playing;
                }
            }),
            Err(error) => tracing::warn!("cast seek failed: {error}"),
        }
    }
}

fn publish_status(
    state: &Arc<RwLock<Session>>,
    session_id: u64,
    status: &rust_cast::channels::media::Status,
    position_offset: f64,
    media_session_id: i32,
) -> bool {
    let Some(entry) = status.entries.first() else {
        return false;
    };
    let mut ended = false;
    let effective_session = if media_session_id == 0 {
        entry.media_session_id
    } else {
        media_session_id
    };
    let player_state = entry.player_state;
    let idle_reason = entry.idle_reason;
    let current_time = entry.current_time;
    let duration = entry.media.as_ref().and_then(|media| media.duration);
    tracing::debug!(
        "cast status: state={:?} idle={:?} entry_session={} current_session={} offset={:.2}",
        player_state,
        idle_reason,
        entry.media_session_id,
        media_session_id,
        position_offset
    );
    update(state, session_id, |session| {
        match player_state {
            PlayerState::Playing => session.state = SessionState::Playing,
            PlayerState::Buffering => session.state = SessionState::Buffering,
            PlayerState::Paused => session.state = SessionState::Paused,
            PlayerState::Idle => match idle_reason {
                Some(IdleReason::Error) => {
                    session.state = SessionState::Error;
                    session.error = "The receiver could not play this media.".to_string();
                    ended = true;
                }
                Some(IdleReason::Finished) => {
                    session.state = SessionState::Stopped;
                    ended = true;
                }
                Some(IdleReason::Cancelled | IdleReason::Interrupted)
                    if entry.media_session_id == effective_session =>
                {
                    session.state = SessionState::Stopped;
                    ended = true;
                }
                _ => {}
            },
        }
        if let Some(position) = current_time {
            session.position = f64::from(position) + position_offset;
        }
        if let Some(duration) = duration
            && session.duration <= 0.0
        {
            session.duration = f64::from(duration);
        }
    });
    ended
}

fn teardown(
    media: &MediaChan<'_>,
    receiver: &Receiver<'_>,
    app: &rust_cast::channels::receiver::Application,
    media_session_id: i32,
) {
    if media_session_id != 0 {
        let _ = media.stop(app.transport_id.as_str(), media_session_id);
    }
    let _ = receiver.stop_app(app.session_id.as_str());
}

fn is_timeout(error: &rust_cast::errors::Error) -> bool {
    matches!(
        error,
        rust_cast::errors::Error::Io(io)
            if matches!(
                io.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
            )
    )
}

fn update(state: &Arc<RwLock<Session>>, session_id: u64, apply: impl FnOnce(&mut Session)) {
    let Ok(mut guard) = state.write() else {
        return;
    };
    if guard.session_id != session_id {
        return;
    }
    apply(&mut guard);
}

#[allow(clippy::cast_possible_truncation)]
fn publish_volume(
    state: &Arc<RwLock<Session>>,
    session_id: u64,
    volume: &rust_cast::channels::receiver::Volume,
) {
    update(state, session_id, |session| {
        session.volume_supported = true;
        if let Some(level) = volume.level {
            session.volume = (f64::from(level) * 100.0).round().clamp(0.0, 100.0) as i32;
        }
        if let Some(muted) = volume.muted {
            session.muted = muted;
        }
    });
}

fn set_error(state: &Arc<RwLock<Session>>, session_id: u64, message: &str) {
    update(state, session_id, |session| {
        session.state = SessionState::Error;
        session.error = message.to_string();
    });
}

fn is_stale(state: &Arc<RwLock<Session>>, session_id: u64) -> bool {
    state
        .read()
        .map_or(true, |guard| guard.session_id != session_id)
}

fn session_state(state: &Arc<RwLock<Session>>, session_id: u64) -> Option<SessionState> {
    let guard = state.read().ok()?;
    (guard.session_id == session_id).then_some(guard.state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hevc_gate() {
        assert!(!hevc_capable("Chromecast", "Living Room"));
        assert!(!hevc_capable("Chromecast", ""));
        assert!(hevc_capable("Chromecast Ultra", ""));
        assert!(hevc_capable("", "Living Room TV"));
        assert!(hevc_capable("LG webOS", ""));
        assert!(hevc_capable("", "Android TV"));
    }

    /// The receiver echoes our LOAD media object inside every `MEDIA_STATUS`; the
    /// library must be able to parse that echo or the session dies right after
    /// loading (as it did when `metadata.images` was missing).
    #[test]
    fn load_payload_survives_status_parsing() {
        let options = CastOptions {
            duration: Some(6120.5),
            transcode: false,
            exact_seek: false,
            source: std::path::PathBuf::from("/tmp/movie.mp4"),
            tracks: vec![TextTrack {
                id: 1,
                url: "http://192.168.1.6:60020/tok/sub/1.vtt".to_string(),
                name: "English".to_string(),
                language: "en".to_string(),
            }],
            active_track: Some(1),
        };
        let payload = media_payload(
            "http://192.168.1.6:60020/tok/movie.mp4",
            "Movie",
            &options,
            12.5,
        );
        assert_eq!(payload["metadata"]["images"], json!([]));
        assert_eq!(
            payload["tracks"][0]["trackContentId"],
            json!("http://192.168.1.6:60020/tok/sub/1.vtt?start=12.500")
        );
        let echo = json!({
            "type": "MEDIA_STATUS",
            "requestId": 7,
            "status": [{
                "mediaSessionId": 3,
                "playbackRate": 1.0,
                "playerState": "BUFFERING",
                "currentTime": 0.0,
                "supportedMediaCommands": 274_447,
                "volume": {"level": 1.0, "muted": false},
                "media": payload,
                "activeTrackIds": [1],
            }]
        });
        let manager = Arc::new(MessageManager::new(std::io::Cursor::new(Vec::<u8>::new())));
        let media = MediaChannel::new("sender-0", Arc::clone(&manager));
        let message = CastMessage {
            namespace: MEDIA_NAMESPACE.to_string(),
            source: "receiver-0".to_string(),
            destination: "sender-0".to_string(),
            payload: CastMessagePayload::String(echo.to_string()),
        };
        assert!(media.can_handle(&message));
        match media.parse(&message).expect("status parses") {
            MediaResponse::Status(status) => {
                assert_eq!(status.request_id, 7);
                let entry = status.entries.first().expect("one entry");
                assert_eq!(entry.media_session_id, 3);
                assert_eq!(
                    entry.media.as_ref().map(|m| m.content_id.as_str()),
                    Some("http://192.168.1.6:60020/tok/movie.mp4")
                );
            }
            other => panic!("unexpected parse result: {other:?}"),
        }
    }

    #[test]
    fn plan_without_ffprobe() {
        let plan = plan_transcode(Path::new("/nonexistent/file.mkv"), "LG webOS", "TV");
        assert_eq!(plan.vcodec, "libx264");
        assert_eq!(plan.acodec, "copy");
    }
}
