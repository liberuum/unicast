use std::net::Ipv4Addr;
use std::time::Duration;

use crate::settings;
use crate::util;

const UFW: &str = "/usr/bin/ufw";
const PKEXEC: &str = "/usr/bin/pkexec";
const TIMEOUT: Duration = Duration::from_secs(60);
/// ufw's saved IPv4 rules; world-readable on Arch. Read to see what already exists.
const USER_RULES: &str = "/etc/ufw/user.rules";
/// Longest line accepted from it; ufw's own lines are well under 1 KiB.
const USER_RULES_MAX_LINE: usize = 64 * 1024;

/// Whether ufw is enabled, read the way ufw reads its own config: an
/// `ENABLED=yes` line (quotes allowed), not the text appearing anywhere.
pub fn active() -> bool {
    std::fs::read_to_string("/etc/ufw/ufw.conf").is_ok_and(|text| enabled_in(&text))
}

fn enabled_in(conf: &str) -> bool {
    conf.lines().any(|line| {
        line.trim()
            .strip_prefix("ENABLED=")
            .is_some_and(|value| value.trim().trim_matches('"') == "yes")
    })
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
pub async fn open(receiver: &str, bind: Ipv4Addr, port: u16) -> Result<(), String> {
    if !active() {
        return Ok(());
    }
    let (Some(receiver), true) = (util::valid_receiver_ip(receiver), util::lan_ipv4(bind)) else {
        return Err("Firewall rule refused: not a LAN receiver".into());
    };
    let bind = bind.to_string();
    let mut rules = ledger_rules();
    if rules.iter().any(|rule| rule.covers(&receiver, &bind, port)) {
        return Ok(());
    }
    let rule = Rule {
        receiver,
        bind,
        port,
    };
    // `ufw allow` does not skip a rule for the same traffic that differs only
    // in action or comment: it replaces it with ours ("Rule updated"), turning
    // a user's deny into an allow, and `clear` would later delete it. Any such
    // rule is the user's decision, so it is left exactly as it is. (Only root
    // can change the rules, so the time between this check and the polkit
    // prompt below is not something another program can use; if the user adds
    // such a rule in that window, ufw answers "Rule updated", which is logged
    // and never recorded.)
    match user_rules_have(&rule) {
        Some(true) => {
            tracing::info!(
                "ufw already has a rule for {} -> {}:{}; leaving it as it is",
                rule.receiver,
                rule.bind,
                rule.port
            );
            return Ok(());
        }
        Some(false) => {}
        None => {
            return Err(format!(
                "Cannot read all of {USER_RULES} to check for a rule of yours, so the firewall was not changed. Allow the receiver yourself: sudo ufw allow from {} proto tcp to {} port {}",
                rule.receiver, rule.bind, rule.port
            ));
        }
    }
    let mut args = vec!["allow".to_string()];
    args.extend(rule.ufw_args());
    let Some(stdout) = run_output(&args).await else {
        return Err("Firewall authorization was declined".into());
    };
    if rule_was_added(&stdout) {
        rules.push(rule);
        write_ledger(&rules);
    } else {
        tracing::warn!(
            "ufw did not add a new rule ({}); a rule of yours for {} -> {}:{} exists, so it is not recorded or ever deleted",
            stdout.trim(),
            rule.receiver,
            rule.bind,
            rule.port
        );
    }
    Ok(())
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

/// Whether ufw's saved rules already hold one for the same traffic as
/// `rule`. The whole file is read, a line at a time so memory stays bounded
/// however long it is. `None` when it cannot be read to the end (missing,
/// unreadable, a read error, an oversized line): a rule past that point could
/// be the one `ufw allow` would replace, so the caller changes nothing.
fn user_rules_have(rule: &Rule) -> Option<bool> {
    let file = std::fs::File::open(USER_RULES).ok()?;
    scan_user_rules(std::io::BufReader::new(file), rule)
}

fn scan_user_rules(mut reader: impl std::io::BufRead, rule: &Rule) -> Option<bool> {
    use std::io::{BufRead, Read};
    let limit = u64::try_from(USER_RULES_MAX_LINE).ok()?;
    let mut line = Vec::new();
    loop {
        line.clear();
        (&mut reader)
            .take(limit)
            .read_until(b'\n', &mut line)
            .ok()?;
        if line.is_empty() {
            return Some(false);
        }
        if line.len() >= USER_RULES_MAX_LINE && line.last() != Some(&b'\n') {
            return None;
        }
        if tuple_matches(&String::from_utf8_lossy(&line), rule) {
            return Some(true);
        }
    }
}

/// Reads a `### tuple ###` line the way ufw loads it (`_read_rules` in
/// `ufw/backend_iptables.py`) and compares it the way `UFWRule.match` does:
/// same protocol, ports, addresses, applications, interface, direction and
/// chain, whatever the action, log type or comment. Exactly the rules
/// `ufw allow` would replace with ours.
fn tuple_matches(line: &str, rule: &Rule) -> bool {
    let Some(tuple) = line.strip_prefix("### tuple ###") else {
        return false;
    };
    // ufw strips everything from the first " comment=".
    let tuple = tuple
        .split_once(" comment=")
        .map_or(tuple, |(head, _)| head);
    let fields: Vec<&str> = tuple.split_whitespace().collect();
    // action proto dport dst sport src [dapp sapp] [direction or interface];
    // the 6 and 8 field forms predate the direction field and mean "in".
    let (action, proto, dport, dst, sport, src, apps, direction) = match fields.as_slice() {
        [a, p, dp, d, sp, s] => (*a, *p, *dp, *d, *sp, *s, ("-", "-"), "in"),
        [a, p, dp, d, sp, s, dir] => (*a, *p, *dp, *d, *sp, *s, ("-", "-"), *dir),
        [a, p, dp, d, sp, s, da, sa] => (*a, *p, *dp, *d, *sp, *s, (*da, *sa), "in"),
        [a, p, dp, d, sp, s, da, sa, dir] => (*a, *p, *dp, *d, *sp, *s, (*da, *sa), *dir),
        _ => return false,
    };
    let bare = |address: &str| address.strip_suffix("/32").unwrap_or(address).to_string();
    // "route:<action>" is a forward-chain rule, which ufw never matches to ours.
    !action.contains(':')
        && proto == "tcp"
        && dport == rule.port.to_string()
        && sport == "any"
        && bare(dst) == rule.bind
        && bare(src) == rule.receiver
        && apps == ("-", "-")
        && direction == "in"
}

/// Only ufw's "Rule added" means a new rule of ours exists. It exits 0 with
/// "Skipping adding existing rule" for an identical rule and "Rule updated"
/// when it replaced one; either rule is the user's and must not enter the
/// ledger (and later be deleted).
fn rule_was_added(stdout: &str) -> bool {
    stdout.lines().any(|line| line.trim() == "Rule added")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ledger_lines_round_trip() {
        let rule = Rule::parse("192.168.50.20 192.168.50.10 60020", 1).expect("rule");
        assert_eq!(rule.line(), "192.168.50.20 192.168.50.10 60020");
        assert!(rule.covers("192.168.50.20", "192.168.50.10", 60020));
        assert!(!rule.covers("192.168.50.20", "192.168.50.10", 60021));
        let legacy = Rule::parse("192.168.50.20", 60020).expect("legacy");
        assert_eq!(legacy.bind, "any");
        assert!(legacy.covers("192.168.50.20", "10.0.0.2", 60020));
        assert_eq!(legacy.line(), "192.168.50.20");
    }

    #[test]
    fn ledger_refuses_what_the_daemon_never_writes() {
        for line in [
            "any",
            "10.0.0.0/8",
            "8.8.8.8",
            "0.0.0.0",
            "192.168.50.20 any 60020",
            "192.168.50.20 8.8.8.8 60020",
            "192.168.50.20 192.168.50.10 0",
            "192.168.50.20 192.168.50.10 22 extra",
            "192.168.001.008",
        ] {
            assert_eq!(Rule::parse(line, 60020), None, "{line}");
        }
    }

    #[test]
    fn existing_user_rules_are_not_claimed() {
        assert!(rule_was_added("Rule added\n"));
        assert!(!rule_was_added("Skipping adding existing rule\n"));
        assert!(!rule_was_added("Rule updated\n"));
        assert!(!rule_was_added("Rules updated\n"));
    }

    #[test]
    fn enabled_is_read_as_a_setting() {
        assert!(enabled_in("# comment\nENABLED=yes\nLOGLEVEL=low\n"));
        assert!(enabled_in("ENABLED=\"yes\"\n"));
        assert!(!enabled_in(
            "ENABLED=no\n# set ENABLED=yes to start on boot\n"
        ));
        assert!(!enabled_in("#ENABLED=yes\n"));
    }

    fn scan(text: &str, rule: &Rule) -> Option<bool> {
        scan_user_rules(std::io::Cursor::new(text.as_bytes()), rule)
    }

    #[test]
    fn a_users_rule_for_the_same_traffic_is_left_alone() {
        let rule = Rule::parse("192.168.50.20 192.168.50.10 60020", 1).expect("rule");
        for tuple in [
            "### tuple ### deny tcp 60020 192.168.50.10 any 192.168.50.20 in",
            "### tuple ### allow tcp 60020 192.168.50.10 any 192.168.50.20 in",
            "### tuple ### allow_log tcp 60020 192.168.50.10/32 any 192.168.50.20/32 in comment=6d696e65",
            "### tuple ### reject tcp 60020 192.168.50.10 any 192.168.50.20 in comment=a comment=b",
            // Pre-direction forms, which ufw loads as inbound.
            "### tuple ### deny tcp 60020 192.168.50.10 any 192.168.50.20",
            "### tuple ### limit tcp 60020 192.168.50.10 any 192.168.50.20 - -",
            "### tuple ### deny tcp 60020 192.168.50.10 any 192.168.50.20 - - in",
        ] {
            assert_eq!(
                scan(&format!("*filter\n{tuple}\n"), &rule),
                Some(true),
                "{tuple}"
            );
            // No trailing newline on the last line is still read.
            assert_eq!(scan(tuple, &rule), Some(true), "{tuple}");
        }
        for tuple in [
            "### tuple ### deny tcp 60021 192.168.50.10 any 192.168.50.20 in",
            "### tuple ### deny udp 60020 192.168.50.10 any 192.168.50.20 in",
            "### tuple ### deny any 60020 192.168.50.10 any 192.168.50.20 in",
            "### tuple ### deny tcp 60020 0.0.0.0/0 any 192.168.50.20 in",
            "### tuple ### deny tcp 60020 192.168.50.10 any 192.168.50.21 in",
            "### tuple ### deny tcp 60020 192.168.50.10 1234 192.168.50.20 in",
            "### tuple ### deny tcp 60020 192.168.50.10 any 192.168.50.20 in_eth0",
            "### tuple ### deny tcp 60020 192.168.50.10 any 192.168.50.20 out",
            "### tuple ### route:deny tcp 60020 192.168.50.10 any 192.168.50.20 in",
            "### tuple ### deny tcp 60020 192.168.50.10 any 192.168.50.20 Cast - in",
            "# deny tcp 60020 192.168.50.10 any 192.168.50.20 in",
        ] {
            assert_eq!(scan(tuple, &rule), Some(false), "{tuple}");
        }
    }

    #[test]
    fn user_rules_are_read_to_the_end_or_not_trusted() {
        let rule = Rule::parse("192.168.50.20 192.168.50.10 60020", 1).expect("rule");
        // A matching rule after far more than any fixed cap is still found.
        let filler = "-A ufw-user-input -p tcp --dport 22 -j ACCEPT\n".repeat(200_000);
        let text =
            format!("{filler}### tuple ### deny tcp 60020 192.168.50.10 any 192.168.50.20 in\n");
        assert!(text.len() > 8 * 1024 * 1024);
        assert_eq!(scan(&text, &rule), Some(true));
        // A line too long to read is not skipped: the result is "unknown".
        let long = format!(
            "{}\n### tuple ### deny tcp 60020 192.168.50.10 any 192.168.50.20 in\n",
            "x".repeat(USER_RULES_MAX_LINE + 10)
        );
        assert_eq!(scan(&long, &rule), None);
        assert_eq!(scan("", &rule), Some(false));
    }

    #[test]
    fn rule_is_scoped_to_the_bind_address() {
        let rule = Rule::parse("192.168.50.20 192.168.50.10 60020", 1).expect("rule");
        assert_eq!(
            rule.ufw_args().join(" "),
            "from 192.168.50.20 proto tcp to 192.168.50.10 port 60020 comment universal-cast-192.168.50.20"
        );
    }
}
