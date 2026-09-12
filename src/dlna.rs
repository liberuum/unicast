use std::collections::HashSet;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, ToSocketAddrs, UdpSocket};
use std::time::{Duration, Instant};

use quick_xml::Reader;
use quick_xml::events::Event;

use crate::protocol::Device;
use crate::util;

pub const SSDP_ADDR: &str = "239.255.255.250";
pub const SSDP_PORT: u16 = 1900;
pub const MEDIARENDERER: &str = "urn:schemas-upnp-org:device:MediaRenderer:1";
pub const PROBE_PORTS: [u16; 11] = [
    9197, 7676, 8080, 1400, 3000, 8060, 55000, 49152, 49153, 2869, 1054,
];
pub const PROBE_PATHS: [&str; 8] = [
    "/dmr",
    "/dmr/",
    "/description.xml",
    "/MediaRenderer/desc.xml",
    "/upnp/desc.xml",
    "/dd.xml",
    "/device.xml",
    "/xml/device_description.xml",
];

fn msearch(st: &str) -> Vec<u8> {
    format!(
        "M-SEARCH * HTTP/1.1\r\nHOST: {SSDP_ADDR}:{SSDP_PORT}\r\nMAN: \"ssdp:discover\"\r\nMX: 2\r\nST: {st}\r\n\r\n"
    )
    .into_bytes()
}

fn new_socket(lan_ip: Ipv4Addr) -> std::io::Result<UdpSocket> {
    let socket = socket2::Socket::new(socket2::Domain::IPV4, socket2::Type::DGRAM, None)?;
    socket.set_reuse_address(true)?;
    socket.bind(&SocketAddr::new(IpAddr::V4(lan_ip), 0).into())?;
    socket.set_multicast_if_v4(&lan_ip)?;
    socket.set_multicast_ttl_v4(2)?;
    socket.set_read_timeout(Some(Duration::from_secs(1)))?;
    Ok(socket.into())
}

fn collect_locations(socket: &UdpSocket, window: Duration) -> HashSet<String> {
    let deadline = Instant::now() + window;
    let mut locations = HashSet::new();
    let mut buffer = [0_u8; 4096];
    while Instant::now() < deadline {
        match socket.recv_from(&mut buffer) {
            Ok((len, _)) => {
                let text = String::from_utf8_lossy(&buffer[..len]);
                for line in text.split("\r\n") {
                    if line.to_ascii_lowercase().starts_with("location:") {
                        if let Some((_, value)) = line.split_once(':') {
                            let value = value.trim().to_string();
                            if !value.is_empty() {
                                locations.insert(value);
                            }
                        }
                    }
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(_) => break,
        }
    }
    locations
}

pub fn ssdp_multicast(lan_ip: Ipv4Addr) -> HashSet<String> {
    let mut locations = HashSet::new();
    let Ok(socket) = new_socket(lan_ip) else {
        return locations;
    };
    for _ in 0..3 {
        for st in [MEDIARENDERER, "ssdp:all"] {
            let _ = socket.send_to(&msearch(st), (SSDP_ADDR, SSDP_PORT));
        }
    }
    locations.extend(collect_locations(&socket, Duration::from_secs(4)));
    locations
}

pub fn ssdp_unicast(ip: &str, lan_ip: Ipv4Addr) -> HashSet<String> {
    let mut locations = HashSet::new();
    let Ok(socket) = new_socket(lan_ip) else {
        return locations;
    };
    for st in [MEDIARENDERER, "ssdp:all"] {
        let _ = socket.send_to(&msearch(st), (ip, SSDP_PORT));
    }
    for location in collect_locations(&socket, Duration::from_secs(2)) {
        if host_of(&location).as_deref() == Some(ip) {
            locations.insert(location);
        }
    }
    locations
}

pub fn host_of(url: &str) -> Option<String> {
    url::Url::parse(url)
        .ok()
        .and_then(|parsed| parsed.host_str().map(str::to_string))
}

fn client() -> &'static reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap_or_default()
    })
}

