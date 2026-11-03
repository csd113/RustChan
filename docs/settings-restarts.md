# Applying administrator settings

Saving a form validates and atomically publishes settings; it does not restart
RustChan. **Configuration restart** shows whether saved startup settings differ
from the running process. Review the active, saved, and next-start values, then
stop and start RustChan using your usual launcher or service manager. For
containers, use `docker compose stop` and `docker compose start` with the same
persistent volume. Unsaved edits are not included.

Environment and launcher overrides still take precedence. Secret rotation can
invalidate sessions, and listener changes can require a new public address or
proxy/firewall changes. Keep a private copy of working settings before changing
startup controls. If new settings prevent startup, stop the process and restore
that settings copy; do not revert the database or media for a settings-only
problem. Older unfinished restart journals require matching-version recovery;
see [software updates](software-updates.md#retiring-an-older-updater).

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

## Graceful shutdown

Normal Ctrl-C, SIGTERM, and service-manager stops close admission and cancel
listeners, media workers, scheduled tasks, and Tor. Accepted HTTP requests have a
ten-second drain window; tracked persistent work shares a 60-second shutdown
budget. Interrupted durable jobs retain startup reconciliation, and a final WAL
checkpoint is attempted. Keep the service/container stop allowance at 90 seconds.
Confirm exit before replacing settings, data, or the binary.
