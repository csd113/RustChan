# Software updates

**Software Updates** in the admin panel checks the official RustChan GitHub
releases and links to the download page. Only a signed-in administrator can run
a check. It compares stable semantic versions, excluding drafts and prereleases.
The server contacts GitHub only when **Check for updates** is pressed; repeated
checks within a minute reuse the last result. The form works without JavaScript.

Checks have a 15-second deadline and a bounded response size. Redirects and
API-supplied download URLs are not followed. A timeout, outage, or rate limit
shows an error and the official releases link; it does not prevent startup or
interrupt the imageboard. Check results are kept in memory until restart.

Download the matching platform binary from the linked official release. Verify
its ZIP against that release's `SHA256SUMS.txt`. The application does not download,
install, or restart itself. Container operators replace the image while retaining
their complete data volume; see [containers](containers.md).

## Manual upgrade of a source install

1. Identify the executable, absolute data directory, account, working directory,
   environment, and launch arguments. Without `--data-dir`, data defaults beside
   the executable. Keep the same path; moving the binary can appear to create a
   new site. Use the same architecture and required Cargo features.
2. Obtain the matching official release, or build it separately with
   `cargo build --locked --release --bin rustchan-cli`. Retain the old executable.
3. Stop RustChan through its normal launcher or service manager and confirm it
   exited. Copy the **complete** data directory to a private independent backup,
   preserving ownership and modes. Include database WAL/SHM files if present,
   settings, boards/media, backups, runtime, Tor identity, TLS material, and any
   configured external persistent paths. A database-only backup is insufficient.
4. Replace only the executable at its existing location, keeping its owner and
   executable mode. Start with the same account, environment, arguments, and data
   path. Startup applies supported schema migrations and verifies the structure
   before recording the running version. Templates and static assets are embedded.
5. Check `/readyz`, sign in, and inspect existing boards, posts, and media. Run
   `admin db-status` with the same data directory. Reload browser forms after the
   upgrade. Check logs before reopening public access.
6. If verification fails, stop the candidate and preserve its state for diagnosis.
   Restore the matching **complete stopped backup and old executable together**,
   then verify the service. Never run an old binary against a migrated live
   database or manually lower `schema_version`.

## Retiring an older updater

Stop the complete older RustChan process tree normally before replacing its
binary. Clean, stopped 1.7.1 source installations can keep their
`.software-updates` directory: the new version reads its admission evidence and
holds the existing leases, but does not execute old controllers or change their
journals, retained versions, or backups.

An unfinished software update, uncertain writer exit, corrupt journal, or
incomplete executable retention blocks startup before database writes. Preserve
that evidence and use the matching older installation to complete recovery before
upgrading. A reboot can establish exit of uncertain older writers; it does not
complete an unfinished transaction. Settings-only recovery restores the known-good
configuration, not SQLite or media. Software rollback needs its matching complete
snapshot. Do not delete old state to bypass a recovery error.

For an old **external** updater service, complete its recovery and stop/disable
that updater before switching launchers. Remove `RUSTCHAN_MANAGED=1` and private
internal-writer invocation arguments; launch the chosen binary directly with the
same data directory. The older updater service examples are no longer required. Ordinary service setup remains in [SETUP](../SETUP.md#linux-service-setup).
Old `RUSTCHAN_RESTART_ON_EXIT` and container markers have no effect; settings
changes now use normal operator stop/start. Keep historical recovery files and
backups, even after the upgrade.

Release builds prepare a draft with the platform ZIPs and checksums. Publish it
only after all CI checks for the release commit, including binary upgrade checks,
have passed.
