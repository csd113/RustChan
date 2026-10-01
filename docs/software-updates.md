# Software updates

RustChan's **Software Updates** task is part of the existing administrator panel.
Only full administrator sessions can check releases, see availability or inspect
transaction/backup state. Public accounts and anonymous visitors have no access.
`/readyz` retains its existing status contract and adds `version`, the **running
package version**. It never reports release discovery or update transactions.

## Boundary and supported deployments

The web process sends only Status, Check, Ready and Install operations over
`/run/rustchan-updater/control.sock`. Install contains a one-use opaque approval
and an authenticated administrator ID; no request supplies a path, URL, command,
service or environment. Existing session-scoped CSRF and same-origin checks apply
to POST mutations. Installation additionally requires the current administrator
password and typing INSTALL. Failures share the existing login lockout. Approvals
expire 10 minutes after a compatible release check and are durably consumed
before an installation worker starts.

The updater accepts only the configured web UID, verifies socket/file ownership,
serializes transactions using OS file locks and persists an atomic fsynced journal.
Configuration and trust keys are root-owned. The web identity cannot write the
updater executable, state, backup snapshots, version directories or active link.
Service actions are fixed argv calls to systemctl for **rustchan.service** start
and stop, with bounded execution; there is no shell or arbitrary service choice.

Native installation supports Linux GNU x86_64 and aarch64 under the supplied
systemd setup. macOS, Windows, custom layouts and containers are deployment-managed
and expose Check only. Never enable native managed mode inside a container.
Ordinary manual/scheduled backup and restore controls remain available. Native installation holds RustChan’s existing maintenance gate through the preparation transaction, preventing overlap with backup/restore while ordinary posts continue. If an install reply is lost before approval consumption can be confirmed, maintenance remains paused; inspect updater status/logs before restarting the web service to clear an unconsumed request.

## One-time Linux setup

Use a disposable staging machine first. Keep an independent full backup before
converting an existing deployment. Do not perform conversion against a running
application. Commands below assume the default `/var/lib/rustchan` layout and an
existing configured instance. Override deployments require conversion to that
layout before native updates; database/media/runtime paths cannot point elsewhere.

Create distinct locked service identities `rustchan` and `rustchan-updater` with
matching primary groups. Build both binaries with `cargo build --locked --release --bins` from the corresponding release source. Install the updater outside the version directories at
`/usr/local/libexec/rustchan-updater`, root-owned mode 0755. It is operator-managed;
software updates cannot replace their own trust/transaction engine. A release
requiring a newer updater is refused until the operator upgrades this executable.

Prepare these paths and permissions:

| Path | Owner | Mode / access |
| --- | --- | --- |
| `/etc/rustchan` | root:root | 0755, no group/other write |
| `/etc/rustchan/updater.toml` | root:root | 0644 |
| `/etc/rustchan/update-public-key.hex` | root:root | 0644 |
| `/opt/rustchan` and `versions` | rustchan-updater:rustchan-updater | 0755, no group/other write |
| `/opt/rustchan/versions/<version>/rustchan-cli` | rustchan-updater:rustchan-updater | 0755 |
| `/var/lib/rustchan-updater` and `backups` | rustchan-updater:rustchan-updater | 0700 |
| `/var/lib/rustchan` and its persistent files | rustchan:rustchan | existing RustChan private permissions |
| `/run/rustchan-updater` | rustchan-updater:rustchan | 0750 |
| `control.sock` | rustchan-updater:rustchan | 0660; peer UID must still match |

Copy the running binary into `versions/<running-version>/rustchan-cli` and create
a relative `current -> versions/<running-version>` symlink inside `/opt/rustchan`.
Preserve `chan.db`, `settings.toml`, `boards` and `runtime`; required roots must
exist and must not contain symlinks, hardlinked files or special files.
Ensure every ancestor of protected installation/state/key/config paths is owned
by root or the updater and cannot be written by the web account. Root-owned sticky
temporary parents are tolerated for isolated tests, not recommended deployment.

Install the unit files and polkit rule from `deploy/systemd`, all root-owned.
Set `web_uid` in `/etc/rustchan/updater.toml` to `id -u rustchan`, confirm
`health_port` matches a working loopback HTTP `/readyz` listener (use the reverse proxy for public TLS), and create the private backups
directory before starting the updater. Install the trusted public key **through
an independently authenticated operator channel**. A downloaded key is not trusted
merely because it appears beside release assets.

For managed updates, enabled ACME state must remain under the data directory’s `runtime/` tree; external mutable caches are rejected before stopping RustChan.

The updater's systemd capability set allows reading owner-only application files
and restoring their original owner/mode. `ProtectSystem=strict`, `ProtectHome`
and explicit writable paths confine filesystem mutation to the managed application,
state and data roots. The web service remains unprivileged with NoNewPrivileges.
The polkit rule grants only start/stop of rustchan.service to the updater identity.
Review these privileges against the host's systemd/polkit policies.

Validate with `systemd-analyze verify deploy/systemd/*.service`, then install units,
`systemctl daemon-reload`, and start the updater before the application. Managed
startup refuses to open/migrate the database or reconcile filesystem operations
until recovered updater admission matches the active running package version.
Check `journalctl -u rustchan-updater -u rustchan` and administrator readiness.

