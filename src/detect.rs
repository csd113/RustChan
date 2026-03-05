// detect.rs — Startup detection for optional external tools.
//
// Responsibilities:
//   • detect_ffmpeg() — probe ffmpeg via `ffmpeg -version`
//   • detect_tor()    — probe tor, set up an isolated hidden-service instance,
//                       launch it as a background process, and poll for the
//                       hostname file.  Works alongside a system Tor that is
//                       already running (brew, apt, systemd, etc.).
//
// ── Why a second Tor process is safe ──────────────────────────────────────────
// The torrc we write contains:
//
//   SocksPort  0                  ← disables SOCKS; no port conflict with
//                                    the system Tor (which owns 9050)
//   DataDirectory  <data_dir>/tor_data/
//                                 ← separate lock-file + keys from system Tor
//
// This means RustChan's Tor instance owns only the hidden service and nothing
// else.  It starts cleanly even when brew / apt / systemd Tor is running.
//
// ── Platform support ──────────────────────────────────────────────────────────
//   macOS  (brew): /opt/homebrew/bin/tor  (Apple Silicon)
//                  /usr/local/bin/tor     (Intel)
//   Linux  (apt):  /usr/bin/tor  or  bare "tor" on PATH
//   Other:         bare "tor" on PATH
//
// ── Security ──────────────────────────────────────────────────────────────────
//   • Command arguments are always separate &str / Path values; no shell string.
//   • We never eval or execute output from spawned processes.
//   • HiddenServiceDir is mode 0700 so only the running user can read the
//     private key and hostname files.
//   • If the spawned Tor process exits within the first few seconds, we capture
//     and display its stderr so the operator can diagnose the problem.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Result of probing for a tool at startup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolStatus {
    Available,
    Missing,
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
        println!("[INFO] ffmpeg detected. Video thumbnails and transcoding enabled.");
        ToolStatus::Available
    } else if require_ffmpeg {
        eprintln!("[ERROR] ffmpeg required but not installed.");
        eprintln!("        Install ffmpeg or set require_ffmpeg = false in settings.toml.");
        std::process::exit(1);
    } else {
        println!("[WARN] ffmpeg not detected. Video thumbnails disabled.");
        println!("       Install from: https://ffmpeg.org/download.html");
        ToolStatus::Missing
    }
}

// ─── Tor ─────────────────────────────────────────────────────────────────────

