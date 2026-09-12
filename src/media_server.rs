use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::{ConnectInfo, Path, State};
use axum::http::response::Builder;
use axum::http::{HeaderMap, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use axum::serve::ListenerExt;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio::process::Command;
use tokio_util::io::ReaderStream;
use tokio_util::sync::CancellationToken;

use crate::subtitles::{self, SubtitleTrack};

/// Directly served file: byte-range seekable, not converted.
const DLNA_FEATURES: &str =
    "DLNA.ORG_OP=01;DLNA.ORG_CI=0;DLNA.ORG_FLAGS=01700000000000000000000000000000";
/// Live ffmpeg output: no seeking, converted content.
const DLNA_FEATURES_TRANSCODE: &str =
    "DLNA.ORG_OP=00;DLNA.ORG_CI=1;DLNA.ORG_FLAGS=01700000000000000000000000000000";

/// Container ffmpeg muxes a live stream into. Cast receivers want fragmented
/// MP4; DLNA renderers (Samsung in particular) only play a live stream as
/// MPEG-TS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranscodeContainer {
    FragmentedMp4,
    MpegTs,
}

impl TranscodeContainer {
    pub fn content_type(self) -> &'static str {
        match self {
            Self::FragmentedMp4 => "video/mp4",
            Self::MpegTs => "video/mpeg",
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Self::FragmentedMp4 => "mp4",
            Self::MpegTs => "ts",
        }
    }
}
const BUFSIZE_HINT: usize = 256 * 1024;
/// Samsung's DLNA player refuses chunked responses; a live MPEG-TS stream is
/// announced with this nominal length instead and simply ends when ffmpeg does.
const NOMINAL_STREAM_LENGTH: &str = "999999999999";

#[derive(Debug, Clone)]
pub struct TranscodePlan {
    pub vcodec: String,
    pub acodec: String,
    /// Extra gain applied to the soundtrack, in dB. 0 leaves the audio untouched.
    pub gain_db: i32,
    /// Channel count of the source audio (0 = unknown). More than two channels
    /// get a dialogue-forward stereo downmix when boosting.
    pub channels: u32,
    pub container: TranscodeContainer,
}

impl TranscodePlan {
    /// The ffmpeg `-af` chain for a boosted soundtrack, or `None` when the
    /// audio is left alone. The limiter keeps the boosted signal from clipping.
    pub fn audio_filter(&self) -> Option<String> {
        if self.gain_db <= 0 {
            return None;
        }
        let mut chain = Vec::new();
        if self.channels > 2 {
            // Centre (dialogue) slightly above the front pair; surrounds and LFE
            // folded in gently. Channels missing from the source layout are ignored.
            chain.push(
                "pan=stereo|FL=0.5*FL+0.6*FC+0.35*BL+0.35*SL+0.25*LFE|FR=0.5*FR+0.6*FC+0.35*BR+0.35*SR+0.25*LFE"
                    .to_string(),
            );
        }
        chain.push(format!("volume={}dB", self.gain_db));
        chain.push("alimiter=limit=0.95:level=disabled".to_string());
        Some(chain.join(","))
    }
}

/// What one media server instance serves.
pub struct ServeOptions {
    pub file: PathBuf,
    pub token: String,
    pub allow: Vec<IpAddr>,
    pub transcode: Option<TranscodePlan>,
    pub content_type: String,
    /// Text subtitle tracks reachable at `/{token}/sub/{id}.vtt`.
    pub subtitles: Vec<SubtitleTrack>,
    /// Where converted `WebVTT` files are cached for this session.
    pub subtitle_dir: PathBuf,
}

pub struct ServerConfig {
    pub file: PathBuf,
    pub token: String,
    pub allow: Vec<IpAddr>,
    pub transcode: Option<TranscodePlan>,
    pub content_type: String,
    pub subtitles: Vec<SubtitleTrack>,
    pub subtitle_dir: PathBuf,
    /// Serialises subtitle conversions so two requests never race on one file.
    pub convert: tokio::sync::Mutex<()>,
    pub shutdown: CancellationToken,
}

