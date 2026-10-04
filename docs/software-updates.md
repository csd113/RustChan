# Software updates

RustChan's **Software Updates** task is part of the existing administrator panel.
Only full administrator sessions can check releases, see availability or inspect
transaction/backup state. Public accounts and anonymous visitors have no access.
`/readyz` retains its existing status contract and adds `version`, the **running
package version**. It never reports release discovery or update transactions.

## Boundary and supported deployments

Eligible native Linux GNU x86_64 and aarch64 installations use `rustchan-cli`
for the application, lifecycle guardians and selected full update controller.
The existing ordinary account must own the replaceable executable, its writable
installation directory and the private data directory. Paths must be free of
symlinks, hardlinked files, special permission bits and foreign write access.
Protected executables, root invocations, macOS, Windows and containers expose
release checks and use the deployment's manual upgrade procedure.

Start with the usual `rustchan-cli --data-dir /absolute/path/to/data serve`.
The same invocation supervises its children and retains the exact original
executable before permitting application startup. The data directory's private
`.software-updates` state holds validated versions, durable journals and rollback
snapshots. Keep this state with the data directory. The first adoption requires
all older, uncooperative RustChan processes to be stopped beforehand.

Local control authenticates the actual account, process ID and kernel start
identity of each selected child. Closed operations cannot supply a path, URL,
command, service or environment. Existing session-scoped CSRF and same-origin
checks apply to administrator POSTs. Installation additionally requires the
current administrator password and typing INSTALL. Approvals expire after ten
minutes and are consumed durably before installation starts. OS file locks
serialize installation, settings restarts and consistent snapshots.

The official Ed25519 public verification key is embedded in source builds.
Installing an official prebuilt release replaces local source modifications;
application data and configuration are retained. Same-account supervision
protects against accidental races and ordinary descendant loss. It does not
create a security boundary against malicious code already running as that
account and able to modify its installation or data.

A durable pre-spawn lifetime record and guardians track every admitted writer.
Normal updates and recoverable failures restart RustChan. Exceptional loss of
complete descendant-exit proof can require a **host reboot**. A missing PID,
free lock, process restart or container recreation cannot substitute for that
proof. After a real changed kernel boot, the retained committed controller
recovers the journal before permitting application migrations or public traffic.
Keep independent backups before recovery and preserve logs for diagnosis.
Native reboot and power-loss behavior has not yet been validated; that testing
is being handled separately from the 1.6.6 release.

## Manual upgrade of a source install

Compiling from source is supported. The controlled single-binary layout begins
with 1.6.6; earlier installations require one stopped, manual adoption. New signed
packages declare minimum updater 1.6.6 rather than accepting controllers whose
startup and handoff protocol cannot run them. An unexpected source rebuild after
retention is preserved and refused before data writes; use an explicit stopped
adoption and retain the matching full backup instead of bypassing its byte check.

For an existing source installation:

1. Identify the running executable, its **absolute data directory**, service
   identity, working directory, environment and launch arguments. Without an
   explicit `--data-dir`, data defaults to `rustchan-data` beside the executable;
   moving the executable can otherwise look like a fresh installation.
2. Build the chosen release in a separate checkout with
   `cargo build --locked --release --bin rustchan-cli`. Keep the running source tree,
   executable and data untouched during compilation. Alternatively use the
   matching official platform release and verify it using an independently
   authenticated signing key. Retain the old executable for rollback.
3. Stop the application through its existing service manager. Confirm it has
   exited, then copy the **entire** data directory to a separate private backup,
   preserving ownership and modes. Include SQLite WAL/SHM files if present,
   settings, boards/media, runtime, Tor identity, TLS state and any configured
   external persistent paths. A database-only backup cannot restore an upgrade.
4. If installing the already published **1.6.5 over 1.6.0**, perform the offline
   preparation below against the stopped database, after that full backup.
   Releases with the corrected migration perform this step during startup.
5. Replace the executable at its existing location, retaining its owner and
   executable mode. Start with the same identity, environment and arguments;
   use the same absolute `--data-dir`. Check `/readyz` reports the chosen running
   version, administrator login works, and existing boards/posts/media remain
   available. Run `admin db-status` against that same data directory to verify
   the resulting schema. Inspect startup logs before reopening public access.
6. If any verification fails, stop the new process. Retain the failed state for
   diagnosis, restore the **matching complete stopped data backup and old
   executable together**, and restart the old service. Verify its readiness and
   administrator login. Never run the old binary on the migrated database or
   manually lower `schema_version`.

### Preparing 1.6.0 for the published 1.6.5 release

The published 1.6.5 executable fails startup on a canonical 1.6.0 database with
`missing index idx_posts_board_ip_created; missing index idx_posts_thread_live`.
Its repair allowlist omitted the 1.6.0 package stamp, although both indexes are
already recognized additive repairs. The release signature and package remain
valid; this is a database migration compatibility error.

For this **specific** error and baseline, stop the application and take the full
backup above. An operator with Python 3 can add the two indexes to the stopped
database using the following command, substituting its actual absolute path:

