# Changelog

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
