use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use crate::util;

pub fn get(key: &str, default: &str) -> String {
    fs::read_to_string(util::settings_file())
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .and_then(|value| value.get(key).and_then(|v| v.as_str()).map(str::to_string))
        .unwrap_or_else(|| default.to_string())
}

pub fn set(key: &str, value: &str) -> std::io::Result<()> {
    util::ensure_dirs();
    let mut data = fs::read_to_string(util::settings_file())
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    data.insert(
        key.to_string(),
        serde_json::Value::String(value.to_string()),
    );
    let text = serde_json::to_string(&data).unwrap_or_else(|_| "{}".to_string());
    let tmp = util::settings_file().with_extension("json.tmp");
    fs::write(&tmp, text)?;
    fs::rename(tmp, util::settings_file())
}

pub fn multicast_discovery() -> bool {
    get("multicastDiscovery", "true") == "true"
}

pub fn last_receiver() -> String {
    get("lastReceiver", "")
}

/// Audio boost levels the widget offers, in dB. 0 leaves the soundtrack untouched.
pub const BOOST_LEVELS: [i32; 3] = [0, 6, 12];

/// Parses a boost level as sent by the widget (`"0"`, `"6"`, `"12"`).
pub fn parse_boost(value: &str) -> Option<i32> {
    let level = value.trim().parse::<i32>().ok()?;
    BOOST_LEVELS.contains(&level).then_some(level)
}

/// Preferred subtitle language ("off", or a two-letter code) for new casts.
pub fn subtitle_preference() -> String {
    get("subtitles", "off")
}

pub fn audio_boost() -> i32 {
    parse_boost(&get("audioBoost", "0")).unwrap_or(0)
}

pub fn manual_ips() -> Vec<String> {
    read_lines(&util::manual_ips_file())
}

pub fn write_manual_ips(ips: &[String]) {
    util::ensure_dirs();
    let body = ips.join("\n");
    let body = if body.is_empty() { body } else { body + "\n" };
    let _ = fs::write(util::manual_ips_file(), body);
}

pub fn firewall_ledger() -> BTreeSet<String> {
    read_lines(&util::firewall_ledger()).into_iter().collect()
}

pub fn write_firewall_ledger(ips: &BTreeSet<String>) {
    util::ensure_dirs();
    let mut body = ips.iter().cloned().collect::<Vec<_>>().join("\n");
    if !body.is_empty() {
        body.push('\n');
    }
    let _ = fs::write(util::firewall_ledger(), body);
}

fn read_lines(path: &Path) -> Vec<String> {
    fs::read_to_string(path)
        .map(|text| {
            text.lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}
