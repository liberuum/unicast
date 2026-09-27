use std::collections::{BTreeSet, HashMap, HashSet};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::time::{Duration, Instant};

use mdns_sd::{ResolvedService, ServiceDaemon, ServiceEvent};

use crate::dlna;
use crate::protocol::Device;
use crate::util;

const CAST_SERVICE: &str = "_googlecast._tcp.local.";
const CAST_SUFFIX: &str = "._googlecast._tcp.local.";
const AIRPLAY_SERVICE: &str = "_airplay._tcp.local.";
const AIRPLAY_SUFFIX: &str = "._airplay._tcp.local.";
const CAST_PORT: u16 = 8009;
const CAST_PROBE_TIMEOUT: Duration = Duration::from_millis(400);
/// Receivers accepted from one mDNS scan. mDNS is unauthenticated, so a
/// flood of fake services must not turn into a flood of probes.
const MAX_MDNS_DEVICES: usize = 32;

/// mDNS records can carry any address. Only private and link-local IPv4
/// addresses are receivers on this LAN; anything else (loopback, public
/// hosts) is ignored rather than probed.
fn lan_address(ip: Ipv4Addr) -> bool {
    util::lan_ipv4(ip)
}

pub async fn discover(
    lan_ip: Option<Ipv4Addr>,
    manual_ips: &[String],
    use_multicast: bool,
) -> Vec<Device> {
    let mdns = tokio::task::spawn_blocking(mdns_devices)
        .await
        .unwrap_or_default();
    let mut candidates: Vec<String> = mdns.iter().map(|device| device.ip.clone()).collect();
    candidates.extend(
        manual_ips
            .iter()
            .filter_map(|ip| util::valid_receiver_ip(ip)),
    );
    candidates.sort();
    candidates.dedup();
    // A spoofed mDNS record naming this machine must not make it probe itself.
    if let Some(own) = lan_ip.map(|ip| ip.to_string()) {
        candidates.retain(|ip| *ip != own);
    }
    let dlna_devices = dlna::discover(lan_ip, &candidates, use_multicast).await;
    // Whatever a receiver or a hand-edited settings file claims, only LAN
    // IPv4 receivers are ever listed, served to, or let through the firewall.
    merge(mdns, dlna_devices)
        .into_iter()
        .filter(|device| util::valid_receiver_ip(&device.ip).as_deref() == Some(device.ip.as_str()))
        .collect()
}

