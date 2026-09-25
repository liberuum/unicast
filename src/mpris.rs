use std::collections::HashMap;

use zbus::zvariant::OwnedValue;

pub async fn current_file() -> Option<String> {
    let connection = zbus::Connection::session().await.ok()?;
    let dbus = zbus::fdo::DBusProxy::new(&connection).await.ok()?;
    let names = dbus.list_names().await.ok()?;
    let mut found: Vec<(u8, String)> = Vec::new();
    for name in names {
        let name = name.to_string();
        if !name.starts_with("org.mpris.MediaPlayer2.") {
            continue;
        }
        let Ok(player) = zbus::Proxy::new(
            &connection,
            name.as_str(),
            "/org/mpris/MediaPlayer2",
            "org.mpris.MediaPlayer2.Player",
        )
        .await
        else {
            continue;
        };
        let Ok(metadata) = player
            .get_property::<HashMap<String, OwnedValue>>("Metadata")
            .await
        else {
            continue;
        };
        let Some(url) = metadata.get("xesam:url") else {
            continue;
        };
        let Ok(url) = url.downcast_ref::<String>() else {
            continue;
        };
        let Some(path) = file_url_to_path(&url) else {
            continue;
        };
        let status = player
            .get_property::<String>("PlaybackStatus")
            .await
            .unwrap_or_default();
        let rank = match status.as_str() {
            "Playing" => 0,
            "Paused" => 1,
            _ => 2,
        };
        found.push((rank, path));
    }
    found.sort_by_key(|(rank, _)| *rank);
    found.into_iter().next().map(|(_, path)| path)
}

fn file_url_to_path(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    if parsed.scheme() != "file" {
        return None;
    }
    // A lossy decode would turn a non-UTF-8 name into a path that does not
    // exist; report "no file" instead of a misleading "file not found".
    percent_encoding::percent_decode_str(parsed.path())
        .decode_utf8()
        .ok()
        .map(std::borrow::Cow::into_owned)
}
