use std::time::Duration;

use crate::settings;
use crate::util;

const UFW: &str = "/usr/bin/ufw";
const PKEXEC: &str = "/usr/bin/pkexec";
const TIMEOUT: Duration = Duration::from_secs(60);

pub fn active() -> bool {
    std::fs::read_to_string("/etc/ufw/ufw.conf").is_ok_and(|text| text.contains("ENABLED=yes"))
}

pub async fn open(ip: &str) -> bool {
    if !active() {
        return true;
    }
    let mut ledger = settings::firewall_ledger();
    if ledger.contains(ip) {
        return true;
    }
    let comment = format!("universal-cast-{ip}");
    let port = util::cast_port().to_string();
    let status = run(&[
        "allow", "from", ip, "proto", "tcp", "to", "any", "port", &port, "comment", &comment,
    ])
    .await;
    if status {
        ledger.insert(ip.to_string());
        settings::write_firewall_ledger(&ledger);
    }
    status
}

pub async fn clear() {
    let ledger = settings::firewall_ledger();
    if !active() {
        settings::write_firewall_ledger(&std::collections::BTreeSet::default());
        return;
    }
    let port = util::cast_port().to_string();
    for ip in &ledger {
        let comment = format!("universal-cast-{ip}");
        run(&[
            "--force", "delete", "allow", "from", ip, "proto", "tcp", "to", "any", "port", &port,
            "comment", &comment,
        ])
        .await;
    }
    settings::write_firewall_ledger(&std::collections::BTreeSet::default());
}

async fn run(args: &[&str]) -> bool {
    let mut command = tokio::process::Command::new(PKEXEC);
    command.arg(UFW).args(args);
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    match tokio::time::timeout(TIMEOUT, command.status()).await {
        Ok(Ok(status)) => status.success(),
        _ => false,
    }
}