const MAX_DESCRIPTION_BYTES: u64 = 1024 * 1024;

async fn http_get_text(url: &str, timeout: Duration) -> Option<String> {
    let response = client().get(url).timeout(timeout).send().await.ok()?;
    if response
        .content_length()
        .is_some_and(|length| length > MAX_DESCRIPTION_BYTES)
    {
        return None;
    }
    response.text().await.ok()
}

pub async fn discover(
    lan_ip: Option<Ipv4Addr>,
    candidate_ips: &[String],
    use_multicast: bool,
) -> Vec<Device> {
    let lan = lan_ip;
    let candidates = candidate_ips.to_vec();
    let locations = tokio::task::spawn_blocking(move || {
        let mut all = HashSet::new();
        if let Some(lan) = lan {
            if use_multicast {
                all.extend(ssdp_multicast(lan));
            }
            for ip in &candidates {
                all.extend(ssdp_unicast(ip, lan));
            }
        }
        all
    })
    .await
    .unwrap_or_default();

    let mut devices: std::collections::HashMap<String, Device> = std::collections::HashMap::new();
    let mut set = tokio::task::JoinSet::new();
    for location in locations {
        set.spawn(async move { device_from_location(&location).await });
    }
    while let Some(result) = set.join_next().await {
        if let Ok(Some(device)) = result {
            devices.insert(device.ip.clone(), device);
        }
    }

    for ip in candidate_ips {
        if !devices.contains_key(ip) {
            if let Some(device) = probe_ip(ip).await {
                devices.insert(ip.clone(), device);
            }
        }
    }
    devices.into_values().collect()
}

async fn device_from_location(location: &str) -> Option<Device> {
    let parsed = url::Url::parse(location).ok()?;
    let host = parsed.host_str()?.to_string();
    let xml = http_get_text(location, Duration::from_secs(4)).await?;
    let description = parse_description(&xml, location);
    let control = description.control?;
    if !control_ok(&control, &host) {
        return None;
    }
    let rendering = description.rendering.filter(|url| control_ok(url, &host));
    let ip = util::resolve_ip(&host);
    Some(Device {
        name: description.friendly.unwrap_or_else(|| host.clone()),
        host,
        ip: ip.clone(),
        port: parsed.port().unwrap_or(0),
        protocol: "dlna".to_string(),
        model: description.model.unwrap_or_default(),
        id: format!("dlna:{ip}"),
        control_url: Some(control),
        rendering_url: rendering,
        alternates: Vec::new(),
    })
}

fn control_ok(control: &str, host: &str) -> bool {
    let Ok(parsed) = url::Url::parse(control) else {
        return false;
    };
    matches!(parsed.scheme(), "http" | "https") && parsed.host_str() == Some(host)
}

pub async fn probe_ip(ip: &str) -> Option<Device> {
    let target = ip.to_string();
    let open_ports = tokio::task::spawn_blocking(move || scan_ports(&target))
        .await
        .unwrap_or_default();
    for port in open_ports {
        for path in PROBE_PATHS {
            let location = format!("http://{ip}:{port}{path}");
            let Some(xml) = http_get_text(&location, Duration::from_secs(2)).await else {
                continue;
            };
            if !xml.contains("MediaRenderer") && !xml.contains("AVTransport") {
                continue;
            }
            let description = parse_description(&xml, &location);
            if let Some(control) = description.control
                && control_ok(&control, ip)
            {
                return Some(Device {
                    name: description.friendly.unwrap_or_else(|| ip.to_string()),
                    host: ip.to_string(),
                    ip: ip.to_string(),
                    port,
                    protocol: "dlna".to_string(),
                    model: description.model.unwrap_or_default(),
                    id: format!("dlna:{ip}"),
                    control_url: Some(control),
                    rendering_url: description.rendering.filter(|url| control_ok(url, ip)),
                    alternates: Vec::new(),
                });
            }
        }
    }
    None
}