/// Set up and launch a Tor hidden-service instance.
///
/// Creates inside `data_dir`:
///   tor_data/           — Tor's DataDirectory (lock file, keys, etc.)
///   tor_hidden_service/ — HiddenServiceDir  (private key + hostname)
///   torrc               — auto-generated config
///
/// Launches `tor -f <torrc>` as a background process then polls for the
/// hostname file in a background OS thread.  Returns immediately — the
/// HTTP server is never blocked.
pub fn detect_tor(enable_tor_support: bool, bind_port: u16, data_dir: &Path) {
    if !enable_tor_support {
        return;
    }

    // ── 1. Find the tor binary ────────────────────────────────────────────────
    let candidates = [
        "/opt/homebrew/bin/tor", // macOS Apple-Silicon brew
        "/usr/local/bin/tor",    // macOS Intel brew
        "/usr/bin/tor",          // Linux apt / rpm
        "tor",                   // anything else on PATH
    ];

    let tor_bin: Option<&str> = candidates.iter().copied().find(|bin| {
        // `--version` may return exit code 1 on some builds; treat any
        // successful spawn (binary found) as "available".
        Command::new(bin)
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok()
    });

    let Some(tor_bin) = tor_bin else {
        print_install_instructions(bind_port);
        return;
    };

    println!("[INFO] Tor binary: {}", tor_bin);

    // ── 2. Create directories ─────────────────────────────────────────────────
    let hs_dir = data_dir.join("tor_hidden_service");
    let data_subdir = data_dir.join("tor_data");

    for dir in [&hs_dir, &data_subdir] {
        if let Err(e) = std::fs::create_dir_all(dir) {
            println!(
                "[WARN] Tor: cannot create directory '{}': {}",
                dir.display(),
                e
            );
            print_torrc_hint(&hs_dir, bind_port);
            return;
        }
    }

    // Tor refuses to use a HiddenServiceDir that is world- or group-readable.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for dir in [&hs_dir, &data_subdir] {
            if let Err(e) = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)) {
                println!(
                    "[WARN] Tor: cannot set 0700 on '{}': {} (Tor may reject it)",
                    dir.display(),
                    e
                );
            }
        }
    }

    // ── 3. Write torrc ────────────────────────────────────────────────────────
    let torrc_path = data_dir.join("torrc");

    // Canonical absolute paths avoid problems when Tor's working directory
    // differs from ours.
    let canon = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    let hs_abs = canon(&hs_dir);
    let data_abs = canon(&data_subdir);

    let torrc = format!(
        "# RustChan — auto-generated torrc  (do not edit while Tor is running)\n\
         \n\
         ## Isolate this instance from any system-level Tor (brew / apt / systemd).\n\
         ## SocksPort 0  → do not bind a SOCKS port; avoids conflict with port 9050.\n\
         ## DataDirectory → separate lock file + state from the system Tor.\n\
         SocksPort 0\n\
         DataDirectory {data}\n\
         \n\
         ## Hidden service — forwards .onion:80 to the local RustChan port.\n\
         HiddenServiceDir {hs}\n\
         HiddenServicePort 80 127.0.0.1:{port}\n",
        data = data_abs.display(),
        hs = hs_abs.display(),
        port = bind_port,
    );

    if let Err(e) = std::fs::write(&torrc_path, &torrc) {
        println!(
            "[WARN] Tor: cannot write torrc to '{}': {}",
            torrc_path.display(),
            e
        );
        print_torrc_hint(&hs_dir, bind_port);
        return;
    }

    println!("[INFO] Tor: torrc          → {}", torrc_path.display());
    println!("[INFO] Tor: hidden-svc dir → {}", hs_abs.display());
    println!("[INFO] Tor: data dir       → {}", data_abs.display());

    // ── 4. Spawn Tor ──────────────────────────────────────────────────────────
    // Pipe stderr so we can capture it if the process dies quickly.
    let child = Command::new(tor_bin)
        .arg("-f")
        .arg(&torrc_path)
        .stdout(Stdio::null())
        .stderr(Stdio::piped()) // captured for diagnostics
        .spawn();

    let mut child = match child {
        Err(e) => {
            println!("[WARN] Tor: failed to start '{}': {}", tor_bin, e);
            print_torrc_hint(&hs_dir, bind_port);
            return;
        }
        Ok(c) => c,
    };

    println!(
        "[INFO] Tor: process started (pid {}). Waiting for .onion address…",
        child.id()
    );

    // ── 5. Quick health-check + hostname polling (background thread) ──────────
    let hostname_path = hs_abs.join("hostname");
    let torrc_display = torrc_path.display().to_string();
    let tor_bin_owned = tor_bin.to_string();

    std::thread::spawn(move || {
        // Give Tor ~4 seconds to either establish itself or fail fast.
        std::thread::sleep(std::time::Duration::from_secs(4));

        match child.try_wait() {
            Ok(Some(status)) => {
                // Process already exited — grab stderr for the operator.
                let stderr_text = child
                    .stderr
                    .take()
                    .map(|mut r| {
                        use std::io::Read;
                        let mut buf = String::new();
                        let _ = r.read_to_string(&mut buf);
                        buf
                    })
                    .unwrap_or_default();

                println!();
                println!("[ERR ] Tor: process exited early ({})", status);
                if !stderr_text.is_empty() {
                    println!("────── Tor stderr ──────────────────────────────");
                    // Limit output to the first 20 lines; Tor can be verbose.
                    for line in stderr_text.lines().take(20) {
                        println!("  {}", line);
                    }
                    println!("────────────────────────────────────────────────");
                }
                println!();
                print_diagnosis_hints(&torrc_display, &tor_bin_owned, bind_port);
                return;
            }
            Ok(None) => {
                // Still running — good.
            }
            Err(e) => {
                println!("[WARN] Tor: could not query process status: {}", e);
                // Continue to poll for the hostname file anyway.
            }
        }

        poll_for_hostname(&hostname_path, &torrc_display, &tor_bin_owned, bind_port);
    });
}

// ─── Hostname polling ─────────────────────────────────────────────────────────

