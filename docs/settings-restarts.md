# Applying administrator settings

Saving a form never restarts RustChan. File settings are validated together and
published by a private, synced, atomic replacement that preserves TOML comments.
The **Configuration restart** status is visible above the admin tasks. When saved,
effective startup settings differ from the running process it says:

> Settings saved. Restart RustChan to apply these changes.

Review the active/saved/next-start values, then choose **Restart RustChan**. This
separate POST action includes only the existing signed CSRF token. Unsaved form
edits are not included. Unchanged submissions and values masked by environment or
launcher overrides do not create restart warnings. Pending changes survive saves
in other sections. A trial remains in progress until the replacement passes
readiness; accepting a request alone never completes it.

JavaScript disables repeated clicks and polls the administrator-only status
endpoint for up to four minutes, showing rejection, timeout, reconnect and final
verification/recovery messages. A lost acknowledgment is treated as uncertain:
check durable status before retrying. Without JavaScript the same POST form,
server-rendered status and refresh link work. Secret rotation can invalidate the
administrator session; sign in again to see the result. Listener/host changes may
require navigating to the new public address and updating external firewall or
proxy settings; a restart cannot change those deployment settings.

## Classification

The server's `SettingDefinition.application` registry is authoritative. Browser
flags never decide whether a restart is needed. Runtime controls are immutable
`CONFIG` reads or initialize the listener, executor, database pool, tool discovery,
logging or recurring worker schedule. Existing live controls retain their existing
save paths.

| Application | Controls |
| --- | --- |
| Live | Site name/subtitle; activity badges; default NSFW visibility; default and enabled themes, custom themes and CSS; favicon; banner content, rotation and external-link policy; board identity, ordering, access, upload/posting/display/pruning policies; global active-media pruning and cap; FFmpeg subprocess timeout; automatic full-backup interval, retention, Tor-key inclusion, storage format and split-part size |
| Restart | All Network & Security controls: proxy trust/CIDRs, public hosts, secure-cookie policy, HTTP port/bind address, browsing allowance/window/policy, admin session lifetime, public readiness detail and metrics |
| Restart | All Tor controls: enablement, Tor-only mode, bootstrap timeout, concurrent streams and service nickname |
| Restart | File-backed Media controls: default image/video/audio limits, arbitrary-file global gate, archive-before-prune, FFmpeg requirement/path and capability detection, thumbnail size, job-queue capacity, waveform cache cap, every managed-media reconciliation toggle/interval/budget |
| Restart | Maintenance schedules: WAL checkpoint interval, vacuum interval, poll cleanup interval and database size warning threshold |
| Restart | Executor blocking threads and SQLite pool size |
| Restart | Every native HTTPS/TLS leaf: enablement, HTTPS-only policy, HTTPS/redirect ports, redirect toggle, ACME enablement/domains/email/staging/cache, manual certificate/key paths |
| Restart | Failed-login and board-password allowances/windows, board access-cookie lifetime and signed self-action window; board index page size/reply previews; log filter; normal read/write deadlines |
| Restart | Saved backup directory and cookie-secret rotation. Storage data/database/upload paths remain operator-managed and do not migrate data merely by changing a path. |

Startup seed aliases for database-owned appearance/board/pruning controls do not
compete with existing database values. Environment and launcher overrides remain
authoritative at startup. Existing sessions retain their stored expiry when only
session lifetime changes. Manual database actions and content/moderation operations
continue to apply immediately.

## Native Linux

Eligible installations use the same `rustchan-cli` invocation and ordinary account
for application supervision and the full update controller; see
[Software Updates](software-updates.md) for adoption and ownership requirements.
Private local IPC authenticates the exact selected application process. The
Restart operation contains only the administrator ID and server-generated
running instance. It cannot name a command, executable, arguments, service,
environment or filesystem path. Software installation still requires password
reauthentication, an expiring one-use approval and verified signed release bytes.

The updater's existing `update.lock`, durable status phases and startup/recovery
admission also own settings restarts. A shared `.settings.lock` file serializes all
configuration saves with service transitions. Restart preflight closes HTTP write
admission; the existing maintenance gate prevents backup/restore overlap. Duplicate
requests, stale process identities and replay of an already consumed restart are
rejected. Software update and restart workers cannot own the service concurrently.
The readiness observation binds a new process identity to exact startup-file bytes;
its private digest is never returned in public health or admin status.

