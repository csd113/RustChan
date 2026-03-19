// detect.rs — Startup detection for optional external tools.
//
// All terminal output goes through crate::logging helpers so it is
// serialised with the tracing terminal layer (CONSOLE_MUTEX).  This
// prevents interleaving with concurrent log events during startup.
//
// Structured events (info!/warn!/error!) capture the detection result in
// the JSON log file with structured fields.  Human-readable install
// instructions are written via console_print_raw so they appear only
// on TTY-mode terminals and never pollute piped / systemd output.

use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};

/// Result of probing for a tool at startup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolStatus {
    Available,
    Missing,
}

// ─── Global Tor child handle ──────────────────────────────────────────────────

static TOR_CHILD: OnceLock<Arc<Mutex<std::process::Child>>> = OnceLock::new();

pub fn kill_tor() {
    if let Some(child) = TOR_CHILD.get() {
        if let Ok(mut c) = child.lock() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

// ─── ffmpeg ───────────────────────────────────────────────────────────────────

pub fn detect_ffmpeg(require_ffmpeg: bool) -> ToolStatus {
    let ok = Command::new("ffmpeg")
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    if ok {
        tracing::info!(target: "detect", available = true, "ffmpeg detected — video thumbnails and transcoding enabled");
        ToolStatus::Available
    } else if require_ffmpeg {
        tracing::error!(
            target: "detect",
            available = false,
            "ffmpeg required but not installed — set require_ffmpeg = false in settings.toml to disable this check"
        );
        crate::logging::console_print_raw(
            "  Install ffmpeg from: https://ffmpeg.org/download.html\n\n",
        );
        std::process::exit(1);
    } else {
        tracing::warn!(
            target: "detect",
            available = false,
            "ffmpeg not detected — video thumbnails and transcoding disabled"
        );
        if crate::logging::is_tty() {
            crate::logging::console_print_raw(
                "  Install from: https://ffmpeg.org/download.html\n\n",
            );
        }
        ToolStatus::Missing
    }
}

/// Probe whether the detected ffmpeg has `libwebp` compiled in.
pub fn detect_webp_encoder(ffmpeg_ok: bool) -> bool {
    if !ffmpeg_ok {
        return false;
    }

    let has_webp = crate::media::ffmpeg::check_webp_encoder();

    if has_webp {
        tracing::info!(
            target: "detect",
            webp = true,
            "ffmpeg libwebp encoder available — image to WebP conversion enabled"
        );
    } else {
        tracing::warn!(
            target: "detect",
            webp = false,
            "ffmpeg libwebp encoder missing — JPEG/PNG/BMP/TIFF stored in original format"
        );
        if crate::logging::is_tty() {
            crate::logging::console_print_raw(&webp_install_hint());
        }
    }

    has_webp
}

fn webp_install_hint() -> String {
    let mut s = String::new();

    #[cfg(target_os = "macos")]
    {
        s.push_str(
            "  ── macOS: reinstall ffmpeg with libwebp ─────────────────────────────\n\
             \n\
             \x1b[2m  brew uninstall ffmpeg\n\
             \x1b[2m  brew tap homebrew-ffmpeg/ffmpeg\n\
             \x1b[2m  brew install homebrew-ffmpeg/ffmpeg/ffmpeg --with-webp\x1b[0m\n\n",
        );
    }
    #[cfg(target_os = "linux")]
    {
        s.push_str(
            "  ── Linux: install ffmpeg with libwebp ───────────────────────────────\n\
             \n\
             \x1b[2m  sudo apt update && sudo apt install ffmpeg libwebp-dev\x1b[0m\n\n",
        );
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        s.push_str(
            "  Reinstall ffmpeg with libwebp support. See: https://ffmpeg.org/download.html\n\n",
        );
    }

    if !crate::logging::is_tty() {
        // Strip any ANSI codes we added for TTY mode
        s.retain(|c| c != '\x1b');
    }
    s
}

/// Probe whether the detected ffmpeg has `libvpx-vp9` + `libopus` compiled in.
pub fn detect_webm_encoder(ffmpeg_ok: bool) -> bool {
    if !ffmpeg_ok {
        return false;
    }

    let has_vp9 = crate::media::ffmpeg::check_vp9_encoder();
    let has_opus = crate::media::ffmpeg::check_opus_encoder();
    let has_webm = has_vp9 && has_opus;

    if has_webm {
        tracing::info!(
            target: "detect",
            vp9 = true, opus = true,
            "ffmpeg VP9 + Opus encoders available — MP4 to WebM transcoding enabled"
        );
    } else {
        tracing::warn!(
            target: "detect",
            vp9   = has_vp9,
            opus  = has_opus,
            "ffmpeg VP9/Opus encoders missing — MP4 uploads stored as MP4"
        );
        if crate::logging::is_tty() {
            crate::logging::console_print_raw(&webm_install_hint(has_vp9, has_opus));
        }
    }

    has_webm
}

fn webm_install_hint(has_vp9: bool, has_opus: bool) -> String {
    let mut s = String::new();
    if !has_vp9 {
        s.push_str("  Missing: libvpx-vp9 (VP9 video encoder)\n");
    }
    if !has_opus {
        s.push_str("  Missing: libopus   (Opus audio encoder)\n");
    }
    s.push('\n');

    #[cfg(target_os = "macos")]
    {
        s.push_str(
            "  ── macOS: reinstall ffmpeg with VP9 + Opus ──────────────────────────\n\
             \n\
             \x1b[2m  brew install ffmpeg\x1b[0m\n\n",
        );
    }
    #[cfg(target_os = "linux")]
    {
        s.push_str(
            "  ── Linux: install ffmpeg with VP9 + Opus ────────────────────────────\n\
             \n\
             \x1b[2m  sudo apt update && sudo apt install ffmpeg libvpx-dev libopus-dev\x1b[0m\n\n",
        );
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        s.push_str(
            "  Reinstall ffmpeg with libvpx-vp9 and libopus support.\n\
             See: https://ffmpeg.org/download.html\n\n",
        );
    }

    if !crate::logging::is_tty() {
        s.retain(|c| c != '\x1b');
    }
    s
}

// ─── Tor ─────────────────────────────────────────────────────────────────────

#[allow(clippy::too_many_lines)]
#[allow(clippy::expect_used)]
#[allow(clippy::arithmetic_side_effects)]
pub fn detect_tor(enable_tor_support: bool, bind_port: u16, data_dir: &Path) -> ToolStatus {
    if !enable_tor_support {
        return ToolStatus::Missing;
    }

    let candidates = [
        "/opt/homebrew/bin/tor",
        "/usr/local/bin/tor",
        "/usr/bin/tor",
        "tor",
    ];

    let tor_bin: Option<&str> = candidates.iter().copied().find(|bin| {
        Command::new(bin)
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok()
    });

    let Some(tor_bin) = tor_bin else {
        tracing::warn!(target: "detect", available = false, "Tor not found — onion service disabled");
        if crate::logging::is_tty() {
            crate::logging::console_print_raw(&tor_install_hint(bind_port));
        }
        return ToolStatus::Missing;
    };

    tracing::info!(target: "detect", binary = tor_bin, "Tor binary found");

    let hs_dir = data_dir.join("tor_hidden_service");
    let data_subdir = data_dir.join("tor_data");

    for dir in [&hs_dir, &data_subdir] {
        if let Err(e) = std::fs::create_dir_all(dir) {
            tracing::warn!(
                target: "detect",
                path = %dir.display(),
                error = %e,
                "Tor: cannot create directory"
            );
            if crate::logging::is_tty() {
                crate::logging::console_print_raw(&tor_torrc_hint(&hs_dir, bind_port));
            }
            return ToolStatus::Missing;
        }
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for dir in [&hs_dir, &data_subdir] {
            if let Err(e) = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)) {
                tracing::warn!(
                    target: "detect",
                    path = %dir.display(),
                    error = %e,
                    "Tor: cannot set 0700 permissions (Tor may reject directory)"
                );
            }
        }
    }

    let canon = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    let hs_abs = canon(&hs_dir);
    let data_abs = canon(&data_subdir);
    let torrc_path = canon(data_dir).join("torrc");

    let torrc = format!(
        "# RustChan — auto-generated torrc  (do not edit while Tor is running)\n\
         \n\
         SocksPort 0\n\
         DataDirectory \"{data}\"\n\
         \n\
         HiddenServiceDir \"{hs}\"\n\
         HiddenServicePort 80 127.0.0.1:{port}\n",
        data = data_abs.display(),
        hs = hs_abs.display(),
        port = bind_port,
    );

    if let Err(e) = std::fs::write(&torrc_path, &torrc) {
        tracing::warn!(
            target: "detect",
            path  = %torrc_path.display(),
            error = %e,
            "Tor: cannot write torrc"
        );
        if crate::logging::is_tty() {
            crate::logging::console_print_raw(&tor_torrc_hint(&hs_dir, bind_port));
        }
        return ToolStatus::Missing;
    }

    tracing::info!(
        target: "detect",
        torrc      = %torrc_path.display(),
        hidden_svc = %hs_abs.display(),
        data_dir   = %data_abs.display(),
        "Tor configured"
    );

    let child = Command::new(tor_bin)
        .arg("-f")
        .arg(&torrc_path)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn();

    let mut child = match child {
        Err(e) => {
            tracing::warn!(
                target: "detect",
                binary = tor_bin,
                error  = %e,
                "Tor: failed to start process"
            );
            if crate::logging::is_tty() {
                crate::logging::console_print_raw(&tor_torrc_hint(&hs_dir, bind_port));
            }
            return ToolStatus::Missing;
        }
        Ok(c) => c,
    };

    let stderr_lines: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    if let Some(pipe) = child.stderr.take() {
        let buf = Arc::clone(&stderr_lines);
        std::thread::spawn(move || {
            use std::io::{BufRead, BufReader};
            for line in BufReader::new(pipe).lines().map_while(Result::ok).take(500) {
                if let Ok(mut g) = buf.lock() {
                    g.push(line);
                }
            }
        });
    }

    let child = Arc::new(Mutex::new(child));
    let _ = TOR_CHILD.set(Arc::clone(&child));

    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        kill_tor();
        prev_hook(info);
    }));

    let pid = child.lock().expect("child process mutex poisoned").id();
    tracing::info!(target: "detect", pid = pid, "Tor process started — waiting for .onion address");

    let hostname_path = hs_abs.join("hostname");
    let torrc_display = torrc_path.display().to_string();
    let tor_bin_owned = tor_bin.to_string();

    let child_bg = Arc::clone(&child);
    let stderr_bg = Arc::clone(&stderr_lines);

    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(4));

        let try_wait_result = child_bg
            .lock()
            .expect("child process mutex poisoned")
            .try_wait();

        match try_wait_result {
            Ok(Some(status)) => {
                let lines = stderr_bg.lock().expect("stderr buffer mutex poisoned");
                tracing::error!(
                    target: "detect",
                    exit_status = %status,
                    "Tor process exited early"
                );
                if crate::logging::is_tty() && !lines.is_empty() {
                    let mut block =
                        String::from("\n  ── Tor stderr ──────────────────────────────────\n");
                    for line in lines.iter().take(20) {
                        block.push_str("  ");
                        block.push_str(line);
                        block.push('\n');
                    }
                    block.push_str("  ────────────────────────────────────────────────\n\n");
                    drop(lines);
                    crate::logging::console_print_raw(&block);
                    crate::logging::console_print_raw(&tor_diagnosis_hint(
                        &torrc_display,
                        &tor_bin_owned,
                        bind_port,
                    ));
                }
                return;
            }
            Ok(None) => {}
            Err(e) => {
                tracing::warn!(target: "detect", error = %e, "Tor: could not query process status");
            }
        }

        poll_for_hostname(
            &hostname_path,
            &child_bg,
            &stderr_bg,
            &torrc_display,
            &tor_bin_owned,
            bind_port,
        );
    });

    ToolStatus::Available
}