pub struct MediaServer {
    task: tokio::task::JoinHandle<()>,
    shutdown: CancellationToken,
    subtitle_dir: PathBuf,
}

impl MediaServer {
    pub async fn stop(self) {
        self.shutdown.cancel();
        self.task.abort();
        let _ = self.task.await;
        let _ = tokio::fs::remove_dir_all(&self.subtitle_dir).await;
    }
}

pub async fn start(
    bind: IpAddr,
    port: u16,
    options: ServeOptions,
) -> std::io::Result<(MediaServer, u16)> {
    let IpAddr::V4(_) = bind else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "refusing to bind without an explicit IPv4 LAN address",
        ));
    };
    if bind.is_unspecified() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "refusing to bind 0.0.0.0",
        ));
    }
    let addr = SocketAddr::new(bind, port);
    let socket = socket2::Socket::new(socket2::Domain::IPV4, socket2::Type::STREAM, None)?;
    socket.set_reuse_address(true)?;
    let mut bind_error = None;
    for _ in 0..30 {
        match socket.bind(&addr.into()) {
            Ok(()) => {
                bind_error = None;
                break;
            }
            Err(error) => {
                bind_error = Some(error);
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
    }
    if let Some(error) = bind_error {
        return Err(error);
    }
    socket.listen(128)?;
    socket.set_nonblocking(true)?;
    let listener = tokio::net::TcpListener::from_std(socket.into())?;
    let bound_port = listener.local_addr()?.port();

    let shutdown = CancellationToken::new();
    let subtitle_dir = options.subtitle_dir.clone();
    let config = Arc::new(ServerConfig {
        file: options.file,
        token: options.token,
        allow: options.allow,
        transcode: options.transcode,
        content_type: options.content_type,
        subtitles: options.subtitles,
        subtitle_dir: options.subtitle_dir,
        convert: tokio::sync::Mutex::new(()),
        shutdown: shutdown.clone(),
    });
    let app = Router::new()
        .route("/{token}/{*rest}", any(handle))
        .with_state(config);
    let make = app.into_make_service_with_connect_info::<SocketAddr>();

    let listener = listener.tap_io(|stream| {
        let _ = stream.set_nodelay(true);
    });
    let task = tokio::spawn(async move {
        if let Err(error) = axum::serve(listener, make).await {
            tracing::warn!("media server stopped: {error}");
        }
    });
    Ok((
        MediaServer {
            task,
            shutdown,
            subtitle_dir,
        },
        bound_port,
    ))
}

async fn handle(
    State(config): State<Arc<ServerConfig>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path((token, rest)): Path<(String, String)>,
    method: Method,
    headers: HeaderMap,
    axum::extract::RawQuery(query): axum::extract::RawQuery,
) -> Response {
    if !config.allow.is_empty() && !config.allow.iter().any(|ip| *ip == peer.ip()) {
        tracing::warn!("403 {} from {}", rest, peer.ip());
        return forbidden();
    }
    if !constant_time_eq(&token, &config.token) {
        tracing::warn!("403 bad token from {}", peer.ip());
        return forbidden();
    }
    if method == Method::OPTIONS {
        return preflight();
    }
    tracing::info!(
        "{method} {rest}{} from {}{}",
        query.as_deref().map_or(String::new(), |q| format!("?{q}")),
        peer.ip(),
        headers
            .get(header::RANGE)
            .and_then(|value| value.to_str().ok())
            .map_or(String::new(), |range| format!(" range={range}"))
    );
    let start = query
        .as_deref()
        .and_then(parse_start_query)
        .filter(|start| start.is_finite() && *start > 0.0);
    if let Some(name) = rest.strip_prefix("sub/") {
        return match method {
            Method::GET => subtitle_response(&config, name, start).await,
            Method::HEAD => cors(
                Response::builder()
                    .status(StatusCode::OK)
                    .header(header::CONTENT_TYPE, "text/vtt; charset=utf-8"),
            )
            .body(Body::empty())
            .expect("static response"),
            _ => (StatusCode::NOT_IMPLEMENTED, "").into_response(),
        };
    }
    match method {
        Method::HEAD => head_response(&config).await,
        Method::GET => get_response(&config, &headers, start).await,
        _ => (StatusCode::NOT_IMPLEMENTED, "").into_response(),
    }
}

fn parse_start_query(query: &str) -> Option<f64> {
    for pair in query.split('&') {
        let Some((key, value)) = pair.split_once('=') else {
            continue;
        };
        if key == "start" {
            return value.parse().ok();
        }
    }
    None
}

fn forbidden() -> Response {
    Response::builder()
        .status(StatusCode::FORBIDDEN)
        .header(header::CONTENT_LENGTH, "0")
        .body(Body::empty())
        .expect("static response")
}

fn constant_time_eq(left: &str, right: &str) -> bool {
    let left = left.as_bytes();
    let right = right.as_bytes();
    if left.len() != right.len() {
        return false;
    }
    let mut difference = 0_u8;
    for (a, b) in left.iter().zip(right) {
        difference |= a ^ b;
    }
    difference == 0
}

/// Cast receivers fetch subtitle tracks (and, with tracks declared, the media
/// itself) from a browser context, which needs these CORS headers.
fn cors(builder: Builder) -> Builder {
    builder
        .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
        .header(
            header::ACCESS_CONTROL_EXPOSE_HEADERS,
            "Content-Length, Content-Range, Accept-Ranges",
        )
}

fn preflight() -> Response {
    cors(Response::builder().status(StatusCode::NO_CONTENT))
        .header(header::ACCESS_CONTROL_ALLOW_METHODS, "GET, HEAD, OPTIONS")
        .header(
            header::ACCESS_CONTROL_ALLOW_HEADERS,
            "Range, Content-Type, Origin, Accept",
        )
        .header(header::ACCESS_CONTROL_MAX_AGE, "86400")
        .header(header::CONTENT_LENGTH, "0")
        .body(Body::empty())
        .expect("static response")
}

fn dlna_headers(builder: Builder, content_type: &str, transcode: bool) -> Builder {
    cors(builder)
        .header(header::CONTENT_TYPE, content_type)
        .header("transferMode.dlna.org", "Streaming")
        .header(
            "contentFeatures.dlna.org",
            if transcode {
                DLNA_FEATURES_TRANSCODE
            } else {
                DLNA_FEATURES
            },
        )
}

async fn head_response(config: &ServerConfig) -> Response {
    if let Some(plan) = config.transcode.as_ref() {
        return dlna_headers(
            live_stream_builder(plan.container),
            &config.content_type,
            true,
        )
        .body(Body::empty())
        .expect("static response");
    }
    let size = match tokio::fs::metadata(&config.file).await {
        Ok(metadata) => metadata.len(),
        Err(_) => return (StatusCode::NOT_FOUND, "").into_response(),
    };
    dlna_headers(
        Response::builder()
            .status(StatusCode::OK)
            .header(header::ACCEPT_RANGES, "bytes")
            .header(header::CONTENT_LENGTH, size.to_string()),
        &config.content_type,
        false,
    )
    .body(Body::empty())
    .expect("static response")
}

async fn get_response(config: &ServerConfig, headers: &HeaderMap, start: Option<f64>) -> Response {
    if config.transcode.is_some() {
        return transcode_response(config, start);
    }
    file_response(config, headers.get(header::RANGE)).await
}

/// `/{token}/sub/{id}.vtt[?start=S]`: the track as `WebVTT`, converted on first
/// use and re-timed when the receiver plays a stream that began `S` seconds in.
async fn subtitle_response(config: &ServerConfig, name: &str, start: Option<f64>) -> Response {
    let id = name
        .strip_suffix(".vtt")
        .and_then(|id| id.parse::<u32>().ok());
    let track = id.and_then(|id| config.subtitles.iter().find(|track| track.id == id));
    let Some(track) = track else {
        return (StatusCode::NOT_FOUND, "").into_response();
    };
    let path = {
        let _guard = config.convert.lock().await;
        subtitles::ensure_vtt(track, &config.file, &config.subtitle_dir).await
    };
    let path = match path {
        Ok(path) => path,
        Err(error) => {
            tracing::warn!("subtitle track {} unavailable: {error}", track.id);
            return (StatusCode::INTERNAL_SERVER_ERROR, "").into_response();
        }
    };
    let Ok(text) = tokio::fs::read_to_string(&path).await else {
        return (StatusCode::INTERNAL_SERVER_ERROR, "").into_response();
    };
    let body = match start {
        Some(start) => subtitles::shift_vtt(&text, start),
        None => text,
    };
    tracing::info!(
        "subtitle track {} served: {} bytes{}",
        track.id,
        body.len(),
        start.map_or(String::new(), |start| format!(
            ", re-timed from {start:.1}s"
        ))
    );
    cors(
        Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/vtt; charset=utf-8")
            .header(header::CACHE_CONTROL, "no-store")
            .header(header::CONTENT_LENGTH, body.len().to_string()),
    )
    .body(Body::from(body))
    .expect("static response")
}

async fn file_response(config: &ServerConfig, range: Option<&header::HeaderValue>) -> Response {
    let size = match tokio::fs::metadata(&config.file).await {
        Ok(metadata) => metadata.len(),
        Err(_) => return (StatusCode::NOT_FOUND, "").into_response(),
    };
    let mut start = 0_u64;
    let mut end = size.saturating_sub(1);
    let mut partial = false;
    if let Some(value) = range
        && let Some(spec) = value.to_str().ok().and_then(|v| v.strip_prefix("bytes="))
        && !spec.contains(',')
        && let Some((low, high)) = parse_range(spec, size)
    {
        start = low;
        end = high.min(size.saturating_sub(1));
        partial = true;
    }
    if start > end || start >= size {
        return Response::builder()
            .status(StatusCode::RANGE_NOT_SATISFIABLE)
            .header(header::CONTENT_RANGE, format!("bytes */{size}"))
            .header(header::CONTENT_LENGTH, "0")
            .body(Body::empty())
            .expect("static response");
    }
    let length = end - start + 1;
    let mut builder = dlna_headers(
        Response::builder()
            .status(if partial {
                StatusCode::PARTIAL_CONTENT
            } else {
                StatusCode::OK
            })
            .header(header::ACCEPT_RANGES, "bytes"),
        &config.content_type,
        false,
    );
    if partial {
        builder = builder.header(header::CONTENT_RANGE, format!("bytes {start}-{end}/{size}"));
    }
    let Ok(mut file) = tokio::fs::File::open(&config.file).await else {
        return (StatusCode::NOT_FOUND, "").into_response();
    };
    if file.seek(std::io::SeekFrom::Start(start)).await.is_err() {
        return (StatusCode::INTERNAL_SERVER_ERROR, "").into_response();
    }
    let body = Body::from_stream(ReaderStream::with_capacity(file.take(length), BUFSIZE_HINT));
    builder
        .header(header::CONTENT_LENGTH, length.to_string())
        .body(body)
        .expect("static response")
}

fn parse_range(spec: &str, size: u64) -> Option<(u64, u64)> {
    let (first, second) = spec.split_once('-')?;
    if first.is_empty() {
        let suffix: u64 = second.parse().ok()?;
        return Some((size.saturating_sub(suffix), size.saturating_sub(1)));
    }
    let low: u64 = first.parse().ok()?;
    let high: u64 = if second.is_empty() {
        size.saturating_sub(1)
    } else {
        second.parse().ok()?
    };
    Some((low, high))
}

fn transcode_response(config: &ServerConfig, start: Option<f64>) -> Response {
    let Some(plan) = config.transcode.as_ref() else {
        return (StatusCode::INTERNAL_SERVER_ERROR, "").into_response();
    };
    let mut child = match spawn_ffmpeg(config, plan, start) {
        Ok(child) => child,
        Err(error) => {
            tracing::error!("ffmpeg failed to start: {error}");
            return (StatusCode::INTERNAL_SERVER_ERROR, "").into_response();
        }
    };
    let Some(stdout) = child.stdout.take() else {
        return (StatusCode::INTERNAL_SERVER_ERROR, "").into_response();
    };
    let shutdown = config.shutdown.clone();
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_millis(200));
        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    if !matches!(child.try_wait(), Ok(None)) {
                        break;
                    }
                }
                () = shutdown.cancelled() => {
                    let _ = child.kill().await;
                    break;
                }
            }
        }
    });
    let body = Body::from_stream(ReaderStream::with_capacity(stdout, BUFSIZE_HINT));
    dlna_headers(
        live_stream_builder(plan.container),
        plan.container.content_type(),
        true,
    )
    .body(body)
    .expect("static response")
}

