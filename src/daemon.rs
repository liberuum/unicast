use std::net::{IpAddr, Ipv4Addr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{Mutex, mpsc};

use crate::airplay;
use crate::cast::{self, ReloadRequest, TextTrack};
use crate::client;
use crate::discovery;
use crate::dlna;
use crate::firewall;
use crate::media_server::{self, MediaServer, ServeOptions, TranscodeContainer, TranscodePlan};
use crate::mpris;
use crate::protocol::{Device, Request, Status, StreamInfo, error_response};
use crate::settings;
use crate::state::{Session, SessionState};
use crate::subtitles::{self, SubtitleTrack};
use crate::util;

pub enum Event {
    SessionEnded { session_id: u64 },
}

/// Seconds to add to the position a receiver reports. Non-zero while the
/// receiver plays an ffmpeg stream that was started part-way through the file
/// (`?start=`), because it then counts from zero.
#[derive(Default)]
pub struct PositionOffset(AtomicU64);

impl PositionOffset {
    pub fn get(&self) -> f64 {
        f64::from_bits(self.0.load(Ordering::Relaxed))
    }

    pub fn set(&self, seconds: f64) {
        self.0.store(seconds.to_bits(), Ordering::Relaxed);
    }
}

#[derive(Default)]
struct Active {
    media: Option<MediaServer>,
    cast: Option<cast::CastSession>,
    dlna: Option<DlnaSession>,
    airplay: Option<AirplaySession>,
    serve: Option<ServeContext>,
}

struct DlnaSession {
    control_url: String,
    rendering_url: Option<String>,
    poller: tokio::task::JoinHandle<()>,
    offset: Arc<PositionOffset>,
}

struct AirplaySession {
    ip: String,
    port: u16,
    poller: tokio::task::JoinHandle<()>,
    offset: Arc<PositionOffset>,
}

/// Everything needed to rebuild the media server for the active session. A
/// live audio-boost change, or a seek inside an ffmpeg stream, re-serves the
/// file and re-points the receiver at the new URL.
#[derive(Clone)]
struct ServeContext {
    device: Device,
    ip: String,
    path: String,
    bind: Ipv4Addr,
    port: u16,
    token: String,
    title: String,
    duration: Option<f64>,
    /// Text subtitle tracks found for the file; served at `/{token}/sub/{id}.vtt`.
    subtitles: Vec<SubtitleTrack>,
    /// Base URL of the stream the receiver currently plays (no `?start=`).
    url: String,
    /// Content type of that stream, as served.
    content_type: String,
    /// Whether that stream goes through ffmpeg (so it is not byte-range seekable).
    transcode: bool,
    exact_seek: bool,
}

impl ServeContext {
    fn with_stream(mut self, url: String, plan: &ServePlan) -> Self {
        self.url = url;
        self.content_type.clone_from(&plan.content_type);
        self.transcode = plan.transcode.is_some();
        self.exact_seek = plan.exact_seek;
        self
    }

    fn mime_for_dlna(&self) -> String {
        if self.transcode {
            self.content_type.clone()
        } else {
            util::mime_for_dlna(&self.path)
        }
    }

    /// Where an ffmpeg stream has to start so that playback resumes at
    /// `position`: exact when video is re-encoded, else the previous keyframe.
    fn stream_start(&self, position: f64) -> f64 {
        if self.exact_seek {
            position
        } else {
            cast::keyframe_at_or_before(Path::new(&self.path), position).unwrap_or(position)
        }
    }

    /// The subtitle tracks as the Cast receiver should see them.
    fn text_tracks(&self) -> Vec<TextTrack> {
        self.subtitles
            .iter()
            .map(|track| TextTrack {
                id: track.id,
                url: format!(
                    "http://{}:{}/{}/sub/{}.vtt",
                    self.bind, self.port, self.token, track.id
                ),
                name: track.label.clone(),
                language: track.language.clone(),
            })
            .collect()
    }
}

/// How a file gets served to a receiver: directly, or through ffmpeg.
struct ServePlan {
    transcode: Option<TranscodePlan>,
    endpoint: String,
    content_type: String,
    exact_seek: bool,
}

impl ServePlan {
    fn mime_for_dlna(&self, path: &str) -> String {
        if self.transcode.is_some() {
            self.content_type.clone()
        } else {
            util::mime_for_dlna(path)
        }
    }
}

fn serve_plan(device: &Device, path: &str, token: &str, boost: i32) -> ServePlan {
    let codec = if device.protocol == "cast" {
        cast::plan_transcode(Path::new(path), &device.model, &device.name)
    } else {
        cast::passthrough_plan(Path::new(path))
    };
    let extension = util::extension(path);
    let container_ok =
        device.protocol != "cast" || matches!(extension.as_str(), "mp4" | "m4v" | "mov");
    let direct = boost == 0 && codec.vcodec == "copy" && codec.acodec == "copy" && container_ok;
    tracing::info!(
        "serve plan for {path}: {}; {}{}",
        codec.reason,
        if direct { "direct" } else { "ffmpeg" },
        if boost > 0 {
            format!(", audio boost +{boost} dB")
        } else {
            String::new()
        }
    );
    if direct {
        ServePlan {
            transcode: None,
            endpoint: format!(
                "{token}/{}",
                percent_encoding::utf8_percent_encode(
                    &util::file_basename(path),
                    percent_encoding::NON_ALPHANUMERIC
                )
            ),
            content_type: util::content_type_for_serve(path, false),
            exact_seek: false,
        }
    } else {
        // Samsung (and most DLNA renderers) only play a live stream as MPEG-TS;
        // Cast and AirPlay receivers want fragmented MP4.
        let container = if device.protocol == "dlna" {
            TranscodeContainer::MpegTs
        } else {
            TranscodeContainer::FragmentedMp4
        };
        ServePlan {
            exact_seek: codec.vcodec != "copy",
            transcode: Some(TranscodePlan {
                vcodec: codec.vcodec,
                acodec: codec.acodec,
                gain_db: boost,
                channels: codec.channels,
                container,
            }),
            endpoint: format!("{token}/stream.{}", container.extension()),
            content_type: container.content_type().to_string(),
        }
    }
}

fn start_url(base: &str, start: f64) -> String {
    if start > 0.0 {
        format!("{base}?start={start:.3}")
    } else {
        base.to_string()
    }
}

#[allow(clippy::cast_possible_truncation)]
fn parse_percent(value: &str) -> Option<i32> {
    let level = value.trim().parse::<f64>().ok()?;
    level
        .is_finite()
        .then(|| level.round().clamp(0.0, 100.0) as i32)
}

fn parse_bool(value: &str) -> Option<bool> {
    match value.trim() {
        "true" | "1" | "on" => Some(true),
        "false" | "0" | "off" => Some(false),
        _ => None,
    }
}

/// The track to enable for a new cast, from the remembered language preference.
fn preferred_subtitle(tracks: &[SubtitleTrack]) -> Option<u32> {
    let preference = settings::subtitle_preference();
    if preference.is_empty() || preference == "off" {
        return None;
    }
    tracks
        .iter()
        .find(|track| track.language == preference)
        .map(|track| track.id)
}

pub struct Daemon {
    state: Arc<RwLock<Session>>,
    active: Mutex<Active>,
    devices: RwLock<(Vec<Device>, Instant)>,
    action: Mutex<()>,
    events: mpsc::UnboundedSender<Event>,
    next_session_id: AtomicU64,
}

impl Daemon {
    fn new(events: mpsc::UnboundedSender<Event>) -> Self {
        Self {
            state: Arc::new(RwLock::new(Session::default())),
            active: Mutex::new(Active::default()),
            devices: RwLock::new((
                Vec::new(),
                Instant::now()
                    .checked_sub(util::DISCOVER_TTL)
                    .unwrap_or_else(Instant::now),
            )),
            action: Mutex::new(()),
            events,
            next_session_id: AtomicU64::new(1),
        }
    }

    async fn dispatch(self: &Arc<Self>, request: &Request) -> Value {
        match request.cmd.as_str() {
            "ping" => json!({"ok": true}),
            "status" => self.status(),
            "deps" => Self::deps(),
            "discover" => json!({"ok": true, "devices": self.discover(request.force).await}),
            "connect" => self.connect(request.ip(), request.file()).await,
            "disconnect" | "stop" => self.disconnect().await,
            "pause" => self.pause().await,
            "resume" => self.resume().await,
            "seek" => self.seek(request.position.unwrap_or(-1.0)).await,
            "set-volume" => self.set_volume(request.value()).await,
            "set-mute" => self.set_mute(request.value()).await,
            "set-boost" => self.set_boost(request.value()).await,
            "set-subtitle" => self.set_subtitle(request.value()).await,
            "add-ip" => self.add_ip(request.ip()).await,
            "remove-ip" => self.remove_ip(request.ip()).await,
            "save-multicast" => self.save_multicast(request.value()),
            "clear-firewall" => {
                firewall::clear().await;
                self.status()
            }
            "shutdown" => {
                let daemon = self.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    daemon.shutdown().await;
                });
                json!({"ok": true})
            }
            other => error_response(&format!("Unknown command: {other}")),
        }
    }

    fn session(&self) -> Session {
        self.state
            .read()
            .map(|guard| guard.clone())
            .unwrap_or_default()
    }

    #[allow(clippy::cast_possible_wrap)]
    fn status(&self) -> Value {
        let session = self.session();
        let active = session.state.is_active();
        let streams = if active {
            vec![StreamInfo {
                device: if session.name.is_empty() {
                    session.ip.clone()
                } else {
                    session.name.clone()
                },
                device_ip: session.ip.clone(),
                protocol: session.protocol.clone(),
                state: session.state.as_str().to_string(),
                position: session.position,
                duration: session.duration,
                paused: session.state == SessionState::Paused,
                volume: session.volume,
                muted: session.muted,
                volume_supported: session.volume_supported,
                boost: session.boost,
                subtitles: session.subtitles.clone(),
                subtitle: session.subtitle.map_or(-1, |id| id as i32),
                subtitles_supported: session.subtitles_supported,
            }]
        } else {
            Vec::new()
        };
        let status = Status {
            ok: true,
            state: if active {
                session.state.as_str().to_string()
            } else {
                "idle".to_string()
            },
            streams,
            last_receiver: settings::last_receiver(),
            multicast_discovery: settings::multicast_discovery(),
            audio_boost: settings::audio_boost(),
            error: if session.state == SessionState::Error {
                session.error.clone()
            } else {
                String::new()
            },
        };
        serde_json::to_value(status).unwrap_or_else(|_| error_response("Failed to encode status"))
    }

    fn deps() -> Value {
        let ffmpeg = have("ffmpeg");
        let ffprobe = have("ffprobe");
        let mut missing = Vec::new();
        if !ffmpeg {
            missing.push("ffmpeg");
        }
        if !ffprobe {
            missing.push("ffprobe");
        }
        json!({
            "ok": true,
            "installed": true,
            "core": true,
            "backends": {"dlna": true, "cast": true, "airplay": true},
            "ffmpeg": ffmpeg,
            "missing": missing,
        })
    }

    async fn discover(&self, force: bool) -> Vec<Device> {
        {
            let Ok(cache) = self.devices.read() else {
                return Vec::new();
            };
            if !force && !cache.0.is_empty() && cache.1.elapsed() < util::DISCOVER_TTL {
                return cache.0.clone();
            }
        }
        let lan = util::lan_ip_for("1.1.1.1").or_else(|| util::lan_ip_for("239.255.255.250"));
        let use_multicast = settings::multicast_discovery();
        let manual = settings::manual_ips();
        let devices = discovery::discover(lan, &manual, use_multicast).await;
        if let Ok(mut cache) = self.devices.write() {
            cache.0.clone_from(&devices);
            cache.1 = Instant::now();
        }
        devices
    }

    #[allow(clippy::too_many_lines)]
    async fn connect(self: &Arc<Self>, ip: &str, file: &str) -> Value {
        let _action = self.action.lock().await;
        let Some(ip) = util::valid_ip(ip) else {
            return error_response("Invalid receiver address");
        };
        let mut path = file.to_string();
        if path.is_empty() {
            path = mpris::current_file().await.unwrap_or_default();
            if path.is_empty() {
                return error_response("Play a file in a media player first, or paste a file path");
            }
        }
        path = util::expand_tilde(&path);
        if !Path::new(&path).is_file() {
            return error_response("File not found");
        }
        let Some(bind) = util::lan_ip_for(&ip) else {
            return error_response(&format!("No route to {ip}"));
        };

        // Publish "connecting" before anything slow (device lookup, ffprobe,
        // receiver handshake) so the panel reflects the click right away.
        self.disconnect_locked().await;
        let session_id = self.next_session_id.fetch_add(1, Ordering::SeqCst);
        if let Ok(mut session) = self.state.write() {
            *session = Session::new_id(session_id);
            session.state = SessionState::Connecting;
            session.ip.clone_from(&ip);
            session.name.clone_from(&ip);
            session.file.clone_from(&path);
        }

        // A receiver we have already seen is used as-is, even from a stale
        // cache: re-running discovery here cost seven silent seconds per cast.
        let mut device = self
            .devices
            .read()
            .ok()
            .and_then(|cache| cache.0.iter().find(|device| device.ip == ip).cloned());
        if device.is_none() {
            device = self
                .discover(false)
                .await
                .into_iter()
                .find(|device| device.ip == ip);
        }
        if device.is_none() {
            if let Some(probed) = dlna::probe_ip(&ip).await {
                if let Ok(mut cache) = self.devices.write() {
                    cache.0.retain(|device| device.ip != ip);
                    cache.0.push(probed.clone());
                }
                device = Some(probed);
            } else {
                self.reset_session();
                return error_response("Unknown device — reload receivers first");
            }
        }
        let device = device.expect("device set above");
        let protocol = device.protocol.clone();
        if let Ok(mut session) = self.state.write()
            && session.session_id == session_id
        {
            if !device.name.is_empty() {
                session.name.clone_from(&device.name);
            }
            session.protocol.clone_from(&protocol);
            // Cast receivers report volume once connected; DLNA needs a
            // RenderingControl service; AirPlay URL playback has no volume API.
            session.volume_supported = protocol == "dlna" && device.rendering_url.is_some();
            session.subtitles_supported = protocol == "cast";
        }

        let token = util::random_token();
        let port = util::cast_port();
        let title = util::file_stem(&path);
        let duration = cast::media_duration(Path::new(&path));
        let boost = settings::audio_boost();
        let subtitle_tracks = subtitles::discover(Path::new(&path));
        let subtitles_supported = protocol == "cast";
        let subtitle = if subtitles_supported {
            preferred_subtitle(&subtitle_tracks)
        } else {
            None
        };
        if !subtitle_tracks.is_empty() {
            tracing::info!(
                "subtitles for {path}: {}",
                subtitle_tracks
                    .iter()
                    .map(|track| format!("{}={}", track.id, track.label))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        if let Ok(mut session) = self.state.write()
            && session.session_id == session_id
        {
            session.duration = duration.unwrap_or(0.0);
            session.boost = boost;
            session.subtitles.clone_from(&subtitle_tracks);
            session.subtitle = subtitle;
        }

        if !firewall::open(&ip).await {
            self.disconnect_locked().await;
            self.reset_session();
            return error_response("Firewall authorization was declined");
        }

        let context = ServeContext {
            device,
            ip: ip.clone(),
            path,
            bind,
            port,
            token,
            title,
            duration,
            subtitles: subtitle_tracks,
            url: String::new(),
            content_type: String::new(),
            transcode: false,
            exact_seek: false,
        };
        let result = match protocol.as_str() {
            "cast" => {
                self.connect_cast(context, boost, subtitle, session_id)
                    .await
            }
            "dlna" => self.connect_dlna(context, boost, session_id).await,
            "airplay" => self.connect_airplay(context, boost, session_id).await,
            other => Err(format!("Unsupported protocol: {other}")),
        };
        if let Err(error) = result {
            self.disconnect_locked().await;
            self.reset_session();
            return error_response(&error);
        }

        if let Ok(mut session) = self.state.write()
            && session.session_id == session_id
            && session.state == SessionState::Connecting
        {
            session.state = SessionState::Buffering;
        }
        let _ = settings::set("lastReceiver", &ip);
        self.status()
    }

    /// Serves the file per `plan`, remembers the server, and returns the
    /// stream's base URL.
    async fn start_media(
        &self,
        context: &ServeContext,
        plan: &ServePlan,
    ) -> Result<String, String> {
        let allow = vec![
            context
                .ip
                .parse::<IpAddr>()
                .map_err(|_| "Invalid receiver address".to_string())?,
        ];
        let server = media_server::start(
            IpAddr::V4(context.bind),
            context.port,
            ServeOptions {
                file: PathBuf::from(&context.path),
                token: context.token.clone(),
                allow,
                transcode: plan.transcode.clone(),
                content_type: plan.content_type.clone(),
                subtitles: context.subtitles.clone(),
                subtitle_dir: util::state_dir().join("subtitles").join(&context.token),
            },
        )
        .await
        .map_err(|error| format!("media server failed to start: {error}"))?;
        self.active.lock().await.media = Some(server.0);
        Ok(format!(
            "http://{}:{}/{}",
            context.bind, context.port, plan.endpoint
        ))
    }

    async fn connect_cast(
        &self,
        context: ServeContext,
        boost: i32,
        subtitle: Option<u32>,
        session_id: u64,
    ) -> Result<(), String> {
        let plan = serve_plan(&context.device, &context.path, &context.token, boost);
        util::kill_pids_on_port(context.port);
        let url = self.start_media(&context, &plan).await?;
        let options = cast::CastOptions {
            duration: context.duration,
            transcode: plan.transcode.is_some(),
            exact_seek: plan.exact_seek,
            source: PathBuf::from(&context.path),
            tracks: context.text_tracks(),
            active_track: subtitle,
        };
        let session = match cast::connect(
            &context.ip,
            &url,
            &context.title,
            session_id,
            options.clone(),
            Arc::clone(&self.state),
            self.events.clone(),
        )
        .await
        {
            Ok(session) => session,
            Err(_first_attempt) => {
                tokio::time::sleep(Duration::from_millis(1500)).await;
                cast::connect(
                    &context.ip,
                    &url,
                    &context.title,
                    session_id,
                    options,
                    Arc::clone(&self.state),
                    self.events.clone(),
                )
                .await?
            }
        };
        let mut active = self.active.lock().await;
        active.cast = Some(session);
        active.serve = Some(context.with_stream(url, &plan));
        Ok(())
    }

    async fn connect_dlna(
        &self,
        context: ServeContext,
        boost: i32,
        session_id: u64,
    ) -> Result<(), String> {
        let control = context
            .device
            .control_url
            .clone()
            .ok_or_else(|| format!("No DLNA control endpoint for {}", context.ip))?;
        let rendering = context.device.rendering_url.clone();
        let plan = serve_plan(&context.device, &context.path, &context.token, boost);
        util::kill_pids_on_port(context.port);
        let url = self.start_media(&context, &plan).await?;
        let mime = plan.mime_for_dlna(&context.path);
        dlna::play(&control, &url, &context.title, &mime)
            .await
            .map_err(|error| format!("Receiver rejected the stream: {error}"))?;
        let offset = Arc::new(PositionOffset::default());
        let poller = tokio::spawn(dlna_poll(
            Arc::clone(&self.state),
            session_id,
            control.clone(),
            rendering.clone(),
            Arc::clone(&offset),
            self.events.clone(),
        ));
        let mut active = self.active.lock().await;
        active.dlna = Some(DlnaSession {
            control_url: control,
            rendering_url: rendering,
            poller,
            offset,
        });
        active.serve = Some(context.with_stream(url, &plan));
        Ok(())
    }

    async fn connect_airplay(
        &self,
        context: ServeContext,
        boost: i32,
        session_id: u64,
    ) -> Result<(), String> {
        let plan = serve_plan(&context.device, &context.path, &context.token, boost);
        util::kill_pids_on_port(context.port);
        let url = self.start_media(&context, &plan).await?;
        let airplay_port = if context.device.port == 0 {
            7000
        } else {
            context.device.port
        };
        airplay::play(&context.ip, airplay_port, &url, 0.0).await?;
        let offset = Arc::new(PositionOffset::default());
        let poller = tokio::spawn(airplay_poll(
            Arc::clone(&self.state),
            session_id,
            context.ip.clone(),
            airplay_port,
            Arc::clone(&offset),
            self.events.clone(),
        ));
        let mut active = self.active.lock().await;
        active.airplay = Some(AirplaySession {
            ip: context.ip.clone(),
            port: airplay_port,
            poller,
            offset,
        });
        active.serve = Some(context.with_stream(url, &plan));
        Ok(())
    }

    async fn disconnect(self: &Arc<Self>) -> Value {
        let _action = self.action.lock().await;
        self.disconnect_locked().await;
        self.reset_session();
        self.status()
    }

    async fn disconnect_locked(&self) {
        let mut active = self.active.lock().await;
        active.serve = None;
        if let Some(dlna) = active.dlna.take() {
            dlna.poller.abort();
            let control = dlna.control_url.clone();
            tokio::spawn(async move {
                dlna::stop(&control).await;
            });
        }
        if let Some(cast) = active.cast.take() {
            cast.shutdown();
        }
        if let Some(airplay) = active.airplay.take() {
            airplay.poller.abort();
            tokio::spawn(async move {
                airplay::stop(&airplay.ip, airplay.port).await;
            });
        }
        if let Some(server) = active.media.take() {
            server.stop().await;
        }
    }

    fn reset_session(&self) {
        if let Ok(mut session) = self.state.write() {
            *session = Session::default();
        }
    }

    async fn pause(self: &Arc<Self>) -> Value {
        let _action = self.action.lock().await;
        let session = self.session();
        if session.protocol == "cast" {
            if let Some(cast) = self.active.lock().await.cast.as_ref() {
                cast.pause();
            }
            if let Ok(mut state) = self.state.write()
                && state.state == SessionState::Playing
            {
                state.state = SessionState::Paused;
            }
        } else if session.protocol == "dlna" {
            let control = self
                .active
                .lock()
                .await
                .dlna
                .as_ref()
                .map(|dlna| dlna.control_url.clone());
            if let Some(control) = control {
                dlna::pause(&control).await;
                if let Ok(mut state) = self.state.write()
                    && state.state == SessionState::Playing
                {
                    state.state = SessionState::Paused;
                }
            }
        } else if session.protocol == "airplay" {
            let target = self
                .active
                .lock()
                .await
                .airplay
                .as_ref()
                .map(|airplay| (airplay.ip.clone(), airplay.port));
            if let Some((ip, port)) = target {
                airplay::pause(&ip, port).await;
                if let Ok(mut state) = self.state.write()
                    && state.state == SessionState::Playing
                {
                    state.state = SessionState::Paused;
                }
            }
        }
        self.status()
    }

    async fn resume(self: &Arc<Self>) -> Value {
        let _action = self.action.lock().await;
        let session = self.session();
        if session.protocol == "cast" {
            if let Some(cast) = self.active.lock().await.cast.as_ref() {
                cast.resume();
            }
            if let Ok(mut state) = self.state.write()
                && state.state == SessionState::Paused
            {
                state.state = SessionState::Playing;
            }
        } else if session.protocol == "dlna" {
            let control = self
                .active
                .lock()
                .await
                .dlna
                .as_ref()
                .map(|dlna| dlna.control_url.clone());
            if let Some(control) = control {
                dlna::resume(&control).await;
                if let Ok(mut state) = self.state.write()
                    && state.state == SessionState::Paused
                {
                    state.state = SessionState::Playing;
                }
            }
        } else if session.protocol == "airplay" {
            let target = self
                .active
                .lock()
                .await
                .airplay
                .as_ref()
                .map(|airplay| (airplay.ip.clone(), airplay.port));
            if let Some((ip, port)) = target {
                airplay::resume(&ip, port).await;
                if let Ok(mut state) = self.state.write()
                    && state.state == SessionState::Paused
                {
                    state.state = SessionState::Playing;
                }
            }
        }
        self.status()
    }

    async fn seek(self: &Arc<Self>, position: f64) -> Value {
        let _action = self.action.lock().await;
        let session = self.session();
        if !session.state.is_active() || !position.is_finite() || position < 0.0 {
            return self.status();
        }
        let position = if session.duration > 0.0 {
            position.min(session.duration)
        } else {
            position
        };
        let transcode = self
            .active
            .lock()
            .await
            .serve
            .as_ref()
            .is_some_and(|serve| serve.transcode);
        if session.protocol == "cast" {
            if let Some(cast) = self.active.lock().await.cast.as_ref() {
                cast.seek(position);
            }
        } else if session.protocol == "dlna" {
            if transcode {
                // An ffmpeg stream cannot be seeked by the renderer; restart it
                // at the wanted position instead.
                if let Err(error) = self.restart_dlna(position).await {
                    tracing::warn!("dlna seek failed: {error}");
                }
            } else {
                let control = self
                    .active
                    .lock()
                    .await
                    .dlna
                    .as_ref()
                    .map(|dlna| dlna.control_url.clone());
                if let Some(control) = control {
                    dlna::seek(&control, position).await;
                    dlna::resume(&control).await;
                }
            }
        } else if session.protocol == "airplay" {
            if transcode {
                if let Err(error) = self.restart_airplay(position).await {
                    tracing::warn!("airplay seek failed: {error}");
                }
            } else {
                let target = self
                    .active
                    .lock()
                    .await
                    .airplay
                    .as_ref()
                    .map(|airplay| (airplay.ip.clone(), airplay.port));
                if let Some((ip, port)) = target {
                    airplay::seek(&ip, port, position).await;
                    airplay::resume(&ip, port).await;
                }
            }
        }
        if let Ok(mut state) = self.state.write() {
            state.position = position;
        }
        self.status()
    }

    async fn set_volume(self: &Arc<Self>, value: &str) -> Value {
        let _action = self.action.lock().await;
        let Some(level) = parse_percent(value) else {
            return error_response("Invalid volume");
        };
        let session = self.session();
        if !session.state.is_active() {
            return error_response("Nothing is casting");
        }
        match session.protocol.as_str() {
            "cast" => {
                let active = self.active.lock().await;
                let Some(cast) = active.cast.as_ref() else {
                    return error_response("No cast session");
                };
                #[allow(clippy::cast_precision_loss)]
                cast.set_volume(level as f32 / 100.0);
            }
            "dlna" => {
                let rendering = self
                    .active
                    .lock()
                    .await
                    .dlna
                    .as_ref()
                    .and_then(|dlna| dlna.rendering_url.clone());
                let Some(rendering) = rendering else {
                    return error_response("This receiver does not expose volume control");
                };
                if !dlna::set_volume(&rendering, level).await {
                    return error_response("The receiver refused the volume change");
                }
            }
            _ => return error_response("Volume control is not available over AirPlay"),
        }
        if let Ok(mut state) = self.state.write() {
            state.volume = level;
        }
        self.status()
    }

    async fn set_mute(self: &Arc<Self>, value: &str) -> Value {
        let _action = self.action.lock().await;
        let Some(muted) = parse_bool(value) else {
            return error_response("Invalid mute value");
        };
        let session = self.session();
        if !session.state.is_active() {
            return error_response("Nothing is casting");
        }
        match session.protocol.as_str() {
            "cast" => {
                let active = self.active.lock().await;
                let Some(cast) = active.cast.as_ref() else {
                    return error_response("No cast session");
                };
                cast.set_mute(muted);
            }
            "dlna" => {
                let rendering = self
                    .active
                    .lock()
                    .await
                    .dlna
                    .as_ref()
                    .and_then(|dlna| dlna.rendering_url.clone());
                let Some(rendering) = rendering else {
                    return error_response("This receiver does not expose volume control");
                };
                if !dlna::set_mute(&rendering, muted).await {
                    return error_response("The receiver refused the mute change");
                }
            }
            _ => return error_response("Volume control is not available over AirPlay"),
        }
        if let Ok(mut state) = self.state.write() {
            state.muted = muted;
        }
        self.status()
    }

    /// Shows one of the session's subtitle tracks (`off` disables them) and
    /// remembers the language for later casts.
    async fn set_subtitle(self: &Arc<Self>, value: &str) -> Value {
        let _action = self.action.lock().await;
        let session = self.session();
        if !session.state.is_active() {
            return error_response("Nothing is casting");
        }
        let track = match value.trim() {
            "" | "off" | "-1" | "none" => None,
            id => match id.parse::<u32>().ok() {
                Some(id) if session.subtitles.iter().any(|track| track.id == id) => Some(id),
                _ => return error_response("Unknown subtitle track"),
            },
        };
        if session.protocol != "cast" {
            return error_response(
                "Subtitle selection is available on Google Cast receivers only for now",
            );
        }
        {
            let active = self.active.lock().await;
            let Some(cast) = active.cast.as_ref() else {
                return error_response("No cast session");
            };
            cast.set_subtitle(track);
        }
        let preference = track
            .and_then(|id| {
                session
                    .subtitles
                    .iter()
                    .find(|candidate| candidate.id == id)
            })
            .map(|chosen| chosen.language.clone())
            .filter(|language| !language.is_empty())
            .unwrap_or_else(|| "off".to_string());
        let _ = settings::set("subtitles", &preference);
        if let Ok(mut state) = self.state.write() {
            state.subtitle = track;
        }
        self.status()
    }

    /// Persists the audio boost and, when something is casting, re-serves the
    /// file with the new gain from the current position.
    async fn set_boost(self: &Arc<Self>, value: &str) -> Value {
        let _action = self.action.lock().await;
        let Some(boost) = settings::parse_boost(value) else {
            return error_response("Invalid boost level");
        };
        let _ = settings::set("audioBoost", &boost.to_string());
        let session = self.session();
        if session.state.is_active()
            && session.boost != boost
            && let Err(error) = self.restream(boost, session.position).await
        {
            return error_response(&format!(
                "Boost saved for the next cast, but it could not be applied now: {error}"
            ));
        }
        self.status()
    }

    /// Rebuilds the media server for the active session with `boost` and points
    /// the receiver at the new stream, resuming at `position`.
    async fn restream(&self, boost: i32, position: f64) -> Result<(), String> {
        let context = {
            let active = self.active.lock().await;
            // The pollers must not mistake the stream swap for the end of playback.
            if let Some(dlna) = active.dlna.as_ref() {
                dlna.poller.abort();
            }
            if let Some(airplay) = active.airplay.as_ref() {
                airplay.poller.abort();
            }
            active
                .serve
                .clone()
                .ok_or_else(|| "No active stream".to_string())?
        };
        let plan = serve_plan(&context.device, &context.path, &context.token, boost);
        if let Some(server) = self.active.lock().await.media.take() {
            server.stop().await;
        }
        let url = self.start_media(&context, &plan).await?;
        let context = context.with_stream(url.clone(), &plan);
        let protocol = context.device.protocol.clone();
        let (transcode, exact_seek) = (context.transcode, context.exact_seek);
        self.active.lock().await.serve = Some(context);
        if let Ok(mut session) = self.state.write() {
            session.boost = boost;
        }
        match protocol.as_str() {
            "cast" => {
                let active = self.active.lock().await;
                let cast = active
                    .cast
                    .as_ref()
                    .ok_or_else(|| "No cast session".to_string())?;
                cast.reload(ReloadRequest {
                    url,
                    transcode,
                    exact_seek,
                    position,
                });
                Ok(())
            }
            "dlna" => self.restart_dlna(position).await,
            "airplay" => self.restart_airplay(position).await,
            other => Err(format!("Unsupported protocol: {other}")),
        }
    }

    /// Re-points the DLNA renderer at the current stream so playback resumes at
    /// `position`, and restarts its poller.
    async fn restart_dlna(&self, position: f64) -> Result<(), String> {
        let (context, dlna_session) = {
            let mut active = self.active.lock().await;
            let context = active
                .serve
                .clone()
                .ok_or_else(|| "No active stream".to_string())?;
            let dlna_session = active
                .dlna
                .take()
                .ok_or_else(|| "No DLNA session".to_string())?;
            (context, dlna_session)
        };
        dlna_session.poller.abort();
        let start = if context.transcode {
            context.stream_start(position)
        } else {
            0.0
        };
        let url = start_url(&context.url, start);
        dlna::stop(&dlna_session.control_url).await;
        let result = dlna::play(
            &dlna_session.control_url,
            &url,
            &context.title,
            &context.mime_for_dlna(),
        )
        .await;
        if result.is_ok() && !context.transcode && position > 0.0 {
            // A directly served file is range-seekable, but a Seek sent before
            // the renderer actually plays is ignored: wait for PLAYING first.
            for _ in 0..12 {
                tokio::time::sleep(Duration::from_millis(500)).await;
                if dlna::status(&dlna_session.control_url).await == "PLAYING" {
                    break;
                }
            }
            dlna::seek(&dlna_session.control_url, position).await;
            dlna::resume(&dlna_session.control_url).await;
        }
        dlna_session.offset.set(start);
        let session_id = self.session().session_id;
        let poller = tokio::spawn(dlna_poll(
            Arc::clone(&self.state),
            session_id,
            dlna_session.control_url.clone(),
            dlna_session.rendering_url.clone(),
            Arc::clone(&dlna_session.offset),
            self.events.clone(),
        ));
        self.active.lock().await.dlna = Some(DlnaSession {
            poller,
            ..dlna_session
        });
        if let Ok(mut session) = self.state.write() {
            session.position = position;
            session.state = SessionState::Buffering;
        }
        result.map_err(|error| format!("Receiver rejected the stream: {error}"))
    }

    /// `AirPlay` counterpart of [`Self::restart_dlna`].
    async fn restart_airplay(&self, position: f64) -> Result<(), String> {
        let (context, airplay_session) = {
            let mut active = self.active.lock().await;
            let context = active
                .serve
                .clone()
                .ok_or_else(|| "No active stream".to_string())?;
            let airplay_session = active
                .airplay
                .take()
                .ok_or_else(|| "No AirPlay session".to_string())?;
            (context, airplay_session)
        };
        airplay_session.poller.abort();
        let start = if context.transcode {
            context.stream_start(position)
        } else {
            0.0
        };
        let url = start_url(&context.url, start);
        // Legacy AirPlay takes the start as a fraction of the duration; only
        // meaningful for a directly served (seekable) file.
        let fraction = match context.duration {
            Some(duration) if !context.transcode && duration > 0.0 => {
                (position / duration).clamp(0.0, 1.0)
            }
            _ => 0.0,
        };
        airplay::stop(&airplay_session.ip, airplay_session.port).await;
        let result = airplay::play(&airplay_session.ip, airplay_session.port, &url, fraction).await;
        airplay_session.offset.set(start);
        let session_id = self.session().session_id;
        let poller = tokio::spawn(airplay_poll(
            Arc::clone(&self.state),
            session_id,
            airplay_session.ip.clone(),
            airplay_session.port,
            Arc::clone(&airplay_session.offset),
            self.events.clone(),
        ));
        self.active.lock().await.airplay = Some(AirplaySession {
            poller,
            ..airplay_session
        });
        if let Ok(mut session) = self.state.write() {
            session.position = position;
            session.state = SessionState::Buffering;
        }
        result
    }

    async fn add_ip(&self, ip: &str) -> Value {
        let Some(ip) = util::valid_ip(ip) else {
            return error_response("Invalid address");
        };
        let mut ips = settings::manual_ips();
        if !ips.contains(&ip) {
            ips.push(ip);
            settings::write_manual_ips(&ips);
        }
        let devices = self.discover(true).await;
        json!({"ok": true, "devices": devices})
    }

    async fn remove_ip(&self, ip: &str) -> Value {
        let Some(ip) = util::valid_ip(ip) else {
            return error_response("Invalid address");
        };
        let ips: Vec<String> = settings::manual_ips()
            .into_iter()
            .filter(|entry| *entry != ip)
            .collect();
        settings::write_manual_ips(&ips);
        let devices = self.discover(true).await;
        json!({"ok": true, "devices": devices})
    }

    fn save_multicast(&self, value: &str) -> Value {
        if value != "true" && value != "false" {
            return error_response("Invalid setting");
        }
        let _ = settings::set("multicastDiscovery", value);
        self.status()
    }

    async fn auto_stop(&self, session_id: u64) {
        let _action = self.action.lock().await;
        {
            let Ok(session) = self.state.read() else {
                return;
            };
            if session.session_id != session_id
                || !matches!(session.state, SessionState::Stopped | SessionState::Error)
            {
                return;
            }
            tracing::debug!(
                "auto-stop session {} in state {}",
                session_id,
                session.state.as_str()
            );
        }
        self.disconnect_locked().await;
        self.reset_session();
    }

    async fn shutdown(&self) {
        self.disconnect_locked().await;
        let _ = std::fs::remove_file(util::socket_path());
        std::process::exit(0);
    }
}

async fn dlna_poll(
    state: Arc<RwLock<Session>>,
    session_id: u64,
    control: String,
    rendering: Option<String>,
    offset: Arc<PositionOffset>,
    events: mpsc::UnboundedSender<Event>,
) {
    let mut ticker = tokio::time::interval(Duration::from_secs(1));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut seen_active = false;
    let mut ticks = 0_u32;
    loop {
        ticker.tick().await;
        {
            let Ok(guard) = state.read() else {
                return;
            };
            if guard.session_id != session_id {
                return;
            }
        }
        let transport = dlna::status(&control).await;
        let (duration, position) = dlna::position_info(&control).await;
        // Volume is cheap to read but rarely changes; every third tick is plenty.
        let volume = match rendering.as_deref() {
            Some(rendering) if ticks % 3 == 0 => (
                dlna::get_volume(rendering).await,
                dlna::get_mute(rendering).await,
            ),
            _ => (None, None),
        };
        ticks = ticks.wrapping_add(1);
        let mut ended = false;
        {
            let Ok(mut session) = state.write() else {
                return;
            };
            if session.session_id != session_id {
                return;
            }
            match transport.as_str() {
                "PLAYING" => {
                    session.state = SessionState::Playing;
                    seen_active = true;
                }
                "PAUSED_PLAYBACK" => {
                    session.state = SessionState::Paused;
                    seen_active = true;
                }
                "TRANSITIONING" => session.state = SessionState::Buffering,
                "STOPPED" | "NO_MEDIA_PRESENT" if seen_active => {
                    session.state = SessionState::Stopped;
                    ended = true;
                }
                _ => {}
            }
            // An ffmpeg stream has no known length on the renderer side; keep
            // the probed duration then.
            if let Some(duration) = duration
                && duration > 0.0
            {
                session.duration = duration;
            }
            if let Some(position) = position {
                session.position = position + offset.get();
            }
            if let Some(level) = volume.0 {
                session.volume = level;
            }
            if let Some(muted) = volume.1 {
                session.muted = muted;
            }
        }
        if ended {
            let _ = events.send(Event::SessionEnded { session_id });
            return;
        }
    }
}

async fn airplay_poll(
    state: Arc<RwLock<Session>>,
    session_id: u64,
    ip: String,
    port: u16,
    offset: Arc<PositionOffset>,
    events: mpsc::UnboundedSender<Event>,
) {
    let mut ticker = tokio::time::interval(Duration::from_secs(1));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut seen_active = false;
    loop {
        ticker.tick().await;
        {
            let Ok(guard) = state.read() else {
                return;
            };
            if guard.session_id != session_id {
                return;
            }
        }
        let info = airplay::playback_info(&ip, port).await;
        let mut ended = false;
        {
            let Ok(mut session) = state.write() else {
                return;
            };
            if session.session_id != session_id {
                return;
            }
            match info {
                Some(info) => {
                    seen_active = true;
                    session.state = if info.playing {
                        SessionState::Playing
                    } else {
                        SessionState::Paused
                    };
                    session.position = info.position + offset.get();
                    if info.duration > 0.0 && offset.get() == 0.0 {
                        session.duration = info.duration;
                    }
                }
                None if seen_active => {
                    session.state = SessionState::Stopped;
                    ended = true;
                }
                None => {}
            }
        }
        if ended {
            let _ = events.send(Event::SessionEnded { session_id });
            return;
        }
    }
}

async fn handle_connection(daemon: Arc<Daemon>, stream: UnixStream) {
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();
    let response = match lines.next_line().await {
        Ok(Some(line)) => match serde_json::from_str::<Request>(&line) {
            Ok(request) => daemon.dispatch(&request).await,
            Err(_) => error_response("bad request"),
        },
        _ => return,
    };
    let mut text = serde_json::to_string(&response)
        .unwrap_or_else(|_| "{\"ok\": false, \"error\": \"internal error\"}".to_string());
    text.push('\n');
    let _ = writer.write_all(text.as_bytes()).await;
}

async fn run_socket_server(daemon: Arc<Daemon>) -> std::io::Result<()> {
    let listener = UnixListener::bind(util::socket_path())?;
    let _ = std::fs::set_permissions(
        util::socket_path(),
        std::os::unix::fs::PermissionsExt::from_mode(0o600),
    );
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let daemon = Arc::clone(&daemon);
                tokio::spawn(handle_connection(daemon, stream));
            }
            Err(error) => {
                tracing::warn!("socket accept failed: {error}");
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
}

pub(crate) fn have(name: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths).any(|directory| directory.join(name).is_file())
    })
}

pub fn main() {
    use tracing_subscriber::EnvFilter;
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_env("OMARCHY_CAST_LOG").unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();
    util::ensure_dirs();
    let Some(_lock) = util::acquire_daemon_lock() else {
        tracing::debug!("another daemon already holds the lock");
        return;
    };
    if client::ping_sync(Duration::from_millis(1000)) {
        tracing::debug!("another daemon is already running");
        return;
    }
    let _ = std::fs::remove_file(util::socket_path());
    let (events_tx, mut events_rx) = mpsc::unbounded_channel();
    let daemon = Arc::new(Daemon::new(events_tx));
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            tracing::error!("could not start async runtime: {error}");
            return;
        }
    };
    runtime.block_on(async move {
        let handler = Arc::clone(&daemon);
        tokio::spawn(async move {
            while let Some(event) = events_rx.recv().await {
                match event {
                    Event::SessionEnded { session_id } => {
                        handler.auto_stop(session_id).await;
                    }
                }
            }
        });
        if let Err(error) = run_socket_server(daemon).await {
            tracing::error!("daemon stopped: {error}");
        }
    });
}
