# Software updates

The administrator's **Software Updates** task checks official stable releases
and displays verified installation, snapshot, and transaction status. It requires
an administrator session. `/readyz` reports the running package version, without
release availability or transaction details.

## Supported deployments

Native Linux GNU x86_64 and aarch64 installations can use the same `rustchan-cli`
executable for the application, guardians, and update controller. Eligibility
requires the existing ordinary account to own the replaceable executable,
writable installation directory, and private data directory. Paths must exclude
symlinks, hardlinked files, special permission bits, and foreign write access.
Root invocations, protected installs, containers, macOS, and Windows remain
check-only and use their deployment's upgrade procedure.

Start with `rustchan-cli --data-dir /absolute/path/to/data serve`. Eligible
invocations supervise their children and retain the exact executable before
application startup. Private `.software-updates` state contains retained versions,
durable journals, and rollback snapshots; keep it with the data directory.
Built-in installation of an official binary replaces local source modifications
while retaining configuration and application data.

Local control authenticates the selected account, PID, and kernel start identity.
Operations cannot supply arbitrary paths, URLs, commands, services, or environment.
Admin POSTs require session-scoped CSRF and same-origin checks. Installation also
requires the current administrator password and typing `INSTALL`. Approval expires
after ten minutes and is consumed durably. File locks serialize installation,
settings restarts, and consistent snapshots.

Same-account supervision protects against races and ordinary descendant loss; it
provides no boundary against malicious code already running under that account.
Exceptional loss of complete writer-exit proof can require a host reboot. A
missing PID, free lock, process restart, or container recreation is insufficient.
After a changed kernel boot, the retained committed controller recovers the
journal before migrations or public traffic. Native reboot/power-loss recovery
still needs validation on disposable Linux staging; process-loss fixtures do not
establish these guarantees.

## Manual upgrade of a source install

Use this procedure for operator-managed/check-only native installs and initial
adoption before a native controller has retained an executable. Already controlled
installs should use verified built-in release installation: replacing their
retained executable with arbitrary rebuilt bytes is rejected before data writes.
The CLI currently has no public command for adopting a new source rebuild into
existing controller state. Keep its journals, retained binaries, and matching
backups; do not delete state or bypass digest validation to force adoption.

1. Identify the running executable, absolute data directory, account, working
   directory, environment, and launch arguments. Without `--data-dir`, data
   defaults beside the executable; moving it can appear to create a new site.
2. Build the chosen release in a separate checkout with
   `cargo build --locked --release --bin rustchan-cli`, or obtain the matching
   official platform release. Authenticate any signing key independently before
   trusting signed artifacts. Retain the old executable.
3. Stop RustChan through its normal launcher or service manager and confirm it
   exited. Copy the entire data directory to a private independent backup,
   preserving ownership and modes. Include database WAL/SHM files if present,
   settings, boards/media, runtime, Tor identity, TLS state, and configured
   external persistent paths. A database-only backup is insufficient.
4. Replace the executable at its existing location, retaining its owner and
   executable mode. Start with the same account, environment, and arguments,
   including the same absolute `--data-dir`.
5. Check `/readyz` reports the chosen running version, administrator login works,
   and existing boards/posts/media are present. Run `admin db-status` with the
   same data directory and inspect startup logs before reopening public access.
6. If verification fails, stop the new process. Preserve failed state for
   diagnosis, restore the matching complete stopped backup and old executable
   together, and verify the restarted service. Never run an old binary against
   a migrated database or manually lower `schema_version`.

Current startup applies recognized older-schema repairs before verifying and
stamping the package version. Upgrade to the current fixed release rather than
applying obsolete index workarounds for earlier binaries. Unknown schema drift
must be investigated rather than bypassed. See
[database architecture](sqlite-engineering.md) for migration and recovery rules.

### Adopting an earlier managed deployment

The single-executable controller layout requires version 1.6.6 or newer.
Fully stop older application/controller processes before adoption. Keep a
complete stopped backup and matching old binaries for rollback. Remove the
previous managed-controller environment setting, then launch the new executable
with the same account, data directory, working directory, and normal arguments.
An unexpected rebuild after executable retention is refused before data writes;
the manual replacement procedure above does not override this check.

The separate updater units and polkit examples in `deploy/systemd/` describe
1.6.0–1.6.5 deployments. Do not install them for a current source layout or mix
an old controller with a new application by changing a version link. The app
never changes ownership or gains privileges to adopt protected installations.

