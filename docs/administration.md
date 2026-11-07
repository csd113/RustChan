# Manage your site

Open `/admin` on your RustChan site and sign in with your administrator account.
This is where you manage boards, posts, reports, appearance, and backups.
Every administrator has full administrative access; there is no separate limited
moderator role.

[All guides](../README.md#documentation) · [First setup](../SETUP.md) · [Settings](settings-restarts.md)

## Everyday tasks

| Task | Where to start |
| --- | --- |
| Add a topic | Create a board in the admin panel; give it a short URL name and title. |
| Change who can view or post | Edit that board's access settings. A board password is separate from an admin password. |
| Choose uploads | Edit the board's media types and limits. Site-wide limits also apply. |
| Moderate content | Review posts and reports, then use the relevant moderation, ban, or appeal controls. |
| Change the site's look | Use site settings, the theme catalog, favicon, and banner controls. |
| Save your site | Open Backups; follow the [backup guide](backups.md). |
| Check for a new version | Open Software Updates; follow the [manual update guide](software-updates.md). |

New boards normally allow images and video. Audio, PDFs, arbitrary files,
CAPTCHA, and NSFW status are opt-in. Arbitrary files also need the site-wide file
upload switch. Board settings control whether people can edit or delete their
own posts; the global signed self-action window defaults to 60 seconds and can
be set from 1 to 3,600 seconds.

For an optional per-board privacy policy, enable **Strict metadata-safe uploads**
in the board's upload settings. New attachments must be successfully rebuilt as
WebP; unsupported formats and failed sanitization are rejected. The supported
set is JPEG, static PNG, BMP, GIF and WebP. Video, audio, documents, APNG,
TIFF and HEIC/HEIF are rejected while it is enabled, even if their upload
permissions are checked. Existing media is unchanged. It does not remove
identifying image content or guarantee anonymity. Read the
[precise scope and tradeoffs](media-capabilities.md#strict-metadata-safe-uploads-optional)
before enabling it.

## Banners and appearance

Upload a supported bitmap such as PNG, JPEG, GIF, or WebP. Banners must have
exactly the **468:60** aspect ratio and be at least **468×60** pixels; **936×120**
is a useful working size. Animated GIF banners are limited to 60 frames.
RustChan targets WebP output and can retain a validated GIF when conversion is
unsupported.

To make one, download [the offline banner maker](rustchan-banner-maker.html) and
open that HTML file in your browser. GitHub's file viewer displays its source.
The tool works locally.

Board banners appear on the board index and catalog according to their placement
settings, not on threads, archives, or search pages. Home announcements have a
separate centered banner. External banner links must be enabled and pass through
an on-site warning before visitors leave.

## Archives and retention

An archive keeps an old conversation readable while removing it from the active
board and catalog. Archived threads keep their URLs and media, appear in search,
and are read-only, including polls. There is no public unarchive or preservation
control.

Each board has an active-thread limit. Background work handles overflow, so a
board can briefly show more threads than its limit. Threads with the most recent
bumps are retained first; ties use descending thread ID. Locked threads count
toward the limit; active sticky threads do not. Replies stop bumping at the bump
limit, and sage replies still count toward that limit. There is no timed expiry.

Overflow is archived if **Archive overflow threads** is enabled for the board
or `archive_before_prune` is enabled globally. Both must be off for overflow to
be deleted outright. The board's **Max archived threads** limit accepts 1–10,000
threads, includes archived sticky threads, and keeps recent bumps first with
ascending thread-ID ties. **Lowering it can permanently delete archives.**

Board retention settings apply live; changing the global safety net requires a
restart. Failed media deletions remain queued for retry after restart. Check
background-job failures if cleanup is not progressing.

## Command-line administration

The browser and terminal setup prompts avoid putting passwords in command
arguments. For recovery or scripted work, first read the available commands:

```sh
./rustchan-cli admin --help
```

Use the installation's real absolute data path and run as its service account
with the same configuration environment. For example:

```sh
rustchan-cli --data-dir /var/lib/rustchan admin list-admins
rustchan-cli --data-dir /var/lib/rustchan admin list-boards
rustchan-cli --data-dir /var/lib/rustchan admin db-status
rustchan-cli --data-dir /var/lib/rustchan admin create-board tech 'Technology' 'Programming and hardware'
```

If the executable is not on `PATH`, use its full path. On Windows, use the `.exe`
name and a Windows absolute data path. In Docker:

```sh
docker exec rustchan rustchan-cli --data-dir /data admin --help
```

To create an administrator or reset a forgotten password, the commands are
`admin create-admin <username> <password>` and
`admin reset-password <username> <new-password>`. Replace the placeholders with
actual values. Password arguments can appear in shell history and local process
inspection. Creating the first administrator closes the browser setup wizard.

## Terminal console

The current source has six views: **1 Overview, 2 Tasks, 3 Content, 4 Logs,
5 System, 6 Configuration**. Press **?** or **H** for help. **Q** asks before
stopping the server; **Ctrl-C** starts the same graceful shutdown immediately.
Configuration is read-only here; edit settings in the web admin or settings file.

| Key | Action |
| --- | --- |
| 1–6, Tab / Shift-Tab | Change view |
| Arrows, Page Up/Down, Home/End | Navigate or scroll |
| R | Refresh observations |
| / | Filter a list |
| S | Change the view's sort, state, or log-level filter |
| Enter | Open details |
| P / F | Pause / follow logs |
| C / A | Start board / administrator creation |
| D / X | Start thread deletion with confirmation |

The console needs at least 44 columns by 14 rows. **The following mode options
are newer than the published v1.7.5 ZIPs.** Check `--help` for your executable:

| Option | Behavior |
| --- | --- |
| `--console auto` | Default: full screen with capable interactive input/output |
| `--console tui` | Request full screen; fall back to logs if initialization fails |
| `--console plain` | Logs plus operational summaries every 15 seconds |
| `--console off` | Ordinary logs and signal-based shutdown |
| `--ascii` | Replace non-ASCII console glyphs |

Redirected streams, `TERM=dumb`, and terminal initialization failures use plain
logging without raw input. `NO_COLOR=1` disables colors. Linux/Windows terminal
relaunch is opt-in with `CHAN_LAUNCH_TERMINAL=1`. On Windows, if the launcher
cannot start the new console, startup continues with the original streams.

### Reading the numbers

Request and active-IP counts are observations from this process, not user or
session counts. Thread counts include archives. Overview separates logical
SQLite size from observed main/WAL/SHM file sizes. System shows separate query,
file, and upload sample ages and errors; retained values may be stale.

Queries normally refresh every 3 seconds, at most once per second on manual
refresh. File sizes refresh every 3 seconds and uploads every 30 seconds. Upload
scans stop at 100,000 entries or two seconds, although a slow filesystem call can
take longer. Task totals cover all jobs; bounded terminal-state rows examine the
newest 4,096 IDs. Logs keep a 256 KiB/4,000-line tail and indicate omitted history.
Task age is not an ETA, and there is no general job-cancellation control.

An onion address, an Arti publication state, and observed local forwarding are
separate facts. None proves that an outside visitor can reach the service.
Automatic-backup outcomes describe attempts seen during this process; `UNKNOWN`
does not mean that no older backup exists.
