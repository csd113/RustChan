// terminal.rs — Cross-platform auto-terminal launcher.
//
// When RustChan is double-clicked from a file manager (no attached TTY) it
// re-spawns itself inside a terminal emulator so the user can see output and
// interact with the console.
//
// Guard against infinite loops: the child process is launched with
// `RUSTCHAN_SPAWNED=1` set, so if we detect that variable we skip the
// re-launch entirely and let normal startup proceed.
//
// Call order in main():
//   terminal::relaunch_in_terminal_if_needed()?;   // ← very first line

use std::process::Command;

const SPAWNED_VAR: &str = "RUSTCHAN_SPAWNED";

/// Re-spawn the current process inside a terminal emulator when stdin is not
/// a TTY and `RUSTCHAN_SPAWNED` is not already set.
///
/// Returns `Ok(true)` if a terminal was launched and the current process
/// should exit immediately.  Returns `Ok(false)` if we are already inside a
/// terminal (or running as a child) and normal startup should proceed.
pub fn relaunch_in_terminal_if_needed() -> anyhow::Result<bool> {
    // Already spawned by a previous terminal-launch attempt → run normally.
    if std::env::var(SPAWNED_VAR).is_ok() {
        return Ok(false);
    }

    // stdin is a TTY → launched from a terminal already → run normally.
    if atty::is(atty::Stream::Stdin) {
        return Ok(false);
    }

    // No TTY and not yet spawned → try to open a terminal.
    spawn_in_terminal()?;
    Ok(true)
}

// ─── Platform dispatch ────────────────────────────────────────────────────────

#[cfg(target_os = "windows")]
fn spawn_in_terminal() -> anyhow::Result<()> {
    spawn_windows()
}

#[cfg(target_os = "macos")]
fn spawn_in_terminal() -> anyhow::Result<()> {
    spawn_macos()
}

#[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
fn spawn_in_terminal() -> anyhow::Result<()> {
    spawn_linux()
}

// ─── Windows ─────────────────────────────────────────────────────────────────
//
// cmd /C "set RUSTCHAN_SPAWNED=1 && <exe> [args] || pause"
//
// `|| pause` keeps the window open when the process exits with a non-zero
// status so the user can read any error output.

#[cfg(target_os = "windows")]
fn spawn_windows() -> anyhow::Result<()> {
    let exe = current_exe_str()?;
    let args = forwarded_args();

    // Build: set RUSTCHAN_SPAWNED=1 && <exe> [args] || pause
    let mut cmd_str = format!("set {SPAWNED_VAR}=1 && {exe}");
    for arg in &args {
        cmd_str.push(' ');
        // Wrap each argument in double-quotes; escape any existing quotes.
        cmd_str.push('"');
        cmd_str.push_str(&arg.replace('"', "\\\""));
        cmd_str.push('"');
    }
    cmd_str.push_str(" || pause");

    Command::new("cmd")
        .args(["/C", &cmd_str])
        .spawn()
        .map_err(|e| anyhow::anyhow!("Failed to launch cmd.exe: {e}"))?;

    Ok(())
}

// ─── macOS ────────────────────────────────────────────────────────────────────
//
// `open -a Terminal <exe>` launches Terminal.app with <exe> as the command.
// macOS does not forward env vars through `open`, so we embed the var into
// the binary invocation via a one-liner login shell.

#[cfg(target_os = "macos")]
fn spawn_macos() -> anyhow::Result<()> {
    let exe = current_exe_str()?;
    let args = forwarded_args();

    // Build: env RUSTCHAN_SPAWNED=1 <exe> [args]
    let mut shell_cmd = format!("env {SPAWNED_VAR}=1 {exe}");
    for arg in &args {
        shell_cmd.push(' ');
        shell_cmd.push_str(&shell_quote(arg));
    }

    Command::new("open")
        .args(["-a", "Terminal", &shell_cmd])
        .spawn()
        .map_err(|e| anyhow::anyhow!("Failed to launch Terminal.app via `open`: {e}"))?;

    Ok(())
}

// ─── Linux / other Unix ───────────────────────────────────────────────────────
//
// Try common terminal emulators in preference order.  Each one is attempted
// with its own invocation style; the first one that `spawn()` succeeds on
// wins.  If none are found, print a helpful message and exit.

#[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
fn spawn_linux() -> anyhow::Result<()> {
    let exe = current_exe_str()?;
    let args = forwarded_args();

    // Build the command string that will run inside the terminal:
    //   env RUSTCHAN_SPAWNED=1 <exe> [args]
    let mut inner = format!("env {SPAWNED_VAR}=1 {exe}");
    for arg in &args {
        inner.push(' ');
        inner.push_str(&shell_quote(arg));
    }

    // Each entry: (terminal binary, args-before-inner-cmd, arg-that-precedes-cmd)
    // The inner command is always the last argument.
    let candidates: &[(&str, &[&str], &str)] = &[
        ("x-terminal-emulator", &[], "-e"),
        ("gnome-terminal", &["--"], "-e"),
        ("konsole", &[], "-e"),
        ("alacritty", &[], "-e"),
        ("xterm", &[], "-e"),
    ];

    for (terminal, prefix_args, exec_flag) in candidates {
        let mut cmd = Command::new(terminal);
        cmd.env(SPAWNED_VAR, "1");
        for a in *prefix_args {
            cmd.arg(a);
        }
        cmd.arg(exec_flag);
        cmd.arg(&inner);

        match cmd.spawn() {
            Ok(_) => return Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                // The terminal exists but failed for another reason — report it
                // but keep trying others.
                eprintln!("rustchan: {terminal} found but failed to launch: {e}");
                continue;
            }
        }
    }

    // Nothing worked.
    eprintln!("Please run RustChan from a terminal.");
    std::process::exit(1);
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// Return the path to the current executable as a `String`.
fn current_exe_str() -> anyhow::Result<String> {
    let exe = std::env::current_exe()
        .map_err(|e| anyhow::anyhow!("Could not determine current executable path: {e}"))?;
    exe.to_str()
        .map(ToOwned::to_owned)
        .ok_or_else(|| anyhow::anyhow!("Executable path contains non-UTF-8 characters"))
}

/// Collect all CLI arguments after `argv[0]` to forward to the child process.
fn forwarded_args() -> Vec<String> {
    std::env::args().skip(1).collect()
}

/// Minimal POSIX single-quote escaping so arguments survive being embedded in
/// a shell command string.  Wraps the value in single quotes and escapes any
/// embedded single quotes with the classic `'\''` trick.
fn shell_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len().saturating_add(2));
    out.push('\'');
    out.push_str(&s.replace('\'', r"'\''"));
    out.push('\'');
    out
}
