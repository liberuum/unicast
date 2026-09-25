# Changelog

## 0.1.2 — 2026-09-25

### Fixed

- Google Cast: a quick re-cast, or a boost change right after a seek, could
  latch onto the previous media session and end the new cast within a second.
- Google Cast: stray unparseable receiver messages no longer add up to
  "Lost the connection" over a long film; the TCP connect is bounded.
- DLNA/AirPlay: a receiver that is switched off or never starts playing now
  ends the session instead of showing "playing" forever.
- A failed seek or boost restream now ends the session with an error instead
  of leaving it stuck.
- Stop now also ends the TV's in-flight download of a directly served file.
- `systemctl --user stop` (SIGTERM) and `shutdown` stop the receiver before
  the daemon exits.
- Device names and control URLs containing XML entities (`&amp;`) were
  truncated.
- Latin-1 `.srt` subtitles lost every accented line; subtitle conversions are
  bounded and never outlive their request. Stale subtitle caches are cleared
  at startup.
- Settings and firewall-ledger writes are atomic and serialised; a declined
  `clear-firewall` keeps the rules it could not remove in the ledger.
- Widget: an IP added during a scan, and the multicast toggle during a long
  action, were silently dropped; the subtitle picker stopped following the
  daemon after the first pick; the Chromecast row icon never highlighted; a
  "service unavailable" error stayed up after the daemon recovered.
- Setup switches an existing unit to the freshly built binary; uninstall can
  no longer be undone by the widget respawning the daemon mid-removal.

### Hardening

- SSDP replies may only describe the host that sent them; device descriptions
  are size-capped while streaming, not only by Content-Length.
- Control-socket requests are size- and time-limited; network-supplied paths
  are escaped in logs; invalid byte ranges are ignored per RFC 9110.
- `pkexec` is killed if its prompt times out, so a late approval cannot add an
  unrecorded rule.
- CI: every workflow action is pinned to a full commit SHA, with read-only
  `contents` permissions and no persisted checkout credentials; `tests/run`
  rejects any `uses:` that is not SHA-pinned.
- PKGBUILD: pinned source checksum, offline `cargo fetch` in `prepare()`,
  `ufw`/`polkit` optdepends.

## 0.1.1 — 2026-09-16

- Pin an exact Rust toolchain (`rust-toolchain.toml`); `bin/omarchy-cast-setup`
  now installs that release through rustup (checksum-verified against the
  signed release manifests, `--no-self-update`) and builds the daemon with it,
  instead of activating the mutable `stable` channel. CI builds with the same
  pin and `tests/run` checks it cannot drift.

## 0.1.0 — 2026-09-12

First public release of UniCast.

- Bar widget with a popout panel: receiver discovery (SSDP, mDNS, manual IP),
  cast the current MPRIS file or a chosen file, live status, pause/resume,
  seek, stop.
- Rust backend `omarchy-castd`: `systemd --user` daemon with an embedded,
  token-protected LAN media server; DLNA/UPnP, Google Cast (CASTV2) and legacy
  AirPlay transports, routed per device.
- Codec gate per receiver: direct serve, remux, or ffmpeg transcode.
- Receiver volume and mute (Cast natively, DLNA via RenderingControl).
- Audio boost (+6/+12 dB with limiter, dialogue-forward 5.1 downmix), applied
  live; fragmented MP4 to Cast, MPEG-TS to DLNA.
- Subtitles on Cast receivers: sidecar and embedded text tracks converted to
  WebVTT, switchable without reloading, choice remembered per language.
- Immediate feedback for every panel interaction.

Tested on Omarchy 4.0.3 / quickshell 0.3.1 / ffmpeg 9.0.1 with a Chromecast, a
Samsung TU7000-series TV (DLNA) and an LG webOS TV (Cast). AirPlay is
implemented but untested.