// ─── Hostname polling ─────────────────────────────────────────────────────────

#[allow(clippy::expect_used)]
#[allow(clippy::arithmetic_side_effects)]
fn poll_for_hostname(
    hostname_path: &Path,
    child: &Arc<Mutex<std::process::Child>>,
    stderr_lines: &Arc<Mutex<Vec<String>>>,
    torrc_display: &str,
    tor_bin: &str,
    bind_port: u16,
) {
    const TIMEOUT_SECS: u64 = 120;
    const POLL_MS: u64 = 500;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(TIMEOUT_SECS);

    loop {
        if let Ok(mut c) = child.try_lock() {
            if let Ok(Some(status)) = c.try_wait() {
                let lines = stderr_lines.lock().expect("stderr buffer mutex poisoned");
                tracing::error!(
                    target: "detect",
                    exit_status = %status,
                    "Tor process crashed during startup"
                );
                if crate::logging::is_tty() && !lines.is_empty() {
                    let mut block =
                        String::from("\n  ── Tor stderr ──────────────────────────────────\n");
                    for line in lines.iter().take(20) {
                        block.push_str("  ");
                        block.push_str(line);
                        block.push('\n');
                    }
                    block.push_str("  ────────────────────────────────────────────────\n\n");
                    drop(lines);
                    crate::logging::console_print_raw(&block);
                    crate::logging::console_print_raw(&tor_diagnosis_hint(
                        torrc_display,
                        tor_bin,
                        bind_port,
                    ));
                }
                return;
            }
        }

        if hostname_path.exists() {
            match std::fs::read_to_string(hostname_path) {
                Ok(raw) => {
                    let onion = raw.trim();
                    if !onion.is_empty() {
                        tracing::info!(
                            target: "detect",
                            onion_address = onion,
                            "Tor hidden service active"
                        );
                        if crate::logging::is_tty() {
                            crate::logging::console_print_raw(&tor_onion_banner(
                                onion,
                                hostname_path,
                            ));
                        }
                        return;
                    }
                }
                Err(e) => {
                    tracing::warn!(target: "detect", error = %e, "Tor: hostname file unreadable");
                }
            }
        }

        if std::time::Instant::now() >= deadline {
            tracing::warn!(
                target: "detect",
                timeout_secs = TIMEOUT_SECS,
                hostname_path = %hostname_path.display(),
                "Tor timed out waiting for hostname file"
            );
            if crate::logging::is_tty() {
                crate::logging::console_print_raw(&tor_diagnosis_hint(
                    torrc_display,
                    tor_bin,
                    bind_port,
                ));
            }
            return;
        }

        std::thread::sleep(std::time::Duration::from_millis(POLL_MS));
    }
}