fn poll_for_hostname(hostname_path: &Path, torrc_display: &str, tor_bin: &str, bind_port: u16) {
    const TIMEOUT_SECS: u64 = 120; // v3 onion keys can take ~60–90 s first run
    const POLL_MS: u64 = 500;

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(TIMEOUT_SECS);

    loop {
        if hostname_path.exists() {
            match std::fs::read_to_string(hostname_path) {
                Ok(raw) => {
                    let onion = raw.trim();
                    if !onion.is_empty() {
                        // ── success banner ────────────────────────────────
                        let addr_line = format!("http://{}", onion);
                        println!();
                        println!("╔══════════════════════════════════════════════════════╗");
                        println!("║        TOR ONION SERVICE ACTIVE  ✓                  ║");
                        println!("╠══════════════════════════════════════════════════════╣");
                        println!("║  {:<52}║", addr_line);
                        println!("║                                                      ║");
                        println!("║  Share this with Tor Browser users.                 ║");
                        println!("║  Your private key is stored at:                     ║");
                        println!(
                            "║    {:<48}║",
                            hostname_path.parent().unwrap_or(hostname_path).display()
                        );
                        println!("║  Back it up — losing it means a new .onion address. ║");
                        println!("╚══════════════════════════════════════════════════════╝");
                        println!();
                        return;
                    }
                    // Empty file — Tor is still writing; retry.
                }
                Err(e) => {
                    println!("[WARN] Tor: hostname unreadable: {}", e);
                }
            }
        }

        if std::time::Instant::now() >= deadline {
            println!();
            println!(
                "[WARN] Tor: timed out after {}s waiting for hostname file.",
                TIMEOUT_SECS
            );
            println!("       Expected at: {}", hostname_path.display());
            println!();
            print_diagnosis_hints(torrc_display, tor_bin, bind_port);
            return;
        }

        std::thread::sleep(std::time::Duration::from_millis(POLL_MS));
    }
}

// ─── Diagnostic helpers ───────────────────────────────────────────────────────

/// Printed when we know Tor is not installed.
fn print_install_instructions(bind_port: u16) {
    println!("[INFO] Tor not found. This server supports Tor Onion Services.");
    println!();
    println!("  ── macOS (Homebrew) ────────────────────────────────────────");
    println!("  brew install tor");
    println!("  (Re-start RustChan after installing — it configures Tor automatically.)");
    println!();
    println!("  ── Debian / Ubuntu ─────────────────────────────────────────");
    println!("  sudo apt-get install tor");
    println!("  (Re-start RustChan after installing — it configures Tor automatically.)");
    println!();
    println!("  ── Manual (any OS) ─────────────────────────────────────────");
    println!("  https://www.torproject.org/download/tor/");
    println!("  Then add to your torrc:");
    println!("    SocksPort 0");
    println!("    HiddenServiceDir /path/to/tor_hidden_service/");
    println!("    HiddenServicePort 80 127.0.0.1:{}", bind_port);
    println!();
}

/// Printed when Tor crashed or timed out, with actionable next steps.
fn print_diagnosis_hints(torrc_path: &str, tor_bin: &str, bind_port: u16) {
    println!("  ── Troubleshooting ─────────────────────────────────────────────────────");
    println!();
    println!("  1. Run Tor manually to see live error output:");
    println!("       {} -f {}", tor_bin, torrc_path);
    println!();
    println!("  2. Common causes:");
    println!();
    println!("     a) SocksPort conflict  (rare with our torrc — SocksPort is set to 0)");
    println!("        Check: lsof -i :9050   or   ss -tlnp | grep 9050");
    println!();
    println!("     b) DataDirectory permissions");
    println!("        Tor requires its data dir to be owned by the current user.");
    println!("        Fix: chmod 700 <data_dir>/tor_data/");
    println!();
    println!("     c) HiddenServiceDir permissions");
    println!("        Fix: chmod 700 <data_dir>/tor_hidden_service/");
    println!();
    println!("     d) Firewall / network — Tor needs outbound TCP on ports 9001 & 443.");
    println!("        Tor may take several minutes on a first run while it builds");
    println!("        its circuits.  Try again; the timeout is now 120 seconds.");
    println!();
    println!("     e) macOS brew service conflict");
    println!("        The brew service Tor and RustChan's Tor are independent");
    println!("        (RustChan uses SocksPort 0 + its own DataDirectory).");
    println!("        Both should coexist, but if you see lock-file errors, stop");
    println!("        the brew service first:");
    println!("          brew services stop tor");
    println!("        Then restart RustChan.");
    println!();
    println!("     f) Linux: SELinux / AppArmor");
    println!("        Check: sudo journalctl -u tor --since '5 min ago'");
    println!("        Or:    sudo ausearch -c tor | tail -20");
    println!();
    println!("  3. If Tor works but you want to manage it yourself:");
    println!("       Set  enable_tor_support = false  in settings.toml");
    println!("       and add to your own torrc:");
    println!("         HiddenServicePort 80 127.0.0.1:{}", bind_port);
    println!("  ────────────────────────────────────────────────────────────────────────");
    println!();
}

/// Printed when we cannot write the torrc / create directories.
fn print_torrc_hint(hs_dir: &Path, bind_port: u16) {
    println!("[HINT] Add the following to your torrc and restart Tor:");
    println!("         SocksPort 0");
    println!("         DataDirectory /var/lib/tor/rustchan-data/");
    println!("         HiddenServiceDir {}", hs_dir.display());
    println!("         HiddenServicePort 80 127.0.0.1:{}", bind_port);
    println!(
        "       Your .onion address will appear in: {}/hostname",
        hs_dir.display()
    );
}
