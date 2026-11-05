# Set up RustChan

RustChan runs a discussion website from one app. Start with a local site on your
own computer; you can set up public access later.

[All guides](README.md#documentation) · [Administration](docs/administration.md) · [Deployment](docs/deployment.md)

## Download the app

1. Open the [official latest release](https://github.com/csd113/RustChan/releases/latest).
2. Under **Assets**, choose the ZIP for your operating system and processor.
   The [README download table](README.md#1-download-the-app) lists the filenames.
3. Extract the ZIP into a writable folder, such as a RustChan folder in your home
   directory. Open the folder containing `rustchan-cli` or `rustchan-cli.exe`.

On a Mac, **About This Mac** tells you whether it has an Apple chip or an Intel
processor. On Windows, check **Settings → System → About → System type**. On
Linux, `uname -m` reports `x86_64` for Intel/AMD or `aarch64` for ARM64.

## Run the app

Open a terminal in the folder containing the executable. To change folders, type
`cd`, a space, and the folder's path in quotes.

**Windows — PowerShell:**

```powershell
$env:CHAN_HOST = "127.0.0.1"
$env:CHAN_TOR_SUPPORT = "false"
.\rustchan-cli.exe
```

**macOS or Linux:**

```sh
chmod +x rustchan-cli
CHAN_HOST=127.0.0.1 CHAN_TOR_SUPPORT=false ./rustchan-cli
```

These commands select local access and disable Tor for this run. Repeat them
when restarting your trial site. Without these overrides, a native installation
normally listens on all network interfaces and enables Tor. To save a local-only
configuration, see [settings](docs/settings-restarts.md#where-settings-live).

## Finish setup

In an interactive terminal, RustChan asks you to create an administrator. Enter a
username and a unique password, confirm it, and choose whether to create your
first board. A board's short name becomes part of its address; use 1–8 letters
or digits, such as `chat`.

Then open <http://localhost:8080>. Sign in at <http://localhost:8080/admin> with
your new account. If you skipped the first board, create one in the admin panel.
You can also change the site name there.

If no terminal questions appear and no administrator exists, use the browser
wizard at <http://localhost:8080/setup>:

1. Choose **Local/testing**, then **load defaults** for a local trial.
2. Enter the site name, administrator username and password, and first board.
3. Select **review setup**, check the details, then **finish setup**.
4. Open the home page and sign in to the admin panel.

The browser wizard is available only until the first administrator is created.
Its presets choose site and board options; they do not configure your firewall
or make a public deployment private. If terminal setup is waiting for input,
finish those prompts before opening the browser.

The current source wizard supports a separate board password for private boards.
Share that password with visitors and keep the administrator password private.
This private-board setup fix is newer than the published v1.7.5 ZIPs; for those
ZIPs, use local setup first and configure board access in the admin panel.
Current setup accepts image limits of 1–100 MiB and video, audio, and PDF limits
of 1–512 MiB; it validates them before review.

## Stop and start again

Press **Ctrl-C** in the terminal and wait for RustChan to exit. Start it again
with the same command, executable location, and data folder. Setup does not run
again once an administrator exists. Keep the terminal open while the site is running.

`localhost` means the computer you are using. To visit from another device or
run a site continuously, continue with [deployment](docs/deployment.md) or
[Docker](docs/containers.md).

## Where your site is saved

By default, RustChan creates `rustchan-data` next to the executable. The current
working directory does not change that location.

```text
rustchan-data/
├── settings.toml           # Server settings and private cookie secret
├── chan.db                 # Posts, boards, accounts, and other database records
├── boards/                 # Uploads and thumbnails
├── backups/
│   ├── full/               # Saved full-site backups
│   └── boards/             # Saved board backups
├── logs/
└── runtime/
    ├── tls/               # Certificates and keys
    ├── tor/
    │   ├── state/         # Persistent onion identity
    │   └── cache/
    ├── favicon/
    └── tmp/
```

Keep this whole folder. `chan.db-wal` and `chan.db-shm` may appear beside the
database; do not remove them. Older installations may also have a
`.software-updates` recovery folder that must be preserved.

You can choose another location with an **absolute** path:

```sh
./rustchan-cli --data-dir /absolute/path/to/rustchan-data serve
```

On Windows, use `--data-dir "C:\RustChan\data"` with `rustchan-cli.exe`.
Use that same path for future launches and admin commands. Changing a path does
not move existing data. For backups and custom storage, see [backups](docs/backups.md).

## Requirements

Downloaded copies need a matching operating system and processor, a writable
data folder, and a web browser. Rust, Git, and a separate database server are not
needed. There is no separate Tor daemon, image library, `ffprobe`, or PDF renderer
to install.

The official release workflow builds Linux x86_64/ARM64, macOS Intel/Apple
silicon, and Windows x86_64. There is no native Windows ARM64 or 32-bit download.
The v1.7.5 Linux executables require glibc 2.39 or newer; they do not run directly
on musl-based systems such as Alpine. Use [Docker](docs/containers.md) or a
[local source build](CONTRIBUTING.md#build-and-run) where appropriate.

Release CI uses Ubuntu 24.04, macOS 15, and `windows-latest`. That build matrix
is not a promise of compatibility with every older OS. Check the notes on the
release you download if an executable cannot start.

## Check the download

Download `SHA256SUMS.txt` from the **same release** as your ZIP. Compute the ZIP's
SHA-256 hash and compare it with that file's entry. Replace the filename below
with your actual download.

**Windows — PowerShell:**

```powershell
Get-FileHash .\rustchan-cli-v1.7.5-windows-x86_64.zip -Algorithm SHA256
```

**macOS:**

```sh
shasum -a 256 rustchan-cli-v1.7.5-macos-apple-silicon.zip
```

**Linux:**

```sh
sha256sum rustchan-cli-v1.7.5-linux-x86_64.zip
```

The hash must match exactly, ignoring letter case. This checks the file against
the official manifest; it is not a code-signing certificate. If the OS blocks the
app, verify its source and hash before using the OS's per-app approval controls.
Do not disable system-wide protection.

## Install FFmpeg

FFmpeg adds video thumbnails, video conversion, and audio previews for codecs
the built-in decoders do not cover. You can try RustChan without it. It becomes
required at startup only if `require_ffmpeg = true`.

| Platform | Example installation |
| --- | --- |
| Debian / Ubuntu / Raspberry Pi OS | `sudo apt install ffmpeg` |
| Fedora | `sudo dnf install ffmpeg` (available codecs depend on enabled repositories) |
| macOS with Homebrew | `brew install ffmpeg` |
| Windows with winget | `winget install --id Gyan.FFmpeg -e` |

Open a new terminal after installation and run `ffmpeg -version`. Restart
RustChan so it detects the tool. If it cannot find FFmpeg, set `ffmpeg_path` in
`settings.toml` to the executable's path. A service must be able to find it under
its own account and `PATH`.

Video conversion needs `libvpx-vp9`, `libopus`, and the `webm` muxer. AV1 input
also needs an AV1 decoder; RustChan's converted output remains VP9/Opus.
Use `ffmpeg -encoders`, `ffmpeg -muxers`, and `ffmpeg -decoders` to inspect a build.
Rust handles WebP encoding; `libvpx-dev` and `libopus-dev` are not build requirements.
See [media capabilities](docs/media-capabilities.md) for details and fallback limits.

## Troubleshooting

| What happened? | What to do |
| --- | --- |
| The command is not found | Open the extracted folder containing the executable. Use `./rustchan-cli` or `.\rustchan-cli.exe`, including the prefix. |
| Linux/macOS says permission denied | Run `chmod +x rustchan-cli`; check that the folder allows execution. |
| Wrong architecture or missing GLIBC error | Check the ZIP choice and [requirements](#requirements). |
| Browser cannot connect | Keep the app running, finish terminal prompts, and read startup errors. Use the same computer and port as the app. |
| Port 8080 is already in use | Start with `--port 9090` and open `http://localhost:9090`. |
| Setup is unavailable | An administrator may already exist. Try `/admin`; see [account recovery](docs/administration.md#command-line-administration). |
| My site looks empty after moving the app | Check the data folder. Launch with `--data-dir` pointing to your original data. |
| Video has no preview | Check FFmpeg detection and codec warnings; valid originals may still be available. |
| Upload fails | Check free disk space, folder permissions, board limits, and logs. If a saved post is named in the error, inspect it before resubmitting. |
| A saved setting has no effect | Check the restart notice and environment overrides in [settings](docs/settings-restarts.md). |
| Database startup fails | Preserve the data and read the error. Use the [database guide](docs/sqlite-engineering.md); do not delete files to force startup. |

Logs are in `logs/rustchan.YYYY-MM-DD.log` inside your data folder. Remove private
information before sharing excerpts. [Support](SUPPORT.md) explains what to include
in a report; [Security](SECURITY.md) covers sensitive vulnerabilities.
