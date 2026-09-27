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
  <receiver-ip> proto tcp to <your-lan-ip> port <port>`. Only rules the daemon
  added are ever deleted.
- The daemon and scripts act only on what UniCast created: no process is
  signalled unless it is the installed `omarchy-castd` binary, and a busy
  media port is reported, not reclaimed.
- Files named by other programs (MPRIS players) are cast only if they are
  audio or video.

Marketplace listing, automated scans and this document are not a security
audit.
