# Software update integration audit

Reference: RustPost commit `4fe628fa4f0270ebceecb73b2f01e325942676e8`.

* RustChan builds `rustchan-cli`, embeds templates/static files, and defaults to
  `chan.db`, `settings.toml`, `boards`, and `runtime` under its data directory.
  Native update setup must pin these paths; arbitrary/custom layouts stay
  deployment-managed. Runtime includes Tor identity, TLS material and managed
  assets. Media reconciliation can mutate files during startup, so rollback
  snapshots must preserve boards/runtime as well as database/configuration.
* Schema versions are semantic release strings, not RustPost's integers. Existing
  initialization verifies/migrates schema transactionally. Manifests declare an
  explicit semantic schema range and target schema; readiness uses the new
  application's own schema probe. The updater checks SQLite integrity and
  foreign keys independently and never runs reverse migrations.
* Only `admin_sessions` authorize administrator routes. Public viewer/account
  preferences grant no administrator privileges. All administrators currently
  have full site authority; there is no reduced administrator/moderator role.
  Existing session-scoped CSRF, same-origin checks, login lockout and credential
  confirmation are reused without widening authorization.
* Admin uses one task workspace, a section index, disclosure panels, existing
  state pills and responsive forms. Baseline Chromium navigation test passed;
  desktop Accounts and mobile Overview screenshots were manually reviewed.
  Software Updates belongs in this same shell, with snapshots in Backups.
* Existing manual/scheduled/maintenance backups use rustchan-backup-v4 manifests
  and private files. Updater-owned rollback snapshots must remain inaccessible
  to the web identity, including deletion/restoration. Their verified metadata is
  integrated through authenticated IPC into the existing Backups workspace;
  manual restore controls retain their existing protection.
* MaintenanceGate serializes web maintenance/background work. Native updater
  coordinates by stopping the fixed service before its final snapshot; HTTP
  mutations fail closed while activation/recovery is in progress. Discovery does
  not stop the service or block writes. Startup must consult updater recovery
  before opening/migrating the database or reconciling persistent files.
* Health endpoints are `/healthz` and `/readyz`, not `/ready`. Readiness is public
  and minimal by default, with optional existing operator details. Add only the
  running version; no update availability/metadata enters public responses.
* Native Linux deployment documentation currently uses systemd with an
  unprivileged rustchan identity; containers run UID 10001 and persist /data.
  Containers/macOS/Windows remain check-only. Unix socket peer UID checks and
  fixed service actions are Linux-native only.
* Releases use v<package version> and preserve ZIP archives for Linux x86_64,
  Linux aarch64, Apple Silicon and Windows x86_64. Signed update artifacts are
  additional Linux assets; the updater remains operator-managed. Container latest
  currently follows main; prerelease tags must not advance latest.
* Existing seven-project Playwright suite has isolated runtimes, real admin forms,
  diagnostics, theme/mobile/no-JS coverage. New browser coverage uses that harness.
