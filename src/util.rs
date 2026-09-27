use std::env;
use std::fmt::Write;
use std::fs;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, ToSocketAddrs, UdpSocket};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::io::AsRawFd;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};

pub const DEFAULT_CAST_PORT: u16 = 60020;
pub const DISCOVER_TTL: std::time::Duration = std::time::Duration::from_secs(20);

/// Reads an HTTP response body, giving up once it grows past `max` bytes.
/// Receivers are untrusted LAN devices: a hostile or broken one must not be
/// able to make the daemon buffer an endless reply. Content-Length is optional
/// (chunked replies), so the cap is enforced while reading as well.
pub async fn read_body_capped(mut response: reqwest::Response, max: usize) -> Option<Vec<u8>> {
    if response
        .content_length()
        .is_some_and(|length| length > max as u64)
    {
        return None;
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.ok()? {
        if body.len() + chunk.len() > max {
            return None;
        }
        body.extend_from_slice(&chunk);
    }
    Some(body)
}

pub fn home() -> PathBuf {
    env::var_os("HOME").map_or_else(|| PathBuf::from("."), PathBuf::from)
}

pub fn config_dir() -> PathBuf {
    env::var_os("XDG_CONFIG_HOME")
        .map_or_else(|| home().join(".config"), PathBuf::from)
        .join("omarchy")
        .join("cast")
}

pub fn systemd_user_dir() -> PathBuf {
    env::var_os("XDG_CONFIG_HOME")
        .map_or_else(|| home().join(".config"), PathBuf::from)
        .join("systemd")
        .join("user")
}

#[allow(unsafe_code)]
pub fn runtime_root() -> PathBuf {
    env::var_os("XDG_RUNTIME_DIR").map_or_else(
        // SAFETY: getuid takes no arguments and cannot fail.
        || PathBuf::from(format!("/run/user/{}", unsafe { libc::getuid() })),
        PathBuf::from,
    )
}

pub fn state_dir() -> PathBuf {
    runtime_root().join("universal-cast")
}

pub fn socket_path() -> PathBuf {
    state_dir().join("castd.sock")
}

pub fn settings_file() -> PathBuf {
    config_dir().join("settings.json")
}

pub fn manual_ips_file() -> PathBuf {
    config_dir().join("manual-ips")
}

pub fn firewall_ledger() -> PathBuf {
    config_dir().join("firewall-rules")
}

pub fn cast_port() -> u16 {
    env::var("OMARCHY_CAST_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_CAST_PORT)
}

pub fn ensure_dirs() {
    let _ = fs::create_dir_all(config_dir());
    let _ = fs::create_dir_all(state_dir());
    for dir in [config_dir(), state_dir()] {
        let _ = fs::set_permissions(&dir, fs::Permissions::from_mode(0o700));
    }
}

#[allow(unsafe_code)]
pub fn acquire_daemon_lock() -> Option<fs::File> {
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(state_dir().join("castd.lock"))
        .ok()?;
    // SAFETY: flock is called with a valid fd; the lock is held for the
    // lifetime of the returned File and released by the kernel on exit.
    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    (result == 0).then_some(file)
}

pub fn valid_ip(value: &str) -> Option<String> {
    value.trim().parse::<IpAddr>().ok().map(|ip| ip.to_string())
}

/// Private (RFC 1918) or link-local IPv4, never broadcast: the only addresses
/// the daemon talks to, serves media to, or opens the firewall for.
pub fn lan_ipv4(ip: Ipv4Addr) -> bool {
    (ip.is_private() || ip.is_link_local()) && !ip.is_broadcast()
}

/// A receiver address the daemon will accept: a LAN IPv4 in canonical form.
pub fn valid_receiver_ip(value: &str) -> Option<String> {
    let ip = value.trim().parse::<Ipv4Addr>().ok()?;
    lan_ipv4(ip).then(|| ip.to_string())
}

pub fn lan_ip_for(ip: &str) -> Option<Ipv4Addr> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect((ip, 9)).ok()?;
    match socket.local_addr().ok()? {
        SocketAddr::V4(addr) if !addr.ip().is_unspecified() => Some(*addr.ip()),
        _ => None,
    }
}

pub fn resolve_ip(host: &str) -> String {
    if host.parse::<IpAddr>().is_ok() {
        return host.to_string();
    }
    let with_port = (host, 0u16);
    with_port
        .to_socket_addrs()
        .ok()
        .and_then(|mut addrs| addrs.next())
        .map_or_else(|| host.to_string(), |addr| addr.ip().to_string())
}

/// Longest plausible media position, in seconds (about 11 days).
pub const MAX_MEDIA_SECONDS: f64 = 1_000_000.0;

/// A position or duration reported by a receiver, or 0 when it is not a
/// usable number (NaN, infinite, negative or absurdly large).
pub fn media_seconds(value: f64) -> f64 {
    if value.is_finite() && (0.0..=MAX_MEDIA_SECONDS).contains(&value) {
        value
    } else {
        0.0
    }
}

pub fn hms(value: &str) -> f64 {
    let parts: Vec<&str> = value.trim().split(':').collect();
    if parts.is_empty() || parts.len() > 3 {
        return 0.0;
    }
    let mut seconds = 0.0_f64;
    for part in parts {
        match part.parse::<f64>() {
            Ok(number) => seconds = seconds * 60.0 + number,
            Err(_) => return 0.0,
        }
    }
    media_seconds(seconds)
}

pub fn mime_for_dlna(path: &str) -> String {
    const EXT_MIME: &[(&str, &str)] = &[
        ("mkv", "video/x-matroska"),
        ("mp4", "video/mp4"),
        ("m4v", "video/mp4"),
        ("avi", "video/x-msvideo"),
        ("mov", "video/quicktime"),
        ("webm", "video/webm"),
        ("ts", "video/mp2t"),
        ("m2ts", "video/mp2t"),
        ("mpg", "video/mpeg"),
        ("mp3", "audio/mpeg"),
        ("flac", "audio/flac"),
        ("m4a", "audio/mp4"),
        ("aac", "audio/aac"),
        ("wav", "audio/wav"),
    ];
    let ext = extension(path);
    EXT_MIME
        .iter()
        .find(|(name, _)| *name == ext)
        .map_or_else(|| "video/mp4".to_string(), |(_, mime)| (*mime).to_string())
}

/// Whether `path` has one of the audio/video extensions the media server knows.
pub fn is_media_extension(path: &str) -> bool {
    let ext = extension(path);
    MEDIA_TYPES.iter().any(|(name, _)| *name == ext)
}

const MEDIA_TYPES: &[(&str, &str)] = &[
    ("mkv", "video/x-matroska"),
    ("mp4", "video/mp4"),
    ("m4v", "video/mp4"),
    ("avi", "video/x-msvideo"),
    ("mov", "video/quicktime"),
    ("webm", "video/webm"),
    ("ts", "video/mp2t"),
    ("m2ts", "video/mp2t"),
    ("mpg", "video/mpeg"),
    ("mpeg", "video/mpeg"),
    ("wmv", "video/x-ms-wmv"),
    ("flv", "video/x-flv"),
    ("mp3", "audio/mpeg"),
    ("flac", "audio/flac"),
    ("m4a", "audio/mp4"),
    ("aac", "audio/aac"),
    ("ogg", "audio/ogg"),
    ("wav", "audio/wav"),
    ("opus", "audio/opus"),
];

pub fn content_type_for_serve(path: &str, transcode: bool) -> String {
    const EXT_TYPES: &[(&str, &str)] = MEDIA_TYPES;
    if transcode {
        return "video/mp4".to_string();
    }
    let ext = extension(path);
    EXT_TYPES.iter().find(|(name, _)| *name == ext).map_or_else(
        || "application/octet-stream".to_string(),
        |(_, mime)| (*mime).to_string(),
    )
}

pub fn file_basename(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map_or_else(String::new, |name| name.to_string_lossy().to_string())
}

pub fn file_stem(path: &str) -> String {
    Path::new(path)
        .file_stem()
        .map_or_else(String::new, |name| name.to_string_lossy().to_string())
}

pub fn extension(path: &str) -> String {
    Path::new(path)
        .extension()
        .map_or_else(String::new, |ext| ext.to_string_lossy().to_lowercase())
}

/// The media file of a cast, opened once when the cast starts. Everything that
/// sends its bytes to the receiver reads this open file through
/// [`PinnedFile::data_path`], never the original path again: if the path is
/// swapped mid-cast (say, for a symlink in a shared folder), the receiver
/// still gets the file the user chose, or nothing.
#[derive(Debug)]
pub struct PinnedFile {
    file: fs::File,
}

impl PinnedFile {
    pub fn open(path: &Path) -> std::io::Result<Self> {
        Self::open_with(path, 0)
    }

    /// Like `open`, but refuses a symlink at `path` itself.
    pub fn open_no_follow(path: &Path) -> std::io::Result<Self> {
        Self::open_with(path, libc::O_NOFOLLOW)
    }

    /// `O_NONBLOCK` so that opening a FIFO (or a device) returns at once
    /// instead of blocking until a writer appears; the regular-file check on
    /// the opened descriptor then refuses it.
    fn open_with(path: &Path, flags: libc::c_int) -> std::io::Result<Self> {
        use std::os::unix::fs::OpenOptionsExt;
        let file = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK | flags)
            .open(path)?;
        if !file.metadata()?.is_file() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "not a regular file",
            ));
        }
        Ok(Self { file })
    }

    /// `/proc/<daemon pid>/fd/<n>`: opens the pinned inode, with its own file
    /// offset, from this process and from the ffmpeg/ffprobe it starts.
    pub fn data_path(&self) -> PathBuf {
        PathBuf::from(format!(
            "/proc/{}/fd/{}",
            std::process::id(),
            self.file.as_raw_fd()
        ))
    }
}

