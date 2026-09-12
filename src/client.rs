use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use serde_json::{Value, json};

use crate::util;

const UNIT_NAME: &str = "omarchy-castd.service";
const SEND_TIMEOUT: Duration = Duration::from_secs(95);

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

fn systemctl(args: &[&str]) -> bool {
    Command::new("systemctl")
        .arg("--user")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn install_unit() -> bool {
    let Ok(executable) = std::env::current_exe() else {
        return false;
    };
    let unit = format!(
        "[Unit]\nDescription=omarchy-cast media daemon\n\n[Service]\nType=simple\nExecStart=\"{}\"\nRestart=on-failure\nRestartSec=1\n\n[Install]\nWantedBy=default.target\n",
        executable.display()
    );
    let directory = util::systemd_user_dir();
    let path = directory.join(UNIT_NAME);
    let current = std::fs::read_to_string(&path).unwrap_or_default();
    if current == unit {
        return true;
    }
    if std::fs::create_dir_all(&directory).is_err() {
        return false;
    }
    if std::fs::write(&path, unit).is_err() {
        return false;
    }
    systemctl(&["daemon-reload"]);
    true
}

fn ensure() -> bool {
    if ping_sync(Duration::from_millis(2000)) {
        return true;
    }
    if install_unit() && systemctl(&["enable", "--now", UNIT_NAME]) {
        for _ in 0..60 {
            if ping_sync(Duration::from_millis(500)) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    let Ok(executable) = std::env::current_exe() else {
        return false;
    };
    if util::detached_command(Path::new(&executable))
        .spawn()
        .is_err()
    {
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
    let request = build(args);
    if !ensure() {
        println!(
            "{}",
            json!({"ok": false, "error": "cast service unavailable"})
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