fn scan_ports(ip: &str) -> Vec<u16> {
    std::thread::scope(|scope| {
        let handles: Vec<_> = PROBE_PORTS
            .iter()
            .map(|&port| {
                scope.spawn(move || {
                    let target = (ip, port).to_socket_addrs().ok()?.next()?;
                    std::net::TcpStream::connect_timeout(&target, Duration::from_millis(600))
                        .ok()
                        .map(|_| port)
                })
            })
            .collect();
        handles
            .into_iter()
            .filter_map(|handle| handle.join().ok().flatten())
            .collect()
    })
}

fn local_name(qname: &str) -> String {
    qname.rsplit(':').next().unwrap_or(qname).to_string()
}

/// What we need from a renderer's `UPnP` device description.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Description {
    pub friendly: Option<String>,
    pub model: Option<String>,
    /// Absolute `AVTransport` control URL.
    pub control: Option<String>,
    /// Absolute `RenderingControl` control URL (volume / mute), if advertised.
    pub rendering: Option<String>,
}

#[allow(clippy::map_unwrap_or)]
pub fn parse_description(xml: &str, location: &str) -> Description {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    let mut url_base: Option<String> = None;
    let mut friendly: Option<String> = None;
    let mut model: Option<String> = None;
    let mut control: Option<String> = None;
    let mut rendering: Option<String> = None;
    let mut in_service = false;
    let mut service_type = String::new();
    let mut service_control = String::new();
    let mut current = String::new();
    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Start(event)) => {
                let name = local_name(event.name().as_ref());
                if name == "service" {
                    in_service = true;
                    service_type.clear();
                    service_control.clear();
                }
                current = name;
            }
            Ok(Event::Text(event)) => {
                let raw = event.into_inner();
                let text = quick_xml::escape::unescape(&raw)
                    .map(std::borrow::Cow::into_owned)
                    .unwrap_or_else(|_| raw.into_owned());
                match current.as_str() {
                    "URLBase" => url_base = Some(text),
                    "friendlyName" if !in_service => friendly = Some(text),
                    "modelName" if !in_service => model = Some(text),
                    "serviceType" if in_service => service_type = text,
                    "controlURL" if in_service => service_control = text,
                    _ => {}
                }
            }
            Ok(Event::End(event)) => {
                let name = local_name(event.name().as_ref());
                if name == "service" {
                    if control.is_none()
                        && service_type.contains("AVTransport")
                        && !service_control.is_empty()
                    {
                        control = Some(service_control.clone());
                    }
                    if rendering.is_none()
                        && service_type.contains("RenderingControl")
                        && !service_control.is_empty()
                    {
                        rendering = Some(service_control.clone());
                    }
                    in_service = false;
                }
                current.clear();
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buffer.clear();
    }
    let base = url_base.unwrap_or_else(|| location.to_string());
    let resolve = |raw: String| {
        url::Url::parse(&base)
            .ok()?
            .join(raw.trim())
            .ok()
            .map(|url| url.to_string())
    };
    Description {
        friendly,
        model,
        control: control.and_then(resolve),
        rendering: rendering.and_then(resolve),
    }
}

/// Invokes an `AVTransport` `action` at `control_url`.
pub async fn soap(control_url: &str, action: &str, args: &str) -> (u16, String) {
    soap_service(control_url, "AVTransport", action, args).await
}