/// Response head for a live ffmpeg stream: unseekable, one connection per
/// stream, and for MPEG-TS a nominal length in place of chunked encoding.
fn live_stream_builder(container: TranscodeContainer) -> Builder {
    let builder = Response::builder()
        .status(StatusCode::OK)
        .header(header::ACCEPT_RANGES, "none")
        .header(header::CONNECTION, "close");
    match container {
        TranscodeContainer::MpegTs => builder.header(header::CONTENT_LENGTH, NOMINAL_STREAM_LENGTH),
        TranscodeContainer::FragmentedMp4 => builder,
    }
}

fn spawn_ffmpeg(
    config: &ServerConfig,
    plan: &TranscodePlan,
    start: Option<f64>,
) -> std::io::Result<tokio::process::Child> {
    let vbitrate = std::env::var("OMARCHY_CAST_VBITRATE").unwrap_or_else(|_| "6000".to_string());
    let vbitrate_value: u64 = vbitrate.parse().unwrap_or(6000);
    let mut command = Command::new("ffmpeg");
    command.arg("-nostdin").arg("-loglevel").arg("error");
    if let Some(start) = start {
        command.arg("-ss").arg(format!("{start:.3}"));
    }
    command
        .arg("-i")
        .arg(&config.file)
        .arg("-map")
        .arg("0:v:0")
        .arg("-map")
        .arg("0:a:0?");
    if plan.vcodec == "copy" {
        command.args(["-c:v", "copy"]);
    } else {
        command.args([
            "-vf",
            "scale='min(1920,iw)':-2",
            "-c:v",
            &plan.vcodec,
            "-preset",
            "veryfast",
            "-profile:v",
            "high",
            "-pix_fmt",
            "yuv420p",
            "-b:v",
            &format!("{vbitrate}k"),
            "-maxrate",
            &format!("{vbitrate}k"),
            "-bufsize",
            &format!("{}k", vbitrate_value * 2),
        ]);
    }
    if let Some(filter) = plan.audio_filter() {
        // Boosting means re-encoding, whatever the source codec was.
        command.args(["-af", &filter, "-c:a", "aac", "-b:a", "192k", "-ac", "2"]);
    } else if plan.acodec == "copy" {
        command.args(["-c:a", "copy"]);
    } else {
        command.args(["-c:a", &plan.acodec, "-b:a", "192k", "-ac", "2"]);
    }
    match plan.container {
        TranscodeContainer::FragmentedMp4 => {
            command.args([
                "-movflags",
                "frag_keyframe+empty_moov+default_base_moof",
                "-f",
                "mp4",
            ]);
        }
        TranscodeContainer::MpegTs => {
            // ffmpeg converts H.264/HEVC to Annex B and AAC to ADTS on its own.
            command.args(["-f", "mpegts"]);
        }
    }
    command
        .arg("pipe:1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true);
    tracing::info!(
        "ffmpeg: {}",
        std::iter::once(command.as_std().get_program().to_string_lossy().to_string())
            .chain(
                command
                    .as_std()
                    .get_args()
                    .map(|a| a.to_string_lossy().to_string())
            )
            .collect::<Vec<_>>()
            .join(" ")
    );
    command.spawn()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subtitles::SubtitleSource;

    async fn test_server(
        content: &[u8],
        allow: Vec<IpAddr>,
        subtitles: Vec<(u32, &str)>,
    ) -> (MediaServer, u16, PathBuf) {
        let directory =
            std::env::temp_dir().join(format!("omarchy-cast-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory).expect("temp dir");
        let file = directory.join("sample.mp4");
        std::fs::write(&file, content).expect("write sample");
        let tracks = subtitles
            .into_iter()
            .map(|(id, body)| {
                let path = directory.join(format!("side-{id}.srt"));
                std::fs::write(&path, body).expect("write srt");
                SubtitleTrack {
                    id,
                    label: format!("Track {id}"),
                    language: "en".to_string(),
                    source: SubtitleSource::Sidecar(path),
                }
            })
            .collect();
        let (server, port) = start(
            "127.0.0.1".parse().expect("loopback"),
            0,
            ServeOptions {
                file,
                token: "tok".to_string(),
                allow,
                transcode: None,
                content_type: "video/mp4".to_string(),
                subtitles: tracks,
                subtitle_dir: directory.join("vtt"),
            },
        )
        .await
        .expect("start server");
        (server, port, directory)
    }

    fn sample() -> Vec<u8> {
        (0..=255_u8).cycle().take(1000).collect()
    }

    #[tokio::test]
    async fn serves_files_and_ranges() {
        let content = sample();
        let (server, port, directory) = test_server(&content, Vec::new(), Vec::new()).await;
        let base = format!("http://127.0.0.1:{port}");
        let client = reqwest::Client::new();

        let response = client
            .get(format!("{base}/tok/sample.mp4"))
            .send()
            .await
            .expect("full get");
        assert_eq!(response.status(), 200);
        assert_eq!(response.headers()["content-length"], "1000");
        assert_eq!(response.headers()["accept-ranges"], "bytes");
        assert_eq!(response.headers()["access-control-allow-origin"], "*");
        assert!(response.headers().contains_key("contentfeatures.dlna.org"));
        assert!(response.headers().contains_key("transfermode.dlna.org"));
        assert_eq!(response.bytes().await.expect("body").len(), 1000);

        let response = client
            .get(format!("{base}/tok/sample.mp4"))
            .header(header::RANGE, "bytes=10-19")
            .send()
            .await
            .expect("range get");
        assert_eq!(response.status(), 206);
        assert_eq!(response.headers()["content-range"], "bytes 10-19/1000");
        assert_eq!(
            response.bytes().await.expect("range body").as_ref(),
            &content[10..20]
        );

        let response = client
            .get(format!("{base}/tok/sample.mp4"))
            .header(header::RANGE, "bytes=-5")
            .send()
            .await
            .expect("suffix get");
        assert_eq!(response.status(), 206);
        assert_eq!(response.headers()["content-range"], "bytes 995-999/1000");

        let response = client
            .get(format!("{base}/tok/sample.mp4"))
            .header(header::RANGE, "bytes=2000-")
            .send()
            .await
            .expect("unsatisfiable get");
        assert_eq!(response.status(), 416);
        assert_eq!(response.headers()["content-range"], "bytes */1000");

        let response = client
            .head(format!("{base}/tok/sample.mp4"))
            .send()
            .await
            .expect("head");
        assert_eq!(response.status(), 200);
        assert_eq!(response.headers()["content-length"], "1000");

        let response = client
            .request(Method::OPTIONS, format!("{base}/tok/sample.mp4"))
            .send()
            .await
            .expect("preflight");
        assert_eq!(response.status(), 204);
        assert!(
            response.headers()["access-control-allow-methods"]
                .to_str()
                .expect("ascii")
                .contains("GET")
        );

        let response = client
            .get(format!("{base}/nope/sample.mp4"))
            .send()
            .await
            .expect("bad token");
        assert_eq!(response.status(), 403);

        server.stop().await;
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[tokio::test]
    async fn serves_subtitles_as_vtt() {
        if !crate::daemon::have("ffmpeg") {
            eprintln!("ffmpeg not installed; skipping subtitle conversion test");
            return;
        }
        let srt =
            "1\n00:00:05,000 --> 00:00:08,000\nGone\n\n2\n00:00:20,000 --> 00:00:25,000\nKept\n";
        let (server, port, directory) = test_server(&sample(), Vec::new(), vec![(1, srt)]).await;
        let base = format!("http://127.0.0.1:{port}");
        let client = reqwest::Client::new();

        let response = client
            .get(format!("{base}/tok/sub/1.vtt"))
            .send()
            .await
            .expect("vtt get");
        assert_eq!(response.status(), 200);
        assert_eq!(response.headers()["access-control-allow-origin"], "*");
        assert!(
            response.headers()["content-type"]
                .to_str()
                .expect("ascii")
                .starts_with("text/vtt")
        );
        let text = response.text().await.expect("vtt body");
        assert!(text.starts_with("WEBVTT"));
        assert!(
            text.contains("00:05.000 --> 00:08.000")
                || text.contains("00:00:05.000 --> 00:00:08.000")
        );

        let response = client
            .get(format!("{base}/tok/sub/1.vtt?start=10"))
            .send()
            .await
            .expect("shifted vtt get");
        let text = response.text().await.expect("vtt body");
        assert!(!text.contains("Gone"));
        assert!(text.contains("00:00:10.000 --> 00:00:15.000\nKept"));

        let response = client
            .get(format!("{base}/tok/sub/9.vtt"))
            .send()
            .await
            .expect("unknown track");
        assert_eq!(response.status(), 404);

        server.stop().await;
        assert!(!directory.join("vtt").exists());
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[tokio::test]
    async fn enforces_client_allowlist() {
        let content = sample();
        let allow = vec!["10.9.9.9".parse().expect("ip")];
        let (server, port, directory) = test_server(&content, allow, Vec::new()).await;
        let response = reqwest::Client::new()
            .get(format!("http://127.0.0.1:{port}/tok/sample.mp4"))
            .send()
            .await
            .expect("request");
        assert_eq!(response.status(), 403);
        server.stop().await;
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn boost_filters() {
        let plan = |gain_db, channels| TranscodePlan {
            vcodec: "copy".to_string(),
            acodec: "copy".to_string(),
            gain_db,
            channels,
            container: TranscodeContainer::FragmentedMp4,
        };
        assert_eq!(plan(0, 6).audio_filter(), None);
        assert_eq!(
            plan(6, 2).audio_filter().as_deref(),
            Some("volume=6dB,alimiter=limit=0.95:level=disabled")
        );
        let surround = plan(12, 6).audio_filter().expect("filter");
        assert!(surround.starts_with("pan=stereo|"));
        assert!(surround.ends_with(",volume=12dB,alimiter=limit=0.95:level=disabled"));
    }

    #[test]
    fn ranges() {
        assert_eq!(parse_range("0-99", 1000), Some((0, 99)));
        assert_eq!(parse_range("100-", 1000), Some((100, 999)));
        assert_eq!(parse_range("-100", 1000), Some((900, 999)));
        assert_eq!(parse_range("abc-", 1000), None);
        assert_eq!(parse_range("0-abc", 1000), None);
        assert_eq!(parse_range("5", 1000), None);
    }
}