fn mdns_devices() -> Vec<Device> {
    let Ok(daemon) = ServiceDaemon::new() else {
        return Vec::new();
    };
    let mut receivers = Vec::new();
    if let Ok(receiver) = daemon.browse(CAST_SERVICE) {
        receivers.push(("cast", receiver));
    }
    if let Ok(receiver) = daemon.browse(AIRPLAY_SERVICE) {
        receivers.push(("airplay", receiver));
    }
    if receivers.is_empty() {
        let _ = daemon.shutdown();
        return Vec::new();
    }
    let deadline = Instant::now() + Duration::from_secs(4);
    let mut devices = Vec::new();
    while Instant::now() < deadline {
        for (protocol, receiver) in &receivers {
            while let Ok(event) = receiver.try_recv() {
                if let ServiceEvent::ServiceResolved(info) = event {
                    let device = if *protocol == "cast" {
                        cast_device(&info)
                    } else {
                        airplay_device(&info)
                    };
                    if let Some(device) = device
                        && devices.len() < MAX_MDNS_DEVICES
                    {
                        devices.push(device);
                    }
                }
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = daemon.shutdown();
    promote_cast_capable(devices, cast_port_open)
}

/// mDNS is best-effort: a TV that speaks both Google Cast and `AirPlay` (LG
/// `webOS`, for one) can show up in a given scan with only its `AirPlay` record,
/// and would then be routed over the weaker protocol. Anything that answers
/// on the Cast port is a Cast receiver, whatever the scan happened to hear.
fn promote_cast_capable(
    devices: Vec<Device>,
    cast_open: impl Fn(&str) -> bool + Sync,
) -> Vec<Device> {
    let cast_ips: HashSet<String> = devices
        .iter()
        .filter(|device| device.protocol == "cast")
        .map(|device| device.ip.clone())
        .collect();
    let to_probe: BTreeSet<&str> = devices
        .iter()
        .filter(|device| device.protocol == "airplay" && !cast_ips.contains(&device.ip))
        .map(|device| device.ip.as_str())
        .collect();
    // Probed side by side, so the scan waits one timeout, not one per device.
    let cast_open = &cast_open;
    let open: HashSet<String> = std::thread::scope(|scope| {
        let handles: Vec<_> = to_probe
            .iter()
            .map(|ip| scope.spawn(move || cast_open(ip).then(|| (*ip).to_string())))
            .collect();
        handles
            .into_iter()
            .filter_map(|handle| handle.join().ok().flatten())
            .collect()
    });
    devices
        .into_iter()
        .map(|device| {
            if device.protocol != "airplay" || !open.contains(&device.ip) {
                return device;
            }
            Device {
                protocol: "cast".to_string(),
                port: CAST_PORT,
                id: format!("cast:{}", device.ip),
                alternates: vec!["airplay".to_string()],
                ..device
            }
        })
        .collect()
}

fn cast_port_open(ip: &str) -> bool {
    let Ok(address) = format!("{ip}:{CAST_PORT}").parse::<SocketAddr>() else {
        return false;
    };
    TcpStream::connect_timeout(&address, CAST_PROBE_TIMEOUT).is_ok()
}

fn cast_device(info: &ResolvedService) -> Option<Device> {
    let ip = info
        .get_addresses_v4()
        .into_iter()
        .find(|ip| lan_address(*ip))?;
    let name = info.get_property_val_str("fn").map_or_else(
        || service_name(info.get_fullname(), CAST_SUFFIX),
        str::to_string,
    );
    let model = info
        .get_property_val_str("md")
        .unwrap_or_default()
        .to_string();
    let ident = info
        .get_property_val_str("id")
        .map_or_else(|| ip.to_string(), str::to_string);
    Some(Device {
        name,
        host: info.get_hostname().to_string(),
        ip: ip.to_string(),
        port: info.get_port(),
        protocol: "cast".to_string(),
        model,
        id: format!("cast:{ident}"),
        control_url: None,
        rendering_url: None,
        alternates: Vec::new(),
    })
}

fn airplay_device(info: &ResolvedService) -> Option<Device> {
    let ip = info
        .get_addresses_v4()
        .into_iter()
        .find(|ip| lan_address(*ip))?;
    let model = info
        .get_property_val_str("model")
        .unwrap_or_default()
        .to_string();
    let ident = info
        .get_property_val_str("deviceid")
        .map_or_else(|| ip.to_string(), str::to_string);
    Some(Device {
        name: service_name(info.get_fullname(), AIRPLAY_SUFFIX),
        host: info.get_hostname().to_string(),
        ip: ip.to_string(),
        port: info.get_port(),
        protocol: "airplay".to_string(),
        model,
        id: format!("airplay:{ident}"),
        control_url: None,
        rendering_url: None,
        alternates: Vec::new(),
    })
}

fn service_name(fullname: &str, suffix: &str) -> String {
    fullname
        .strip_suffix(suffix)
        .unwrap_or(fullname)
        .to_string()
}

fn preference(protocol: &str) -> u8 {
    match protocol {
        "dlna" => 3,
        "cast" => 2,
        "airplay" => 1,
        _ => 0,
    }
}

fn merge(mdns: Vec<Device>, dlna_devices: Vec<Device>) -> Vec<Device> {
    let mut by_ip: HashMap<String, Device> = HashMap::new();
    for device in mdns.into_iter().chain(dlna_devices) {
        let ip = device.ip.clone();
        match by_ip.remove(&ip) {
            None => {
                by_ip.insert(ip, device);
            }
            Some(existing) => {
                let (mut keep, drop) =
                    if preference(&device.protocol) > preference(&existing.protocol) {
                        (device, existing)
                    } else {
                        (existing, device)
                    };
                let mut alternates: BTreeSet<String> = keep.alternates.iter().cloned().collect();
                alternates.extend(drop.alternates.iter().cloned());
                alternates.insert(drop.protocol.clone());
                keep.alternates = alternates.into_iter().collect();
                if (keep.name.is_empty() || keep.name == keep.ip) && !drop.name.is_empty() {
                    keep.name.clone_from(&drop.name);
                }
                if keep.model.is_empty() && !drop.model.is_empty() {
                    keep.model.clone_from(&drop.model);
                }
                by_ip.insert(ip, keep);
            }
        }
    }
    let mut devices: Vec<Device> = by_ip.into_values().collect();
    devices.sort_by(|a, b| {
        (a.protocol.as_str(), a.name.to_lowercase())
            .cmp(&(b.protocol.as_str(), b.name.to_lowercase()))
    });
    devices
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(ip: &str, protocol: &str, name: &str) -> Device {
        Device {
            name: name.to_string(),
            host: ip.to_string(),
            ip: ip.to_string(),
            port: 8009,
            protocol: protocol.to_string(),
            model: String::new(),
            id: format!("{protocol}:{ip}"),
            control_url: None,
            rendering_url: None,
            alternates: Vec::new(),
        }
    }

    #[test]
    fn merge_prefers_dlna() {
        let merged = merge(
            vec![device("10.0.0.9", "cast", "TV")],
            vec![device("10.0.0.9", "dlna", "")],
        );
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].protocol, "dlna");
        assert_eq!(merged[0].name, "TV");
        assert_eq!(merged[0].alternates, vec!["cast".to_string()]);
    }

    #[test]
    fn only_lan_addresses_are_receivers() {
        for ip in ["192.168.1.8", "10.0.0.7", "172.16.4.2", "169.254.3.3"] {
            assert!(lan_address(ip.parse().expect("ip")), "{ip}");
        }
        for ip in [
            "127.0.0.1",
            "8.8.8.8",
            "0.0.0.0",
            "224.0.0.251",
            "255.255.255.255",
        ] {
            assert!(!lan_address(ip.parse().expect("ip")), "{ip}");
        }
    }

    #[test]
    fn airplay_only_receiver_with_cast_port_becomes_cast() {
        let scanned = vec![
            device("10.0.0.7", "airplay", "LG TV"),
            device("10.0.0.8", "airplay", "Apple TV"),
            device("10.0.0.9", "cast", "Chromecast"),
        ];
        let probed = std::sync::Mutex::new(Vec::new());
        let promoted = promote_cast_capable(scanned, |ip| {
            probed.lock().expect("probe log").push(ip.to_string());
            ip == "10.0.0.7"
        });
        // Only AirPlay-only receivers are probed; the Chromecast is left alone.
        let mut probed = probed.into_inner().expect("probe log");
        probed.sort();
        assert_eq!(probed, ["10.0.0.7", "10.0.0.8"]);
        let lg = promoted.iter().find(|d| d.ip == "10.0.0.7").expect("lg");
        assert_eq!(lg.protocol, "cast");
        assert_eq!(lg.port, 8009);
        assert_eq!(lg.id, "cast:10.0.0.7");
        assert_eq!(lg.alternates, vec!["airplay".to_string()]);
        assert_eq!(lg.name, "LG TV");
        let apple = promoted.iter().find(|d| d.ip == "10.0.0.8").expect("apple");
        assert_eq!(apple.protocol, "airplay");
    }

    #[test]
    fn service_name_strips_suffix() {
        assert_eq!(
            service_name("Living Room TV._googlecast._tcp.local.", CAST_SUFFIX),
            "Living Room TV"
        );
        assert_eq!(
            service_name("Samsung 7 Series._airplay._tcp.local.", AIRPLAY_SUFFIX),
            "Samsung 7 Series"
        );
    }
}