/// Invokes `action` on a `UPnP` `service` (`AVTransport`, `RenderingControl`) at
/// `control_url`. Returns the HTTP status and response body (0 / empty when the
/// request itself failed).
pub async fn soap_service(
    control_url: &str,
    service: &str,
    action: &str,
    args: &str,
) -> (u16, String) {
    let body = format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\
         <s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\" \
         s:encodingStyle=\"http://schemas.xmlsoap.org/soap/encoding/\"><s:Body>\
         <u:{action} xmlns:u=\"urn:schemas-upnp-org:service:{service}:1\">\
         <InstanceID>0</InstanceID>{args}</u:{action}></s:Body></s:Envelope>"
    );
    let response = client()
        .post(control_url)
        .header("Content-Type", "text/xml; charset=\"utf-8\"")
        .header(
            "SOAPACTION",
            format!("\"urn:schemas-upnp-org:service:{service}:1#{action}\""),
        )
        .body(body)
        .timeout(Duration::from_secs(8))
        .send()
        .await;
    match response {
        Ok(response) => {
            let status = response.status().as_u16();
            let text = response.text().await.unwrap_or_default();
            (status, text)
        }
        Err(_) => (0, String::new()),
    }
}

fn didl(url: &str, title: &str, mime: &str) -> String {
    let class = if mime.starts_with("audio") {
        "object.item.audioItem.musicTrack"
    } else {
        "object.item.videoItem"
    };
    format!(
        "<DIDL-Lite xmlns=\"urn:schemas-upnp-org:metadata-1-0/DIDL-Lite/\" \
         xmlns:dc=\"http://purl.org/dc/elements/1.1/\" \
         xmlns:upnp=\"urn:schemas-upnp-org:metadata-1-0/upnp/\">\
         <item id=\"0\" parentID=\"-1\" restricted=\"1\">\
         <dc:title>{}</dc:title><upnp:class>{class}</upnp:class>\
         <res protocolInfo=\"http-get:*:{mime}:DLNA.ORG_OP=01;DLNA.ORG_CI=0;\
         DLNA.ORG_FLAGS=01700000000000000000000000000000\">{}</res>\
         </item></DIDL-Lite>",
        util::xml_escape(title),
        util::xml_escape(url)
    )
}

pub async fn play(control_url: &str, url: &str, title: &str, mime: &str) -> Result<(), String> {
    let metadata = didl(url, title, mime);
    let args = format!(
        "<CurrentURI>{}</CurrentURI><CurrentURIMetaData>{}</CurrentURIMetaData>",
        util::xml_escape(url),
        util::xml_escape(&metadata)
    );
    let (mut status, mut body) = soap(control_url, "SetAVTransportURI", &args).await;
    if status != 200 {
        (status, body) = soap(control_url, "SetAVTransportURI", &args).await;
    }
    if status != 200 {
        let description = first_tag(&body, "errorDescription").unwrap_or_default();
        return Err(format!("SetAVTransportURI {status}: {description}"));
    }
    let _ = soap(control_url, "Play", "<Speed>1</Speed>").await;
    Ok(())
}

pub async fn stop(control_url: &str) {
    let _ = soap(control_url, "Stop", "").await;
}

pub async fn pause(control_url: &str) {
    let _ = soap(control_url, "Pause", "").await;
}

pub async fn resume(control_url: &str) {
    let _ = soap(control_url, "Play", "<Speed>1</Speed>").await;
}

