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
- The only privileged action is an optional `pkexec ufw allow` for the
  receiver's IP, which polkit confirms with you.

Marketplace listing, automated scans and this document are not a security
audit.