/// ffmpeg/ffprobe input arguments for a local file: the `file:` prefix and
/// the protocol whitelist stop a name or a playlist-shaped file from being
/// opened as anything but a local file (no `http:`, `concat:`, `-option`).
pub fn ffmpeg_input(path: &Path) -> Vec<String> {
    vec![
        "-protocol_whitelist".to_string(),
        "file".to_string(),
        "-i".to_string(),
        format!("file:{}", path.to_string_lossy()),
    ]
}

/// Longest an ffprobe run may take; a hung probe must not hold the daemon.
const FFPROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);
/// Most ffprobe output kept; real outputs are a few KiB.
const FFPROBE_MAX_OUTPUT: u64 = 4 * 1024 * 1024;

/// Run ffprobe on a local file with a deadline and a stdout cap. `None` on
/// spawn failure or timeout (the process is killed).
pub fn ffprobe(args: &[&str], path: &Path) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut child = std::process::Command::new("ffprobe")
        .args(args)
        .args(ffmpeg_input(path))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stdout.take(FFPROBE_MAX_OUTPUT).read_to_end(&mut bytes);
        bytes
    });
    let deadline = std::time::Instant::now() + FFPROBE_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    reader.join().ok()
}

pub fn expand_tilde(path: &str) -> String {
    path.strip_prefix("~/").map_or_else(
        || path.to_string(),
        |rest| home().join(rest).to_string_lossy().to_string(),
    )
}

