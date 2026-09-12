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
    seconds
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

pub fn content_type_for_serve(path: &str, transcode: bool) -> String {
    const EXT_TYPES: &[(&str, &str)] = &[
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
pub fn kill_pids_on_port(port: u16) {
    let output = std::process::Command::new("ss")
        .args(["-ltnp", &format!("sport = :{port}")])
        .output();
    let Ok(output) = output else { return };
    let text = String::from_utf8_lossy(&output.stdout);
    for (name, pid) in processes_from_ss(&text) {
        if !is_our_renderer_process(&name) {
            continue;
        }
        // SAFETY: pid comes from `ss` output for our port; killing a stale
        // process here is the intended self-heal. ESRCH is ignored.
        unsafe {
            libc::kill(pid, libc::SIGKILL);
        }
    }
}

fn is_our_renderer_process(name: &str) -> bool {
    name.contains("omarchy") || name.contains("python")
}

fn processes_from_ss(text: &str) -> Vec<(String, i32)> {
    let mut found = Vec::new();
    let mut rest = text;
    while let Some(index) = rest.find("((\"") {
        rest = &rest[index + 3..];
        let Some((name, tail)) = rest.split_once('"') else {
            break;
        };
        if let Some(pid_start) = tail.find("pid=") {
            let digits: String = tail[pid_start + 4..]
                .chars()
                .take_while(char::is_ascii_digit)
                .collect();
            if let Ok(pid) = digits.parse() {
                found.push((name.to_string(), pid));
            }
        }
    }
    found
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

    #[test]
    fn ss_pid_parsing() {
        let sample = "LISTEN 0 128 192.168.1.6:60020 0.0.0.0:* users:((\"python3\",pid=1234,fd=3))";
        assert_eq!(
            processes_from_ss(sample),
            vec![("python3".to_string(), 1234)]
        );
        assert!(is_our_renderer_process("omarchy-castd"));
        assert!(is_our_renderer_process("python3"));
        assert!(!is_our_renderer_process("nginx"));
    }
}
