# Back up and restore your site

A backup is a copy you can use if data is lost or an update goes wrong. Start
with **Backups** in the admin panel, and keep an independent copy away from the
running site. Test a restore on a disposable installation before relying on it.

[All guides](../README.md#documentation) · [Updates](software-updates.md) · [Docker backups](containers.md#backups)

## Choose a backup

| Need | Use |
| --- | --- |
| Save site content and settings | A full-site backup in the admin panel |
| Export or restore one board | A board backup, or extract/restore that board from a saved full backup |
| Recover an entire installation or roll back an upgrade | A complete stopped filesystem copy plus its matching executable or container image |

In the admin panel, choose **Save to server** for a saved full backup. You can
also download an archive and schedule automatic full backups there. Review the
retention count and whether to include Tor identity keys. Those keys are private.
For restore, choose a saved backup or upload a supported archive from your computer.
Read the selected operation's scope before confirming: a full restore replaces
site data.

Full application backups include the database, settings, managed media and
appearance assets, and optionally Tor keys. They do **not** replace a full copy
of TLS/runtime state and every custom external path. Board backups do not
preserve the site's Tor identity.

## Make a complete filesystem copy

1. Identify the executable, data directory, and any custom database, media,
   backup, certificate, or other persistent paths.
2. Stop RustChan normally and confirm it has exited.
3. Copy the **whole data directory**, including `chan.db`, any WAL/SHM sidecars,
   settings, boards, backups, logs, runtime/Tor/TLS state, and older recovery files.
   Copy external persistent paths too, preserving ownership and private permissions.
4. Keep the matching executable or image revision with the backup record.
5. Start the site again and check it.

Never copy only `chan.db` while the site is running. SQLite may have committed
changes in its WAL file, and media files can be changing at the same time.
Use the [container example](containers.md#backups) for a Docker volume.

## Backup storage and retention

Change **Admin → Backups → backup storage directory**, or `backup_directory` in
`settings.toml`, then restart. `CHAN_BACKUP_DIRECTORY` overrides the file.
Existing backups stay in the old location; the panel lists only the active
location. Changing the path back makes existing backups there visible again.

Use a dedicated writable directory where RustChan can set private permissions.
Mount external storage before startup. In Docker, paths outside `/data` need
their own persistent mount. A copy on the same disk is not protection against
losing that disk.

Automatic retention counts usable full-site backups separately from board
exports. Incomplete or damaged backups remain available for inspection.
Pre-maintenance snapshots preserve the original database, including damage the
repair operation is meant to fix.

## Limits and temporary downtime

During the exclusive part of a backup or restore, normal requests return
HTTP 503. RustChan waits up to two minutes for requests, maintenance, and media
jobs to finish; if they cannot drain, the operation fails without changing live
data. Archive uploads arrive before this phase. Authenticated backup progress
remains available, and a disconnected browser does not release active blocking work.

Local archive uploads allow at most **8 GiB uploaded and 8 GiB extracted**.
Transfer conversion shares the extraction budget, and legacy ZIP downloads must
fit the restore budget. Board exports have a 64 MiB JSON manifest limit; larger
boards can still be recovered through a full-site restore.

For larger sites, use a saved directory or split-ZIP backup and **Restore saved
backup**. Its extraction budget comes from its verified inventory. Allow extra
disk space for archive conversion, staged files, and a live-database safety copy.
An I/O failure must not publish an incomplete backup.

Saved full backups are marked verified after payload checksum, database,
restored-post-body, and referenced-media checks. These checks do not replace an
independent recovery test.

## Interrupted restore recovery

If a restore fails, **preserve the data and recovery files**. Do not delete them
to make startup continue. Restart normally once and read the recovery result.
If startup still refuses, stop and preserve the complete directory for diagnosis.

Before replacing live data, RustChan syncs staged files and a private database
safety snapshot, then writes `runtime/restore-recovery.json`. That record points
to the pending filesystem operation and snapshot. Directory swaps are persisted
before displaced files are discarded.

If database rollback or filesystem completion fails, live work is blocked.
Startup attempts the recorded swap before accepting requests or media jobs,
verifies the database, and then clears the record. It retains a recovered safety
snapshot for inspection. A missing matching operation or failed reconciliation
stops startup and reports the snapshot location.

Manual recovery needs the recovery record, indicated database snapshot and
sidecars, and the staged/displaced directories recorded in `pending_fs_ops`.
Restore the database and matching media together before clearing the record;
removing the record alone does not finish a restore. See the
[database guide](sqlite-engineering.md#backup-and-recovery) for the underlying model.

## Developer checks

`cargo test --locked --test backup_regressions --all-features` covers
cancellation-safe draining, blocked access after failed recovery, streaming
budgets, retained rollback files, retention selection, staging cleanup, and
interrupted swap replay with small temporary fixtures.