pub fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

pub fn random_token() -> String {
    let bytes: [u8; 24] = rand::random();
    let mut token = String::with_capacity(48);
    for byte in bytes {
        let _ = write!(token, "{byte:02x}");
    }
    token
}

#[allow(unsafe_code)]
pub fn detached_command(program: &Path) -> std::process::Command {
    let mut command = std::process::Command::new(program);
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    // SAFETY: pre_exec runs in the forked child before exec; setsid is
    // async-signal-safe and only detaches the session.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    command
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hms_parses() {
        assert!((hms("01:02:03.5") - 3723.5).abs() < f64::EPSILON);
        assert!((hms("12:30") - 750.0).abs() < f64::EPSILON);
        assert!((hms("5") - 5.0).abs() < f64::EPSILON);
        assert!((hms("bogus") - 0.0).abs() < f64::EPSILON);
        for bogus in ["inf", "NaN", "-5", "1e308", "99999999:00:00"] {
            assert!(hms(bogus).abs() < f64::EPSILON, "{bogus}");
        }
    }

    #[test]
    fn receiver_ips_are_lan_ipv4_only() {
        assert_eq!(
            valid_receiver_ip(" 192.168.1.8 "),
            Some("192.168.1.8".into())
        );
        assert_eq!(valid_receiver_ip("10.0.0.2"), Some("10.0.0.2".into()));
        assert_eq!(valid_receiver_ip("172.16.4.1"), Some("172.16.4.1".into()));
        assert_eq!(valid_receiver_ip("169.254.3.3"), Some("169.254.3.3".into()));
        for bad in [
            "8.8.8.8",
            "127.0.0.1",
            "0.0.0.0",
            "255.255.255.255",
            "224.0.0.251",
            "fe80::1",
            "::1",
            "any",
            "10.0.0.0/8",
            "192.168.1.8 port 22",
            "",
        ] {
            assert_eq!(valid_receiver_ip(bad), None, "{bad}");
        }
    }

    #[test]
    fn pinned_file_survives_a_path_swap() {
        let dir = std::env::temp_dir().join(format!("unicast-pin-{}", random_token()));
        fs::create_dir_all(&dir).expect("dir");
        let media = dir.join("movie.mkv");
        fs::write(&media, b"the chosen movie").expect("write");
        let pinned = PinnedFile::open(&media).expect("pin");
        fs::remove_file(&media).expect("remove");
        fs::write(&media, b"a swapped-in secret").expect("swap");
        assert_eq!(
            fs::read(pinned.data_path()).expect("read"),
            b"the chosen movie"
        );
        assert!(PinnedFile::open(&dir).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    #[allow(unsafe_code)]
    fn pinned_file_refuses_fifos_and_links_without_blocking() {
        use std::os::unix::ffi::OsStrExt;
        let dir = std::env::temp_dir().join(format!("unicast-fifo-{}", random_token()));
        fs::create_dir_all(&dir).expect("dir");
        let fifo = dir.join("movie.mp4");
        let name = std::ffi::CString::new(fifo.as_os_str().as_bytes()).expect("path");
        // SAFETY: mkfifo gets a valid NUL-terminated path.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        // With no writer, a blocking open would hang this test forever.
        assert!(PinnedFile::open(&fifo).is_err());
        let real = dir.join("real.srt");
        fs::write(&real, b"1\n").expect("write");
        let link = dir.join("link.srt");
        std::os::unix::fs::symlink(&real, &link).expect("link");
        assert!(PinnedFile::open_no_follow(&link).is_err());
        assert!(PinnedFile::open_no_follow(&real).is_ok());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn tilde_expands() {
        assert!(expand_tilde("~/Videos/a.mkv").ends_with("Videos/a.mkv"));
    }

    #[test]
    fn mime_tables() {
        assert_eq!(
            content_type_for_serve("/x/a.MKV", false),
            "video/x-matroska"
        );
        assert_eq!(
            content_type_for_serve("/x/a.bin", false),
            "application/octet-stream"
        );
        assert_eq!(content_type_for_serve("/x/a.mkv", true), "video/mp4");
        assert_eq!(mime_for_dlna("/x/a.flac"), "audio/flac");
    }
}
