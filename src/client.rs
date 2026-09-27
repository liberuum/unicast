use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use serde_json::{Value, json};

use crate::util;

const UNIT_NAME: &str = "omarchy-castd.service";

/// First line of every unit the client writes. The client rewrites, and the
/// uninstall script removes, only a unit that carries it (or a byte-exact
/// unit from a release before the marker, see `legacy_units`).
const UNIT_MARKER: &str =
    "# Written by UniCast (universal-cast); omarchy-cast-uninstall removes it.";

const UNIT_HEAD: &str = "# Written by UniCast (universal-cast); omarchy-cast-uninstall removes it.
[Unit]
Description=omarchy-cast media daemon

[Service]
Type=simple
";

/// Limits that hold in a user unit without disabling the daemon's one
/// privileged step. systemd's namespace options (`ProtectSystem=`,
/// `PrivateTmp=`, ...) imply `PrivateUsers=` there, and its seccomp options
/// (`RestrictAddressFamilies=`, `MemoryDenyWriteExecute=`, ...) imply
/// `NoNewPrivileges=`; either would stop `pkexec ufw` for the per-receiver
/// firewall rule. What remains bounds the daemon and its ffmpeg children:
/// private file modes, a task and memory ceiling, first in line for the OOM
/// killer, and no access to the user's session keyring.
const UNIT_TAIL: &str = "Restart=on-failure
RestartSec=1
UMask=0077
TasksMax=1024
MemoryMax=4G
OOMScoreAdjust=200
KeyringMode=private

[Install]
WantedBy=default.target
";
/// Longer than the slowest `connect`: probing, a polkit prompt for the
/// firewall rule (up to 60 s) and two Cast attempts (25 s each). Giving up
/// earlier reports an error while the daemon goes on and casts anyway.
const SEND_TIMEOUT: Duration = Duration::from_secs(150);

pub fn ping_sync(timeout: Duration) -> bool {
    let Ok(mut stream) = UnixStream::connect(util::socket_path()) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(timeout));
    let _ = stream.set_write_timeout(Some(timeout));
    if stream.write_all(b"{\"cmd\": \"ping\"}\n").is_err() {
        return false;
    }
    let mut buffer = [0_u8; 256];
    match stream.read(&mut buffer) {
        Ok(read) if read > 0 => serde_json::from_slice::<Value>(&buffer[..read])
            .ok()
            .and_then(|value| value.get("ok").and_then(Value::as_bool))
            .unwrap_or(false),
        _ => false,
    }
}

fn send(request: &Value) -> std::io::Result<String> {
    let mut stream = UnixStream::connect(util::socket_path())?;
    stream.set_read_timeout(Some(SEND_TIMEOUT))?;
    stream.set_write_timeout(Some(SEND_TIMEOUT))?;
    let mut payload = serde_json::to_vec(request).unwrap_or_default();
    payload.push(b'\n');
    stream.write_all(&payload)?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    Ok(line.trim().to_string())
}

fn build(args: &[String]) -> Value {
    let mut request = json!({"cmd": args.first().cloned().unwrap_or_default()});
    let cmd = request
        .get("cmd")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    match cmd.as_str() {
        "connect" => {
            if let Some(ip) = args.get(1) {
                request["ip"] = json!(ip);
            }
            if let Some(file) = args.get(2) {
                request["file"] = json!(file);
            }
        }
        "add-ip" | "remove-ip" => {
            if let Some(ip) = args.get(1) {
                request["ip"] = json!(ip);
            }
        }
        "save-multicast" | "set-volume" | "set-mute" | "set-boost" | "set-subtitle" => {
            if let Some(value) = args.get(1) {
                request["value"] = json!(value);
            }
        }
        "seek" => {
            if let Some(position) = args.get(1).and_then(|value| value.parse::<f64>().ok()) {
                request["position"] = json!(position);
            }
        }
        _ => {}
    }
    request
}