pub async fn seek(control_url: &str, seconds: f64) -> bool {
    let target = format_hms(seconds);
    let args = format!("<Unit>REL_TIME</Unit><Target>{target}</Target>");
    let (status, _) = soap(control_url, "Seek", &args).await;
    status == 200
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn format_hms(seconds: f64) -> String {
    let total = seconds.max(0.0);
    let hours = (total / 3600.0).floor() as u64;
    let minutes = ((total % 3600.0) / 60.0).floor() as u64;
    let secs = (total % 60.0).floor() as u64;
    format!("{hours:02}:{minutes:02}:{secs:02}")
}

pub async fn status(control_url: &str) -> String {
    let (_, body) = soap(control_url, "GetTransportInfo", "").await;
    first_tag(&body, "CurrentTransportState").unwrap_or_else(|| "UNKNOWN".to_string())
}

pub async fn position_info(control_url: &str) -> (Option<f64>, Option<f64>) {
    let (_, body) = soap(control_url, "GetPositionInfo", "").await;
    let duration = first_tag(&body, "TrackDuration").map(|value| util::hms(&value));
    let position = first_tag(&body, "RelTime").map(|value| util::hms(&value));
    (duration, position)
}

const MASTER_CHANNEL: &str = "<Channel>Master</Channel>";

/// Receiver volume (0-100) via `RenderingControl`, if it answers.
pub async fn get_volume(rendering_url: &str) -> Option<i32> {
    let (status, body) = soap_service(
        rendering_url,
        "RenderingControl",
        "GetVolume",
        MASTER_CHANNEL,
    )
    .await;
    if status != 200 {
        return None;
    }
    first_tag(&body, "CurrentVolume")?
        .trim()
        .parse::<i32>()
        .ok()
        .map(|level| level.clamp(0, 100))
}

pub async fn get_mute(rendering_url: &str) -> Option<bool> {
    let (status, body) =
        soap_service(rendering_url, "RenderingControl", "GetMute", MASTER_CHANNEL).await;
    if status != 200 {
        return None;
    }
    let value = first_tag(&body, "CurrentMute")?;
    Some(matches!(value.trim(), "1" | "true" | "True" | "TRUE"))
}

pub async fn set_volume(rendering_url: &str, level: i32) -> bool {
    let args = format!(
        "{MASTER_CHANNEL}<DesiredVolume>{}</DesiredVolume>",
        level.clamp(0, 100)
    );
    soap_service(rendering_url, "RenderingControl", "SetVolume", &args)
        .await
        .0
        == 200
}

pub async fn set_mute(rendering_url: &str, muted: bool) -> bool {
    let args = format!(
        "{MASTER_CHANNEL}<DesiredMute>{}</DesiredMute>",
        u8::from(muted)
    );
    soap_service(rendering_url, "RenderingControl", "SetMute", &args)
        .await
        .0
        == 200
}

fn first_tag(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find(&close)? + start;
    Some(xml[start..end].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn description_parsing() {
        let xml = r#"<?xml version="1.0"?>
        <root xmlns="urn:schemas-upnp-org:device-1-0">
          <URLBase>http://192.168.1.50:9197/</URLBase>
          <device>
            <friendlyName>Living Room TV</friendlyName>
            <modelName>OLED55</modelName>
            <serviceList>
              <service>
                <serviceType>urn:schemas-upnp-org:service:AVTransport:1</serviceType>
                <controlURL>/upnp/control/AVTransport1</controlURL>
              </service>
              <service>
                <serviceType>urn:schemas-upnp-org:service:RenderingControl:1</serviceType>
                <controlURL>/upnp/control/RenderingControl1</controlURL>
              </service>
            </serviceList>
          </device>
        </root>"#;
        let description = parse_description(xml, "http://192.168.1.50:9197/dd.xml");
        assert_eq!(description.friendly.as_deref(), Some("Living Room TV"));
        assert_eq!(description.model.as_deref(), Some("OLED55"));
        assert_eq!(
            description.control.as_deref(),
            Some("http://192.168.1.50:9197/upnp/control/AVTransport1")
        );
        assert_eq!(
            description.rendering.as_deref(),
            Some("http://192.168.1.50:9197/upnp/control/RenderingControl1")
        );
    }

    #[test]
    fn relative_control_resolution() {
        let xml = r"<device><serviceList><service>
            <serviceType>AVTransport</serviceType>
            <controlURL>ctl/AVTransport</controlURL>
        </service></serviceList></device>";
        let description = parse_description(xml, "http://10.0.0.5:8080/dmr/desc.xml");
        assert_eq!(
            description.control.as_deref(),
            Some("http://10.0.0.5:8080/dmr/ctl/AVTransport")
        );
        assert_eq!(description.rendering, None);
    }

    #[test]
    fn control_ssrf_guard() {
        assert!(control_ok("http://10.0.0.5:8080/ctl", "10.0.0.5"));
        assert!(!control_ok("http://evil.example/ctl", "10.0.0.5"));
        assert!(!control_ok("file:///etc/passwd", "10.0.0.5"));
    }
}