The updater retains the last ready configuration in its own private state directory.
After gracefully stopping the service it starts the same executable, requires the
fixed loopback `/readyz` to report the expected version, healthy database and a new
process identity in the nonsecret `X-RustChan-Instance` readiness header, and checks that the replacement loaded the requested settings.
Startup and health checks are bounded to 60 seconds. Keep the effective HTTP port reachable at 127.0.0.1. Original launcher port
overrides continue to apply to replacement processes. Changes that remove that probe path cannot
commit, including HTTPS-only configurations without a suitable local backend.
Use a reverse proxy when public TLS would otherwise remove the local probe path.

Failed or interrupted settings startup enters the existing updater recovery
barrier, stops the application before restoring the verified last healthy file,
then starts and checks the previous configuration. This is configuration-only
recovery: it does not revert SQLite, posts, media or software. Software updates
continue to use their complete persistent-state snapshot rollback. A settings
rollback restores the whole last healthy settings file, including file-owned
backup defaults; database-owned live changes remain persisted. If previous
configuration recovery also fails, admission stays closed and the private payload
is retained for operator recovery. Configuration files and rollback payloads must
be regular files without symlinks/hardlinks.

## Containers

The shipped Compose deployment sets `RUSTCHAN_RESTART_ON_EXIT=1` together with
`restart: unless-stopped` and a 90-second stop grace period. For `docker run`, pair
`--restart unless-stopped --stop-timeout 90` with
`-e RUSTCHAN_RESTART_ON_EXIT=1`. The image alone does not opt in: the process cannot
inspect Docker's restart policy without access to Docker administration, which it
never receives. Do not opt in without a supervisor that restarts the process after
normal exit. Keep the data volume mounted and its private permissions intact.

On explicit admin restart the process writes private intent, blocks further file
saves, drains HTTP/workers and exits. Docker starts its replacement. Private state
under `runtime/settings-restart` gives the new instance one startup trial; it must
initialize the database/listeners and pass the existing loopback HTTP readiness
probe at CHAN_PORT before committing. A failed or interrupted trial restores the
last healthy file on the next supervised startup, before logging, immutable config
or the database is initialized. A failed recovery remains blocked for operator
repair rather than repeatedly trying an unverified candidate. Docker's existing
health check remains useful for deployment monitoring; a health label alone does
not restart containers. Ordinary `docker exec ... admin` commands do not consume
or roll back a pending startup trial.

Containers keep software images deployment-managed. The settings restart action
never installs an image or invokes Docker. Native update IPC remains disabled
inside containers. Keep the fixed mapped HTTP health port available; HTTPS-only
cutovers or alternate unmapped listener ports fail the health check and recover.

## Shutdown and unsupported deployments

Shutdown closes admission to new work and stops listeners, allowing accepted HTTP
requests ten seconds to finish. Background media workers, scheduled tasks and Tor
receive their existing cancellation token and share one 60-second shutdown budget.
Tracked persistent operations drain within that budget; interrupted durable jobs
retain the existing startup reconciliation path. A final SQLite WAL checkpoint is
attempted, database resources are released, the Tokio blocking pool has a further
ten-second drain cap, and file logging guards flush queued output. SQLite's
transaction/WAL recovery remains authoritative if an operation exceeds the budget.

The [service example](../SETUP.md#linux-service-setup) uses `TimeoutStopSec=90`
and `SendSIGKILL=no`. A stuck service causes control/recovery to fail closed;
the controller does not restore files
under an unconfirmed stopped process. No process-name matching, pkill, kill -9,
replacement spawning by the web process, or remote reboot operation is used.

Ineligible/protected native invocations, macOS/Windows native runs, and containers
or custom supervisors without the explicit container contract show why automatic
restart is unavailable. Eligible native Linux terminal invocations use the same
controller and support administrator restarts.
Saving still works and active/saved settings remain visible. Use a supported
supervised deployment for administrator-triggered restart, or stop/start the
standalone process through its normal launcher. No unsupported service name or
shell-command input is offered as a fallback.
