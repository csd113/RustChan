# Update RustChan

Open **Software Updates** in the admin panel and select **Check for updates**.
RustChan checks official stable releases and links to the GitHub release page.
You download and install the update yourself. The app does not download a binary,
install it, or restart itself.

[All guides](../README.md#documentation) · [Backups](backups.md) · [Container updates](containers.md#update-the-container)

## Before updating

Read the release notes, choose the correct platform ZIP, and
[verify its checksum](../SETUP.md#check-the-download) against that release's
`SHA256SUMS.txt`. Keep the old executable and a complete stopped backup.
A container installation is updated by replacing its image while retaining data;
use the [container steps](containers.md#update-the-container).

## Manual upgrade of a source install

These steps apply to both downloaded native binaries and source builds.

1. Find the executable and its data directory. Record the account, environment,
   working directory, and launch arguments. By default data lives beside the
   executable; moving the binary can make the app appear to be a new site.
2. Download the matching official binary, or build separately with
   `cargo build --locked --release --bin rustchan-cli`. Keep the same architecture
   and any required Cargo features. Keep the old executable.
3. Stop RustChan through its normal terminal or service manager and confirm it
   exited. Make a [complete stopped backup](backups.md#make-a-complete-filesystem-copy),
   including external persistent paths. An admin database-only snapshot is not enough.
4. Replace the executable in place, preserving its owner and executable mode.
   Start with the same account, environment, arguments, and data path. Templates
   and static website files are embedded; you do not install them separately.
5. Check `/readyz`, sign in, and inspect existing boards, posts, and media. Run
   `admin db-status` with the same data directory. Reload open browser forms and
   review logs before reopening public access.

Startup applies supported database migrations and verifies the structure before
recording the running version. It rejects unknown, partial, or corrupt layouts.

### If the new version fails

Stop it and preserve the failed state for diagnosis. Restore the matching
**complete stopped backup and old executable together**, then verify the site.
Never run an old executable against a migrated live database or manually lower
`schema_version`. See [database recovery](sqlite-engineering.md#backup-and-recovery).

## How update checks work

Only a signed-in administrator can check. The server contacts GitHub when
**Check for updates** is pressed, not on a schedule. Checks within a minute reuse
the previous result. Only stable semantic versions are compared; drafts and
prereleases are excluded. The form works without JavaScript.

The request has a 15-second deadline and a bounded response size. RustChan does
not follow redirects or download URLs supplied by the API. A timeout, outage, or
rate limit shows an error and the official releases link without preventing
startup or stopping the site. Results stay in memory until restart.
`/readyz` reports the running package version, not update availability. The
version comparison cannot distinguish two development builds with the same
package version; use the commit when identifying a source build.

## Retiring an older updater

This section is only for installations that used RustChan's older automatic
updater. New installations do not need an updater service.

Stop the complete older process tree normally before replacing the binary. A
cleanly stopped 1.7.1 native installation can keep `.software-updates`. Current
RustChan reads its recovery evidence and holds existing leases, but does not run
old controllers or change their journals, retained versions, or backups.

An unfinished update, uncertain writer exit, corrupt journal, or incomplete
retained executable blocks startup before database writes. Keep the evidence
and use the matching older installation to finish recovery before upgrading.
A reboot can establish that older writers have exited; it does not finish an
incomplete update. Never delete state to bypass a recovery error.

For an old **external** updater service, complete recovery and stop/disable that
updater before switching launchers. Remove `RUSTCHAN_MANAGED=1` and private
internal-writer arguments, then launch the selected binary directly with the
same data directory. Use the ordinary [service setup](deployment.md#linux-service-setup).
Old `RUSTCHAN_RESTART_ON_EXIT` and container markers have no effect.

Settings-only recovery restores known-good configuration, not SQLite or media.
Software rollback needs the matching complete snapshot. Keep historical recovery
files and backups after upgrading. See [settings recovery](settings-restarts.md#recover-from-a-bad-setting).

## Release maintainers

The release workflow prepares a draft with five platform ZIPs and checksums.
Publish only after all required checks for that release commit, including the
release workflow's binary-upgrade checks, pass. Normal push CI does not run the
binary-upgrade workflow. See [contributing](../CONTRIBUTING.md#validation).