// ─── Tor print helpers (console_print_raw blocks) ────────────────────────────

fn tor_onion_banner(onion: &str, hostname_path: &Path) -> String {
    let addr_line = format!("http://{onion}");
    let key_dir = hostname_path.parent().unwrap_or(hostname_path);
    format!(
        "\n\
        ╔════════════════════════════════════════════════════════════════════════╗\n\
        ║        TOR ONION SERVICE ACTIVE  ✓                                     ║\n\
        ╠════════════════════════════════════════════════════════════════════════╣\n\
        ║  {addr:<70}║\n\
        ║                                                                        ║\n\
        ║  Share this with Tor Browser users.                                    ║\n\
        ║  Your private key is stored at:                                        ║\n\
        ║    {key:<68}║\n\
        ║  Back it up — losing it means a new .onion address.                    ║\n\
        ╚════════════════════════════════════════════════════════════════════════╝\n\n",
        addr = addr_line,
        key = key_dir.display(),
    )
}

fn tor_install_hint(bind_port: u16) -> String {
    format!(
        "\n\
        \x1b[2m  ── Install Tor ────────────────────────────────────────────────────────\n\
          macOS:   brew install tor\n\
          Linux:   sudo apt-get install tor\n\
          Other:   https://www.torproject.org/download/tor/\n\
        \n\
          After installing, restart RustChan — it configures Tor automatically.\n\
          Or add to your own torrc:\n\
            HiddenServicePort 80 127.0.0.1:{bind_port}\x1b[0m\n\n"
    )
}