/// The only binaries the client will start or register as the login service:
/// the one the setup script installs and the one a distro package would.
fn trusted_daemon() -> Option<std::path::PathBuf> {
    let executable = std::env::current_exe().ok()?;
    let allowed = [
        util::home().join(".local/bin/omarchy-castd"),
        std::path::PathBuf::from("/usr/bin/omarchy-castd"),
    ];
    let text = executable.to_str()?;
    // The path goes into a unit file: refuse anything systemd would parse
    // as quoting, a specifier or a new line rather than escape it.
    if text.contains(['"', '\\', '%', '\n', '\r']) {
        return None;
    }
    allowed.contains(&executable).then_some(executable)
}

fn systemctl(args: &[&str]) -> bool {
    Command::new("/usr/bin/systemctl")
        .arg("--user")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// The exact unit files releases before 0.1.6 wrote for a trusted binary,
/// which had no marker line. Only these byte-for-byte are taken as ours.
fn legacy_units() -> Vec<String> {
    const OLD_TAIL: &str =
        "Restart=on-failure\nRestartSec=1\n\n[Install]\nWantedBy=default.target\n";
    let head = UNIT_HEAD
        .strip_prefix(UNIT_MARKER)
        .and_then(|rest| rest.strip_prefix('\n'))
        .unwrap_or(UNIT_HEAD);
    [
        util::home().join(".local/bin/omarchy-castd"),
        std::path::PathBuf::from("/usr/bin/omarchy-castd"),
    ]
    .iter()
    .flat_map(|path| {
        let exec = format!("ExecStart=\"{}\"\n", path.display());
        [
            format!("{head}{exec}{OLD_TAIL}"),
            format!("{head}{exec}{UNIT_TAIL}"),
        ]
    })
    .collect()
}

fn owns_unit(current: &str) -> bool {
    current.lines().next() == Some(UNIT_MARKER) || legacy_units().iter().any(|unit| unit == current)
}

/// Writes the login unit, unless a file the client did not write already has
/// its name: then it is left alone (not rewritten, enabled or started) and
/// the caller falls back to running the daemon outside systemd.
fn install_unit(directory: &Path, executable: &Path) -> bool {
    use std::os::unix::fs::OpenOptionsExt;

    let unit = format!(
        "{UNIT_HEAD}ExecStart=\"{}\"\n{UNIT_TAIL}",
        executable.display()
    );
    let path = directory.join(UNIT_NAME);
    match std::fs::symlink_metadata(&path) {
        Ok(meta) if !meta.file_type().is_file() => return false,
        Ok(_) => {
            let Ok(current) = std::fs::read_to_string(&path) else {
                return false;
            };
            if current == unit {
                return true;
            }
            if !owns_unit(&current) {
                return false;
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return false,
    }
    if std::fs::create_dir_all(directory).is_err() {
        return false;
    }
    // A fresh sibling file renamed over the unit: never writes through a
    // symlink or into a file someone else holds open.
    let tmp = directory.join(format!(".{UNIT_NAME}.{}", std::process::id()));
    let written = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o644)
        .open(&tmp)
        .and_then(|mut file| {
            file.write_all(unit.as_bytes())?;
            file.sync_all()
        })
        .and_then(|()| std::fs::rename(&tmp, &path));
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
        return false;
    }
    systemctl(&["daemon-reload"]);
    true
}

fn ensure() -> bool {
    if ping_sync(Duration::from_millis(2000)) {
        return true;
    }
    let Some(executable) = trusted_daemon() else {
        return false;
    };
    if install_unit(&util::systemd_user_dir(), &executable)
        && systemctl(&["enable", "--now", UNIT_NAME])
    {
        for _ in 0..60 {
            if ping_sync(Duration::from_millis(500)) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    if util::detached_command(&executable).spawn().is_err() {
        return false;
    }
    for _ in 0..50 {
        if ping_sync(Duration::from_millis(500)) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

pub fn main(args: &[String]) {
    // `--no-start` talks to a daemon that is already running and never
    // installs, enables or spawns one (the uninstall script uses it).
    let (start, args) = match args.split_first() {
        Some((first, rest)) if first == "--no-start" => (false, rest),
        _ => (true, args),
    };
    let request = build(args);
    let available = if start {
        ensure()
    } else {
        ping_sync(Duration::from_millis(2000))
    };
    if !available {
        println!(
            "{}",
            json!({"ok": false, "error": "cast service unavailable (the backend must be installed at ~/.local/bin/omarchy-castd or /usr/bin/omarchy-castd)"})
        );
        return;
    }
    match send(&request) {
        Ok(reply) if !reply.is_empty() => println!("{reply}"),
        Ok(_) => println!(
            "{}",
            json!({"ok": false, "error": "empty reply from cast service"})
        ),
        Err(error) => println!(
            "{}",
            json!({"ok": false, "error": format!("cast service error: {error}")})
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("unicast-unit-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        dir
    }

    #[test]
    fn unit_writes_fresh_and_rewrites_its_own() {
        let dir = scratch("own");
        let exe = Path::new("/usr/bin/omarchy-castd");
        assert!(install_unit(&dir, exe));
        let written = std::fs::read_to_string(dir.join(UNIT_NAME)).expect("unit");
        assert!(written.starts_with(UNIT_MARKER));
        std::fs::write(dir.join(UNIT_NAME), format!("{UNIT_MARKER}\n[Unit]\n")).expect("stale");
        assert!(install_unit(&dir, exe));
        assert_eq!(
            std::fs::read_to_string(dir.join(UNIT_NAME)).expect("unit"),
            written
        );
        for legacy in legacy_units() {
            std::fs::write(dir.join(UNIT_NAME), &legacy).expect("legacy");
            assert!(install_unit(&dir, exe));
            assert_eq!(
                std::fs::read_to_string(dir.join(UNIT_NAME)).expect("unit"),
                written
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unit_leaves_foreign_files_and_symlinks_alone() {
        let dir = scratch("foreign");
        let exe = Path::new("/usr/bin/omarchy-castd");
        let foreign = "[Unit]\nDescription=someone else's castd\n[Service]\nExecStart=/opt/castd\n";
        std::fs::write(dir.join(UNIT_NAME), foreign).expect("foreign");
        assert!(!install_unit(&dir, exe));
        assert_eq!(
            std::fs::read_to_string(dir.join(UNIT_NAME)).expect("unit"),
            foreign
        );

        std::fs::remove_file(dir.join(UNIT_NAME)).expect("rm");
        let target = dir.join("target");
        std::fs::write(&target, "keep").expect("target");
        std::os::unix::fs::symlink(&target, dir.join(UNIT_NAME)).expect("link");
        assert!(!install_unit(&dir, exe));
        assert_eq!(std::fs::read_to_string(&target).expect("target"), "keep");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unit_is_bounded_but_keeps_pkexec_usable() {
        let unit = format!("{UNIT_HEAD}ExecStart=\"/usr/bin/omarchy-castd\"\n{UNIT_TAIL}");
        for line in [
            "UMask=0077",
            "TasksMax=1024",
            "MemoryMax=4G",
            "OOMScoreAdjust=200",
        ] {
            assert!(unit.lines().any(|l| l == line), "{line}");
        }
        // Each of these implies NoNewPrivileges= or PrivateUsers= in a user
        // unit, which would break the pkexec firewall prompt.
        for option in [
            "NoNewPrivileges",
            "PrivateUsers",
            "ProtectSystem",
            "ProtectHome",
            "PrivateTmp",
            "RestrictAddressFamilies",
            "SystemCallFilter",
            "MemoryDenyWriteExecute",
            "LockPersonality",
            "RestrictNamespaces",
        ] {
            assert!(!unit.contains(option), "{option}");
        }
    }
}
