# Security

## Reporting a vulnerability

Please report security issues privately through GitHub's vulnerability
reporting for this repository:
<https://github.com/liberuum/unicast/security/advisories/new>.
Do not open a public issue for something exploitable. You should hear back
within a week.

## Scope and threat model

- The widget runs unsandboxed inside the Omarchy shell, like every Omarchy
  plugin. The daemon runs as your user.
- Untrusted input arrives from the local network: SSDP/mDNS announcements,
  UPnP device descriptions, receiver status messages, and HTTP requests to the
  media server. All of it is treated as data; nothing from the network is ever
  passed to a shell.
- The media server only listens on the LAN address, serves a single file per
  cast under an unguessable token, and only to the receiver's IP.
- Receivers must have a private or link-local IPv4 address; anything else is
  refused, whether typed in, discovered, or found in the settings files.
- Privileged actions, each confirmed with you: setup installs missing packages
  with `omarchy pkg add`, and casting may run `pkexec ufw allow from
  <receiver-ip> proto tcp to <your-lan-ip> port <port>`. A rule you already
  have for that traffic (allow or deny) is never replaced, and only rules
  ufw reports as newly added are recorded and later deleted.
- The daemon and scripts act only on what UniCast created: no process is
  signalled unless it is the installed `omarchy-castd` binary, and a busy
  media port is reported, not reclaimed. Setup replaces, and uninstall
  deletes, `~/.local/bin/omarchy-castd` only if its SHA-256 is the one setup
  recorded; the client rewrites, and uninstall deletes,
  `omarchy-castd.service` only if it is a regular file carrying UniCast's
  marker line; the build cache is removed only if setup created it, and
  settings only as the named files the daemon writes.
- Untrusted structured input is bounded in work as well as size: AirPlay's
  binary plist replies are read as a capped event stream, never built as a
  tree. Files are opened non-blocking and must be regular files, so a FIFO
  cannot stall the daemon.
- Files named by other programs (MPRIS players) are cast only if they are
  audio or video.

Marketplace listing, automated scans and this document are not a security
audit.