fn tor_torrc_hint(hs_dir: &Path, bind_port: u16) -> String {
    format!(
        "\n  Add to your torrc and restart Tor:\n\
         \x1b[2m    SocksPort 0\n\
           DataDirectory /var/lib/tor/rustchan-data/\n\
           HiddenServiceDir {hs}\n\
           HiddenServicePort 80 127.0.0.1:{bind_port}\x1b[0m\n\n",
        hs = hs_dir.display(),
    )
}

fn tor_diagnosis_hint(torrc_path: &str, tor_bin: &str, bind_port: u16) -> String {
    format!(
        "\n  \x1b[2m── Tor troubleshooting ──────────────────────────────────────────────────\n\
         \n\
           1. Run manually to see live output:\n\
                {tor_bin} -f {torrc_path}\n\
         \n\
           2. Common causes:\n\
              a) DataDirectory permissions  — chmod 700 <data_dir>/tor_data/\n\
              b) HiddenServiceDir perms     — chmod 700 <data_dir>/tor_hidden_service/\n\
              c) Firewall: Tor needs outbound TCP on ports 9001 and 443.\n\
              d) macOS brew conflict: brew services stop tor, then restart RustChan.\n\
              e) Linux SELinux/AppArmor: sudo journalctl -u tor --since '5 min ago'\n\
         \n\
           3. To manage Tor yourself:\n\
              Set enable_tor_support = false in settings.toml\n\
              and add to your torrc:  HiddenServicePort 80 127.0.0.1:{bind_port}\x1b[0m\n\n"
    )
}