## Verification and transactions

Discovery uses the official `csd113/RustChan` GitHub Releases API, excludes draft
and prerelease versions, and orders stable tags semantically. Network deadlines,
response-size bounds and explicit HTTPS host/path redirect allowlists prevent
arbitrary fetches. Unverified or incompatible releases cannot enable installation.

Each native artifact has an Ed25519 signature over the exact deterministic JSON
manifest. It binds GitHub release ID, application version, target triple, package
size/SHA-256, executable size/SHA-256, format, minimum updater and semantic database
migration range. The archive has exactly one regular `rustchan-cli` entry. Raw tar
validation refuses GNU/PAX metadata, traversal, absolute paths, links, duplicates,
unexpected entries and hidden trailing contents. Expanded bytes are bounded and
ELF class, endianness, executable type and machine architecture are checked.
Archives are staged separately, never unpacked over the live installation.

Before outage, SQLite VACUUM INTO creates and verifies a live database/config
snapshot. After stopping the service, the updater refreshes that snapshot and
preserves **all boards and private runtime files**, including startup-mutable
media and Tor/TLS identity. Snapshot hashes, SQLite integrity/foreign keys,
configuration types and fixed file inventory are verified. Snapshots are private
and listed in the existing Backups task as verified pre-upgrade backups. They
are not interchangeable with a manually downloaded full-site backup, and the
web process cannot delete/restore them. Retention keeps the active rollback
snapshot and at least two recent snapshots; the immediately previous executable
is retained after success.

Activation intent is journaled before atomic current-link replacement. The new
service performs its usual startup migration/reconciliation. The updater checks
bounded `/readyz` health and expected running version, then independently checks
the expected recorded schema and database integrity. Success is persisted only
after these checks pass. HTTP mutations fail closed during activation/recovery;
release discovery and download verification do not block ordinary writes.

After an activation, migration, startup or readiness failure, the updater stops
the service and restores the previous database, configuration, boards/runtime and
version together. It never reverse-migrates. Restoration remains journaled until
the old service passes health checks. A crash during any restore stage causes the
same immutable snapshot to be replayed on the next updater startup. A preparation
interruption resumes the previous service; no update is marked successful merely
because its pointer changed. Failed recovery blocks startup and retains snapshots
for operator intervention. Never manually delete the journal to bypass recovery.

The administrator sees persisted progress and final success/rollback/failure.
Enhanced clients tolerate outages and reconnect to the canonical Software Updates
task. Without JavaScript, the POST redirects explain the restart; reload the same
task after reconnecting to inspect the server-side result.

## Release signing

Generate an Ed25519 key using an operator OpenSSL installation:

```sh
umask 077
openssl genpkey -algorithm ED25519 -out rustchan-update-key.pem
openssl pkey -in rustchan-update-key.pem -pubout -outform DER -out rustchan-update-public.der
```

Store the private PEM as the repository secret `RUSTCHAN_UPDATE_SIGNING_KEY`.
Keep an offline recovery copy and never commit it. The trusted key file on the
installation contains the final 32 Ed25519 public-key bytes as 64 hex characters,
not the whole DER/PEM. The signing tool validates that the public DER has the
Ed25519 prefix and emits `.hex`/`.pem` public artifacts for operator distribution.
Key rotation is an explicit operator action, never an HTTP configuration change.

`tools/update_package.py package` builds deterministic USTAR/gzip payloads for the
two supported native targets and validates ELF architecture and release identity.
It reads the package version from Cargo.toml, avoiding execution of cross-built
binaries. `rustchan-cli --update-info` prints build/schema/target protocol metadata
without creating a data directory. Review the declared minimum schema whenever
changing migrations; the initial supported upgrade baseline is 1.5.0.

CI preserves existing platform ZIP releases and adds signed Linux update artifacts.
It creates a draft release to obtain its immutable GitHub release ID, verifies
both packages, signs/round-trip verifies both manifests, and publishes only after
signing succeeds. Signing failure leaves the release draft. Prereleases omit update
artifacts, are explicitly marked prerelease, and cannot advance stable latest.
Existing GHCR main/latest and version-tag policy remains unchanged.

Run `python3 -m unittest discover -s tools -p test_update_package.py` for offline
packaging/signing regressions. Native systemd/polkit capabilities, reboot during
activation, actual migration binaries and service readiness still require a
disposable Linux/systemd staging deployment before production rollout.

## Troubleshooting

A failed check leaves the imageboard operational; retry later for GitHub outages
or rate limits. A release missing signatures/target assets or requiring an
unsupported schema/updater stays non-installable. Recheck after operator upgrades
or expired approvals. Resolve insufficient disk space before retrying; snapshots
include media and can be large. Never relax path ownership to fix an IPC failure.

For failed rollback, keep RustChan stopped and inspect updater logs/journal and
immutable snapshots as the operator. Correct permissions, space, service/polkit or
snapshot integrity issues before restarting the updater. No web action can force
an unverified restore or clear failed recovery. Keep independent manual/offsite
backups; native rollback snapshots protect the local transaction, not disk loss.
