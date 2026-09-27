# Changelog

## 0.1.5 — 2026-09-27

### Security

The three lower-priority items left open in 0.1.4:

- The media file is opened once at connect and everything that sends its
  bytes (direct HTTP, the ffmpeg stream, embedded-subtitle extraction, seek
  keyframe lookups) reads that descriptor through `/proc/<pid>/fd/<n>`.
  Swapping the path for a symlink or another file mid-cast no longer changes
  what the receiver gets.
- mDNS browses only the LAN interface (the address the default route leaves
  from, or any private IPv4 interface when that is a full-tunnel VPN); IPv6,
  VPN, container and other interfaces are never used.
- The user unit now runs with `UMask=0077`, `TasksMax=1024`, `MemoryMax=4G`,
  `OOMScoreAdjust=200` and `KeyringMode=private`. Namespace and seccomp
  sandboxing is deliberately left out, since in a user unit it implies
  `PrivateUsers=`/`NoNewPrivileges=` and would disable the `pkexec ufw`
  firewall prompt; a test keeps it that way. Re-run setup (or stop the unit)
  for an existing install to pick up the new unit file.

## 0.1.4 — 2026-09-27

### Security

Marketplace review of 0.1.3, plus an audit for the same class of issue:

- The daemon no longer kills whatever listens on the media port. It stops its
  own previous server in-process and reports a port held by another program
  instead of reclaiming it.
- The uninstall script signals only a daemon whose executable is the binary
  it installed (was `pkill -x omarchy-castd`).
- Receivers must be private or link-local IPv4. Public, loopback, multicast
  and IPv6 addresses are refused in `connect`, `add-ip`, manual IPs and
  discovery results, and the media server refuses to bind a non-LAN address
  (for example a full-tunnel VPN).
- ufw rules are scoped to the LAN address the media server binds
  (`to <lan-ip> port <port>`, was `to any`), the ledger records receiver,
  address and port, and lines that are not a rule the daemon could have
  written are ignored instead of being passed to `ufw delete`. A matching
  rule that already existed is not recorded, so it is never deleted.
- The media server closes connections from any host but the receiver at
  accept time, before any request is read.
- ffprobe runs with a 15 s deadline and a 4 MiB output cap. ffmpeg and
  ffprobe get `file:` inputs with `-protocol_whitelist file`, and cast paths
  must be absolute.
- A file reported over MPRIS is cast only with an audio/video extension and
  an audio or video stream; the D-Bus lookup is limited to 3 s.
- Subtitle sidecars must be regular files (no symlinks) under 8 MiB, converted
  WebVTT is capped at 16 MiB, and at most 32 tracks are offered.
- The client starts and registers only `~/.local/bin/omarchy-castd` or
  `/usr/bin/omarchy-castd` (no `PATH` lookup, no in-tree binaries), refuses
  unit-file-unsafe paths, and calls `/usr/bin/systemctl`.
- File names, track titles and receiver errors are debug-escaped in logs.
- Discovery never probes this machine's own address.

### Changed

- The `PKGBUILD` is removed: it always lagged the release it described and
  built with the distro compiler instead of the pinned one. Setup is the
  supported install.
- `tests/run` builds with `--locked` and fails if any cargo build in setup,
  tests or CI does not; the CI cache is written only from `main`.
- README "What it touches" now lists package installs, the login-enabled user
  unit, the crate cache left behind, the unverified Cast TLS certificate and
  the environment variables.

## 0.1.3 — 2026-09-26

### Fixed

- Google Cast: films with 5.1 (or any multichannel) audio stopped a moment
  after starting — the receiver rejects multichannel AAC with
  `MEDIA_SRC_NOT_SUPPORTED`. Such soundtracks are now downmixed to stereo AAC
  (dialogue-forward), with or without audio boost.
- Receiver errors that end a Cast session are now logged.

### Security

Hardening against a hostile or broken device on the LAN (marketplace review
of 0.1.2):

- DLNA SOAP replies and AirPlay `/playback-info` are read with size caps
  (256 KiB / 64 KiB) instead of being buffered whole; device descriptions
  already had a 1 MiB cap.
- Google Cast: frames over 64 KiB are refused before `rust_cast` allocates
  for them, and a receiver flooding the channel with unrelated messages
  (256 frames / 30 s while a reply is awaited) ends the session instead of
  growing an unbounded buffer.
- The media server admits nobody when its allowlist is empty (fail closed),
  runs at most two live ffmpeg streams per session (the oldest is ended to
  make room), and stops retrying subtitle tracks ffmpeg already failed on.
- Discovery accepts at most 4 SSDP description URLs per replying host and
  64 per scan, 32 mDNS receivers per scan, probes at most 8 hosts at once
  under a 15 s deadline, and ignores mDNS addresses outside private and
  link-local ranges (no probes to loopback or the internet).
- Positions and durations reported by receivers must be finite and under
  1 000 000 s; anything else reads as 0.

### Changed

- `plist` 1.10.1 and `time` 0.3.55 (drops a duplicate `quick-xml`);
  `rust-version` now matches the pinned 1.98 toolchain.

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