```sh
python3 - /absolute/path/to/rustchan-data/chan.db <<'PY'
import sqlite3
import sys
from pathlib import Path

path = Path(sys.argv[1])
if not path.is_absolute() or not path.is_file():
    raise SystemExit("Expected the existing stopped database's absolute path")
with sqlite3.connect(path) as db:
    db.execute("BEGIN IMMEDIATE")
    if db.execute("SELECT version FROM schema_version").fetchall() != [("1.6.0",)]:
        raise SystemExit("This preparation applies only to a 1.6.0 database")
    if db.execute("PRAGMA integrity_check").fetchall() != [("ok",)]:
        raise SystemExit("Database integrity failed; do not upgrade")
    if db.execute("PRAGMA foreign_key_check").fetchall():
        raise SystemExit("Foreign-key validation failed; do not upgrade")
    for name, sql in (
        ("idx_posts_thread_live", "CREATE INDEX idx_posts_thread_live ON posts(thread_id, id)"),
        ("idx_posts_board_ip_created", "CREATE INDEX idx_posts_board_ip_created ON posts(board_id, ip_hash, created_at DESC)"),
    ):
        if db.execute("SELECT 1 FROM sqlite_master WHERE name = ?", (name,)).fetchone():
            raise SystemExit("Index already exists; inspect the schema instead of applying this preparation")
        db.execute(sql)
    # Commit both additive indexes together. Do not change rows or schema_version.
PY
```

Keep the application stopped between preparation and replacement with 1.6.5.
The old executable's exact schema verifier also rejects the newer indexes, so
returning to 1.6.0 requires the complete pre-preparation backup. If a different
schema mismatch is reported, restore that backup and investigate; do not add
arbitrary indexes, drop constraints or edit the version stamp to bypass checks.
The unchanged official Linux ARM64 1.6.0 and 1.6.5 binaries have been tested on an
isolated Linux fixture through this route, including administrator authentication,
post/config/media/private-file preservation and full data-and-binary rollback.
Adopt the controlled single-binary layout only after the old invocation is stopped.

The published 1.6.5 native manifest also advertises minimum updater 1.6.0, but its
managed startup requires the newer `Started` control operation, which the 1.6.0
updater cannot recognize. Keeping an old 1.6.0 updater while replacing only the
managed application with 1.6.5 is therefore not a supported transition. The
manual source procedure above uses ordinary unmanaged startup and does not
exercise that separate control protocol. The 1.6.6 candidate implements the
single-binary layout, advertises protocol 2 and requires updater 1.6.6 in new
signed packages. Actual kernel reboot/power-loss validation remains separate
follow-up work and has not been established by process-loss fixtures.

## Adopting an earlier managed deployment

Take the complete stopped backup described above and retain the old application
and its matching controller for rollback. Remove the previous deployment's
managed-controller environment setting before starting the new ordinary-account
invocation. Keep the same absolute data directory, account, working directory,
environment and port arguments. An existing service manager may launch this one
executable; desktop source launches work through the same foreground invocation.
Protected installations remain deployment-managed until the operator deliberately
prepares an eligible layout. The application never changes ownership or gains
privileges to adopt protected files.

The separate updater units and polkit examples under `deploy/systemd` describe
historical 1.6.0–1.6.5 deployments. They are not the single-binary source setup.
Do not mix an old controller with a new application merely by changing its
version link. The old deployment must be fully stopped before adoption.

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
the expected recorded schema and database integrity. The selected newest full controller must initialize before success is persisted.
Only after the durable terminal journal commits does it become active, publish
the new executable at the original source path and open public admission. Public requests fail closed during source activation/recovery;
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
Keep an offline recovery copy and never commit it. The source build embeds the reviewed final 32 Ed25519 public-key bytes. Public
release artifacts provide the hex and PEM encodings for independent verification. The signing tool validates that the public DER has the
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
packaging/signing regressions. Native kernel reboot and power loss during activation still require a disposable
Linux staging deployment before production rollout. Container process tests do
not establish those boot guarantees.

## Troubleshooting

A failed check leaves the imageboard operational; retry later for GitHub outages
or rate limits. A release missing signatures/target assets or requiring an
unsupported schema/updater stays non-installable. Recheck after operator upgrades
or expired approvals. Resolve insufficient disk space before retrying; snapshots
include media and can be large. Never relax path ownership to fix an IPC failure.

For failed rollback, keep RustChan stopped and inspect updater logs/journal and
immutable snapshots as the operator. Correct permissions, space or snapshot
integrity issues before restarting RustChan. If descendant exit remains uncertain,
restart the host before journal recovery. No web action can force
an unverified restore or clear failed recovery. Keep independent manual/offsite
backups; native rollback snapshots protect the local transaction, not disk loss.

## Settings restarts

[Administrator settings restarts](settings-restarts.md) reuse the same selected
controller, transaction lock, durable journal and recovery admission. Their
rollback restores configuration only. The controller comes from the installed
1.6.6-or-newer executable. Software installation retains its password, approval
and persistent-state snapshot protections.
