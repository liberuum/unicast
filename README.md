<p align="center">
  <img src="assets/banner.png" alt="UniCast: cast local video to any TV, straight from the Omarchy bar" width="100%">
</p>

<p align="center">
  <a href="https://github.com/liberuum/unicast/actions/workflows/ci.yml"><img src="https://github.com/liberuum/unicast/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <img src="https://img.shields.io/badge/Omarchy-4.0-81a1c1" alt="Omarchy 4.0">
  <img src="https://img.shields.io/badge/Rust-1.98.1-a3be8c" alt="Rust 1.98.1">
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-b48ead" alt="MIT license"></a>
</p>

<p align="center">
  <a href="#install">Install</a> ·
  <a href="#usage">Usage</a> ·
  <a href="#supported-receivers">Receivers</a> ·
  <a href="#troubleshooting">Troubleshooting</a> ·
  <a href="#security">Security</a>
</p>

---

**UniCast** is an [Omarchy](https://omarchy.org) bar widget that plays a local
video on your TV over **DLNA**, **Google Cast** or **AirPlay**.

It is not screen mirroring: the TV fetches the file and decodes it itself, so
4K and HEVC play at full quality and the laptop stays quiet.

## Features

- **One click.** Every TV on your network is listed in the bar.
- **Casts what you are watching.** Picks up the file playing in VLC, mpv or any MPRIS player.
- **Remote control.** Pause, seek, volume and mute from the panel.
- **Audio boost.** +6 / +12 dB with a limiter, and a dialogue-first stereo downmix for 5.1.
- **Subtitles.** Sidecar and embedded text subtitles, on Google Cast and DLNA TVs.
- **Plays as is when it can.** Remuxes, and transcodes only when the TV needs it.
- **LAN only.** One file per cast, served to the TV alone. No cloud, no telemetry.

<table>
  <tr>
    <td width="50%"><img src="preview.png" alt="UniCast panel listing receivers"></td>
    <td width="50%"><img src="assets/playing.png" alt="UniCast panel while casting, with seek, volume and audio boost"></td>
  </tr>
  <tr>
    <td align="center"><sub>Pick a receiver</sub></td>
    <td align="center"><sub>While casting: seek, volume, boost, stop</sub></td>
  </tr>
</table>

## Install

**1. Add the widget**

```sh
omarchy plugin add https://github.com/liberuum/unicast --enable
```

**2. Build the backend**

Open the widget and click **Build and install the backend**, or run:

```sh
~/.config/omarchy/plugins/universal-cast/bin/omarchy-cast-setup
```

Setup installs anything missing (`rustup`, `cmake`, `ffmpeg`) with
`omarchy pkg add`, asking first. It then builds the `omarchy-castd` daemon with
the pinned Rust release, installs it to `~/.local/bin`, and starts it as a
user service. The first build takes a minute or two.

**Requirements:** Omarchy 4.0 with the Omarchy shell, `ffmpeg`, and
`systemd --user`. `ufw` is optional; if it is enabled, UniCast asks to open
the media port for each TV.

<details>
<summary><b>Manual build</b></summary>

```sh
cargo build --release --locked
install -Dm755 target/release/omarchy-castd ~/.local/bin/omarchy-castd
```

A binary installed this way is not recorded as UniCast's: setup will not
replace it and uninstall will not delete it.

</details>

## Usage

1. Play a video in VLC or mpv, **or** click **Choose media to play**.
2. Click the **UniCast** icon in the bar.
3. Click a TV.
4. Control playback, volume, boost and subtitles from the panel.
5. Press **Stop** to end the cast.

A TV that does not appear can be added in **Settings → Add a device by IP**.

## Supported receivers

| Protocol | Devices | Features |
| --- | --- | --- |
| **DLNA / UPnP** | Most smart TVs, consoles, Kodi | Play, seek, pause, volume, mute, boost, subtitles |
| **Google Cast** | Chromecast, Android TV, Google TV, Cast-enabled TVs and speakers | Play, seek, pause, volume, mute, boost, subtitles |
| **AirPlay** | Apple TV and AirPlay 1 receivers | Play, seek, pause (experimental) |

If a device supports several protocols, UniCast uses the most capable one
(DLNA, then Cast, then AirPlay).

### Subtitles

UniCast finds subtitle files next to the video (`Movie.srt`, `Movie.en.srt`,
a `Subs/` folder) and text tracks inside the file (MKV, MP4). Pick one in the
panel; your choice is remembered per language. Subtitles stay in sync after
seeking.

- **Google Cast:** switching tracks is instant.
- **DLNA TVs:** the TV reads subtitles when the video loads, so switching
  track reloads the video at the same position.
- Image-based subtitles (PGS, VobSub) are not supported. AirPlay has no
  subtitle support.

## Troubleshooting

**The TV does not show up.** Make sure the TV and laptop are on the same
network. Guest networks, "AP isolation" and VPNs that route the LAN (such as
Tailscale with `--accept-routes`) hide devices. Try **Settings → Scan the
whole network**, or add the TV by IP.

**The TV never starts playing.** If `ufw` is enabled, approve the polkit
prompt that opens port 60020 for that TV.

**The movie is too quiet.** Turn on **Audio boost** (+6 or +12 dB).

**Logs:**

```sh
journalctl --user -u omarchy-castd -f
```

For more detail, set `OMARCHY_CAST_LOG=omarchy_castd=debug`. The media port
(`OMARCHY_CAST_PORT`, default 60020) and transcode bitrate
(`OMARCHY_CAST_VBITRATE`, default 6000 kb/s) can be changed the same way,
with `systemctl --user set-environment` and a restart of the unit.

## Update and remove

```sh
# Update, then rebuild the daemon
omarchy plugin update universal-cast
~/.config/omarchy/plugins/universal-cast/bin/omarchy-cast-setup

# Remove the daemon, its service, cache and firewall rules, then the widget
~/.config/omarchy/plugins/universal-cast/bin/omarchy-cast-uninstall
omarchy plugin remove universal-cast
```

Settings in `~/.config/omarchy/cast/` are kept unless you pass `--purge`.
Uninstall removes only files UniCast created. Packages installed by setup
(`ffmpeg`, `cmake`, `base-devel`, `rustup`), `~/.rustup/` and
`~/.cargo/registry/` are left in place, since other software may use them.

## What it installs

Everything runs as your user. Root is needed only for the two steps below,
and each asks first.

| | |
| --- | --- |
| **Files** | `~/.local/bin/omarchy-castd`, `~/.config/systemd/user/omarchy-castd.service`, `~/.config/omarchy/cast/` (settings), `~/.cache/universal-cast/` (build), `$XDG_RUNTIME_DIR/universal-cast/` (socket, subtitle cache), and `~/.rustup/` / `~/.cargo/registry/` for the build |
| **Packages** | Setup installs any missing `ffmpeg`, `cmake`, `base-devel`, `rustup` with `omarchy pkg add` (root, asks first) |
| **Service** | `omarchy-castd.service`, a `systemd --user` unit started at login |
| **Firewall** | With `ufw` enabled, casting to a new TV runs `pkexec ufw allow from <tv-ip> proto tcp to <lan-ip> port 60020` (root, asks each time). Uninstall removes exactly the rules UniCast added; your own rules are never changed. |
| **Network** | LAN only: SSDP/mDNS discovery, control traffic to the TV, and a media server on port 60020 that serves the TV alone |
| **Config** | Never edits `shell.json`; bar placement goes through `omarchy plugin enable` |

## Security

- The media server listens on the LAN address only, serves one file per cast
  under a random token, and answers only the TV's IP.
- Receivers must have a private or link-local IPv4 address. Data from the
  network is validated and never reaches a shell.
- The file you pick is opened once and served from that descriptor, so
  changing the path mid-cast changes nothing.
- The Rust release, crates and CI actions are pinned and checksummed.
- Setup and uninstall only replace or delete files UniCast created.
- Like every Omarchy plugin, the widget runs unsandboxed in the shell: review
  the code before enabling it.

Details and how to report a vulnerability: [SECURITY.md](SECURITY.md).

## Development

```sh
tests/run    # manifest and pin checks, fmt, clippy, tests, QML parse, scripts
```

The widget talks to the daemon over a unix socket. You can send it the same
commands:

```sh
omarchy-castd --client discover
omarchy-castd --client connect <tv-ip> ~/Videos/movie.mkv
omarchy-castd --client status
```

Release notes: [CHANGELOG.md](CHANGELOG.md).

## Support

Report bugs at [github.com/liberuum/unicast/issues](https://github.com/liberuum/unicast/issues),
with the output of `journalctl --user -u omarchy-castd`.

## License

MIT, see [LICENSE](LICENSE). Not affiliated with Google, Apple, Samsung or LG.
Chromecast and Google Cast are trademarks of Google LLC; AirPlay is a
trademark of Apple Inc.