## Verification and transactions

Discovery uses the official `csd113/RustChan` GitHub Releases API, excludes drafts
and prereleases, and orders stable versions semantically. Network deadlines,
response-size bounds, and HTTPS host/path redirect allowlists bound fetching.
Unverified or incompatible releases cannot enable installation. The official
Ed25519 verification key is embedded in source builds.

A signed deterministic JSON manifest binds release ID, version, target triple,
package/executable sizes and SHA-256 hashes, archive format, minimum updater,
and database migration range. The archive contains exactly one regular
`rustchan-cli` entry. Validation rejects traversal, absolute paths, links,
duplicates, unexpected entries, GNU/PAX metadata, hidden trailing contents, and
unbounded expansion. ELF architecture and executable metadata are checked.
Staging never extracts over the live installation.

Before outage, `VACUUM INTO` creates and verifies a live database/config snapshot.
After stopping RustChan, the controller refreshes it and preserves boards and
private runtime state, including Tor/TLS identity and startup-mutable media.
Hashes, inventory, SQLite integrity/foreign keys, and configuration types are
verified. Private pre-upgrade snapshots appear in Backups but cannot be deleted
or restored by the web process. They differ from downloadable application
backups. Retention keeps the active rollback snapshot and at least two recent
snapshots; the immediately previous executable is retained after success.

Activation intent is journaled before atomic version-link replacement. Startup
runs normal migrations/reconciliation. The controller checks bounded readiness,
expected running version, recorded schema, and integrity, then initializes the
new full controller. Success is committed durably before publishing the new
executable at the original path and opening public admission. Discovery/download
verification leaves ordinary writes available; activation and recovery fail closed.

On activation, migration, startup, or health failure, the controller stops the
service and restores database, configuration, boards/runtime, and executable
version together. It never reverse-migrates. Interrupted restoration replays the
same immutable snapshot on restart. Failed recovery blocks startup and preserves
state for the operator. Never delete the journal to bypass recovery.

JavaScript clients reconnect to the canonical Software Updates task after an
outage. Without JavaScript, reload that task to inspect the persisted result.

## Release signing

Generate an Ed25519 key with OpenSSL:

```sh
umask 077
openssl genpkey -algorithm ED25519 -out rustchan-update-key.pem
openssl pkey -in rustchan-update-key.pem -pubout -outform DER -out rustchan-update-public.der
```

Store the private PEM in the repository secret `RUSTCHAN_UPDATE_SIGNING_KEY`;
keep an offline recovery copy and never commit it. Source builds embed the
reviewed final 32 public-key bytes. Public release artifacts distribute hex/PEM
encodings. Rotation requires an explicit maintainer change, never an HTTP setting.

[`tools/update_package.py`](../tools/update_package.py) creates deterministic
USTAR/gzip payloads, validates ELF/release identity, and signs manifests. It reads
Cargo's version without executing cross-built binaries. `rustchan-cli --update-info`
prints build/schema/target protocol metadata without creating data directories.
New packages require updater 1.6.6; review the declared schema range on migration
changes (minimum supported baseline 1.5.0).

The release workflow retains platform ZIPs and publishes signed Linux native
packages. It obtains an immutable release ID through a draft, verifies/signs
both targets, and publishes after signing succeeds. Failure leaves a draft.
Prereleases omit native update artifacts and cannot advance stable latest.
Platform ZIPs and container images use their separate deployment procedures.

Run the offline package/signature regressions with:

```sh
python3 -m unittest discover -s tools -p test_update_package.py
```

## Troubleshooting

GitHub outages/rate limits leave the imageboard operational; retry checks later.
Missing signatures/assets, incompatible schema/updater requirements, expired
approvals, or insufficient snapshot space prevent installation. Snapshots include
media and can be large. Never relax ownership checks to repair IPC.

For failed rollback, keep RustChan stopped and inspect private journals, logs,
and immutable snapshots. Correct space, permission, or integrity problems before
retrying recovery. If writer exit is uncertain, reboot the host before journal
recovery. No web action forces an unverified restore or clears failed recovery.
Keep independent/offsite backups; local rollback does not protect against disk loss.

[Settings restarts](settings-restarts.md) share controller locking and recovery
admission but restore configuration only. Software installation retains its
password, approval, signature, and full persistent-state snapshot requirements.
