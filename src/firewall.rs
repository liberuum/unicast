use std::net::Ipv4Addr;
use std::time::Duration;

use crate::settings;
use crate::util;

const UFW: &str = "/usr/bin/ufw";
const PKEXEC: &str = "/usr/bin/pkexec";
const TIMEOUT: Duration = Duration::from_secs(60);

pub fn active() -> bool {
    std::fs::read_to_string("/etc/ufw/ufw.conf").is_ok_and(|text| text.contains("ENABLED=yes"))
}

/// One rule the daemon added: traffic from `receiver` to the media server on
/// `bind:port`. Recorded in the ledger as `receiver bind port`; a bare
/// `receiver` line is the pre-0.1.4 form, whose rule targeted `any`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Rule {
    receiver: String,
    bind: String,
    port: u16,
}

impl Rule {
    /// Parse a ledger line, refusing anything that is not exactly a rule this
    /// daemon could have written: the ledger is a user-writable file and its
    /// contents end up as `pkexec ufw delete` arguments.
    fn parse(line: &str, legacy_port: u16) -> Option<Self> {
        let fields: Vec<&str> = line.split_whitespace().collect();
        let (receiver, bind, port) = match fields.as_slice() {
            [receiver] => (*receiver, "any".to_string(), legacy_port),
            [receiver, bind, port] => {
                let bind = bind
                    .parse::<Ipv4Addr>()
                    .ok()
                    .filter(|ip| util::lan_ipv4(*ip))?;
                (
                    *receiver,
                    bind.to_string(),
                    port.parse().ok().filter(|p| *p > 0)?,
                )
            }
            _ => return None,
        };
        let receiver = util::valid_receiver_ip(receiver).filter(|ip| ip == receiver)?;
        Some(Self {
            receiver,
            bind,
            port,
        })
    }

    fn line(&self) -> String {
        if self.bind == "any" {
            self.receiver.clone()
        } else {
            format!("{} {} {}", self.receiver, self.bind, self.port)
        }
    }

    fn covers(&self, receiver: &str, bind: &str, port: u16) -> bool {
        self.receiver == receiver && self.port == port && (self.bind == "any" || self.bind == bind)
    }

    fn ufw_args(&self) -> Vec<String> {
        [
            "from",
            &self.receiver,
            "proto",
            "tcp",
            "to",
            &self.bind,
            "port",
            &self.port.to_string(),
            "comment",
            &format!("universal-cast-{}", self.receiver),
        ]
        .iter()
        .map(ToString::to_string)
        .collect()
    }
}

fn ledger_rules() -> Vec<Rule> {
    let port = util::cast_port();
    settings::firewall_ledger()
        .iter()
        .filter_map(|line| {
            let rule = Rule::parse(line, port);
            if rule.is_none() {
                tracing::warn!("ignoring malformed firewall ledger line {line:?}");
            }
            rule
        })
        .collect()
}

fn write_ledger(rules: &[Rule]) {
    settings::write_firewall_ledger(&rules.iter().map(Rule::line).collect());
}

/// Allow `receiver` to reach the media server on `bind:port` only.
pub async fn open(receiver: &str, bind: Ipv4Addr, port: u16) -> bool {
    if !active() {
        return true;
    }
    let (Some(receiver), true) = (util::valid_receiver_ip(receiver), util::lan_ipv4(bind)) else {
        return false;
    };
    let bind = bind.to_string();
    let mut rules = ledger_rules();
    if rules.iter().any(|rule| rule.covers(&receiver, &bind, port)) {
        return true;
    }
    let rule = Rule {
        receiver,
        bind,
        port,
    };
    let mut args = vec!["allow".to_string()];
    args.extend(rule.ufw_args());
    let Some(stdout) = run_output(&args).await else {
        return false;
    };
    if rule_was_added(&stdout) {
        rules.push(rule);
        write_ledger(&rules);
    }
    true
}

/// Delete exactly the rules recorded in the ledger, nothing else.
pub async fn clear() {
    let rules = ledger_rules();
    if !active() {
        write_ledger(&[]);
        return;
    }
    let mut remaining = Vec::new();
    for rule in rules {
        let mut args = vec![
            "--force".to_string(),
            "delete".to_string(),
            "allow".to_string(),
        ];
        args.extend(rule.ufw_args());
        // Keep what could not be removed (polkit declined, timed out) so a
        // later `clear` and the uninstall script still know about it.
        if !run(&args).await {
            remaining.push(rule);
        }
    }
    write_ledger(&remaining);
}

async fn run(args: &[String]) -> bool {
    run_output(args).await.is_some()
}

/// Stdout of a successful `pkexec ufw …`, `None` on failure or timeout.
async fn run_output(args: &[String]) -> Option<String> {
    let mut command = tokio::process::Command::new(PKEXEC);
    command.arg(UFW).args(args);
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        // On timeout the future is dropped: take pkexec (and its polkit prompt)
        // down with it, or a late approval would add a rule the ledger never
        // records and `clear` never removes.
        .kill_on_drop(true);
    match tokio::time::timeout(TIMEOUT, command.output()).await {
        Ok(Ok(output)) if output.status.success() => {
            Some(String::from_utf8_lossy(&output.stdout).into_owned())
        }
        _ => None,
    }
}

/// ufw exits 0 without adding anything when an identical rule exists; that
/// rule is the user's, so it must not enter the ledger (and later be deleted).
fn rule_was_added(stdout: &str) -> bool {
    !stdout.contains("Skipping")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ledger_lines_round_trip() {
        let rule = Rule::parse("192.168.1.8 192.168.1.6 60020", 1).expect("rule");
        assert_eq!(rule.line(), "192.168.1.8 192.168.1.6 60020");
        assert!(rule.covers("192.168.1.8", "192.168.1.6", 60020));
        assert!(!rule.covers("192.168.1.8", "192.168.1.6", 60021));
        let legacy = Rule::parse("192.168.1.8", 60020).expect("legacy");
        assert_eq!(legacy.bind, "any");
        assert!(legacy.covers("192.168.1.8", "10.0.0.2", 60020));
        assert_eq!(legacy.line(), "192.168.1.8");
    }

    #[test]
    fn ledger_refuses_what_the_daemon_never_writes() {
        for line in [
            "any",
            "10.0.0.0/8",
            "8.8.8.8",
            "0.0.0.0",
            "192.168.1.8 any 60020",
            "192.168.1.8 8.8.8.8 60020",
            "192.168.1.8 192.168.1.6 0",
            "192.168.1.8 192.168.1.6 22 extra",
            "192.168.001.008",
        ] {
            assert_eq!(Rule::parse(line, 60020), None, "{line}");
        }
    }

    #[test]
    fn existing_user_rules_are_not_claimed() {
        assert!(rule_was_added("Rule added\n"));
        assert!(!rule_was_added("Skipping adding existing rule\n"));
    }

    #[test]
    fn rule_is_scoped_to_the_bind_address() {
        let rule = Rule::parse("192.168.1.8 192.168.1.6 60020", 1).expect("rule");
        assert_eq!(
            rule.ufw_args().join(" "),
            "from 192.168.1.8 proto tcp to 192.168.1.6 port 60020 comment universal-cast-192.168.1.8"
        );
    }
}
