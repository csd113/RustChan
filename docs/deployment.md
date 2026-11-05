# Put RustChan on a server

Start by running a local site with the [setup guide](../SETUP.md). This guide is
for keeping it running on a server and allowing other people to visit. Docker
users can start with the [container guide](containers.md).

[All guides](../README.md#documentation) · [Settings](settings-restarts.md) · [Security](../SECURITY.md)

## Before allowing visitors

Complete the administrator setup through trusted local access. Choose the site's
public address, set up HTTPS for public web access, and keep the app's backend
private if you use a proxy. Run RustChan as an unprivileged account and arrange
[backups](backups.md) before storing content you need to keep.

Native defaults listen on all interfaces and enable Tor. The local-trial
commands in the setup guide override both. Decide which listeners you intend to
expose, then configure the app, firewall, and proxy consistently. `localhost`
addresses in these docs refer to the machine where the app is running.

## Linux service setup

This example uses systemd, `/usr/local/bin/rustchan-cli` for the executable, and
`/var/lib/rustchan` for data. Run the install commands from the folder containing
your verified downloaded binary (or a binary you built yourself):

```sh
sudo useradd --system --home /var/lib/rustchan --create-home --shell /usr/sbin/nologin rustchan
sudo install -o root -g root -m 0755 rustchan-cli /usr/local/bin/rustchan-cli
sudo install -d -o rustchan -g rustchan -m 0700 /var/lib/rustchan
```

Create `/etc/systemd/system/rustchan.service`:

```ini
[Unit]
Description=RustChan
After=network-online.target
Wants=network-online.target

[Service]
User=rustchan
Group=rustchan
WorkingDirectory=/var/lib/rustchan
StateDirectory=rustchan
StateDirectoryMode=0700
Environment=CHAN_BIND=127.0.0.1:8080
Environment=CHAN_TOR_SUPPORT=false
ExecStart=/usr/local/bin/rustchan-cli --data-dir /var/lib/rustchan serve
Restart=on-failure
RestartSec=5
TimeoutStopSec=90
SendSIGKILL=no
UMask=0077
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=full
ProtectHome=true

[Install]
WantedBy=multi-user.target
```

Start it and read the logs:

```sh
sudo systemctl daemon-reload
sudo systemctl enable --now rustchan
sudo journalctl -u rustchan -f
```

This example starts a local HTTP listener with Tor off. For a remote server,
forward it through SSH from your own computer, replacing `user@server`:

```sh
ssh -L 8080:127.0.0.1:8080 user@server
```

Keep that connection open and complete <http://localhost:8080/setup> locally.
Then configure public access below. If local port 8080 is occupied, choose
another local forwarding port and use that port in your browser.

Use `sudo systemctl edit rustchan` for environment overrides, then reload and
restart the service. The account needs write access to data and custom storage;
`ProtectHome=true` prevents access to files under home directories. The binary
stays root-owned and is replaced by the operator during [updates](software-updates.md).

## Reverse proxy

A reverse proxy, such as nginx or Caddy, can provide HTTPS and forward requests
to RustChan's local HTTP listener. Configure it to:

- Preserve the visitor-facing `Host` and supply the correct forwarded protocol.
- Set `CHAN_BEHIND_PROXY=true` for RustChan and set `CHAN_TRUSTED_PROXY_CIDRS`
  to only the proxy's actual addresses or network.
- Keep the backend inaccessible to untrusted clients. Trust forwarded client
  addresses only from the configured proxies.
- Forward request bodies with `Content-Length`, removing chunked transfer
  encoding. RustChan rejects `Transfer-Encoding` at the application boundary.
- Allow the upload sizes you intend to accept and keep private settings and
  runtime files outside the proxy's public file roots.

Forms require their CSRF token and a matching `Origin` or `Referer`, or
`Sec-Fetch-Site: same-origin`. Keep RustChan's `same-origin` referrer policy;
replacing it with `no-referrer` can break native form uploads. Referrers are
already omitted when navigating to another site. Header limits are 32 KiB per
value and 64 KiB in total.

Set `public_hosts` for the built-in HTTP-to-HTTPS redirect listener when needed;
it is not a general substitute for access control at the proxy or firewall.
See [settings](settings-restarts.md) for overrides and restart requirements.

## Built-in HTTPS

You can use RustChan's HTTPS listener instead of terminating TLS at a proxy.
It starts disabled. Configure certificate/key paths in `[tls]` and set
`enabled = true`. Default builds can generate self-signed certificates for local
testing; use valid trusted certificates for a public web site.

Automatic ACME certificates need a build with `--features tls-acme` and
configured `[tls.acme]` settings. Official native ZIPs and the standard container
use default features, so they do not include ACME support.

These settings have different effects:

| Setting | Effect |
| --- | --- |
| `[tls].enabled = true` | Adds HTTPS; normally keeps the main HTTP application listener |
| `[tls].require_https = true` | Disables public plaintext application access |
| `[tls].redirect_http = true` | Runs a redirect listener on `tls.http_port` |

The default HTTPS port is 8443 and redirect port is 8080. Use non-conflicting
ports and the intended public hosts. With Tor enabled, HTTPS-only mode retains
a loopback HTTP backend restricted to connections registered by the in-process
Tor proxy. Restart manually and verify readiness after changing listeners.

## Tor onion service

RustChan includes Arti, so it can host an onion service without a separate Tor
daemon. Native generated settings enable it; Docker and this guide's service
example override it to off. Enabling Tor requires outbound connectivity and
write access to `runtime/tor/` in the data directory.

For local HTTP plus onion access only:

```toml
enable_tor_support = true
tor_only = true
```

Remove or change any conflicting environment overrides, then restart. Tor-only
mode binds HTTP to loopback. In a container, that loopback is inside the
container, so it is unsuitable for a published Docker HTTP port.

Keep `runtime/tor/state/` private and back it up with its permissions. Losing it
changes the onion identity/address. A board backup does not include it; full
application backups include Tor keys only when selected. See [backups](backups.md).
An onion address or published descriptor does not prove external reachability,
and Tor does not protect visitors from an operator who controls the site.

## Health and monitoring

`/healthz` is a minimal liveness endpoint. `/readyz` checks readiness, including
the database, and reports the running package version. Detailed readiness is off
by default; `/metrics` returns 404 unless enabled.

```toml
public_readiness_details = true
public_metrics_enabled = true
```

Enable those only behind a trusted monitoring/network boundary. They reveal
database, backup, media, maintenance, and Tor operational state, but never
software-update availability or update transactions. Monitor free disk space,
logs, and failed jobs as well as readiness.

## Troubleshooting a server

| Symptom | Check |
| --- | --- |
| Forms or uploads fail behind a proxy | Host/origin headers, forwarded protocol, trusted proxy addresses, referrer policy, and body framing |
| HTTPS will not start | Enabled settings, occupied ports, certificate permissions/paths, and ACME feature/settings |
| Tor does not become ready | Outbound access, bootstrap timeout, and state-directory permissions |
| Onion address changed | Missing or replaced persistent Tor state |
| FFmpeg works in your shell but not the service | Service account's `PATH`, `ffmpeg_path`, and sandbox permissions |
| Settings appear ignored | Launcher/environment overrides and the admin panel's next-start values |

Use the [setup troubleshooting table](../SETUP.md#troubleshooting) for general
startup problems and [support](../SUPPORT.md) for a sanitized report.
