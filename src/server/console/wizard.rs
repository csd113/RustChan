// server/console/wizard.rs — Multi-step admin wizards.
//
// These wizards need read_line() for interactive input and cannot run inside
// crossterm raw mode. Pattern:
//
//   1. Disable raw mode, leave alternate screen  → terminal looks normal
//   2. Run the blocking wizard (prompts + read_line)
//   3. Wait for Enter so the operator can read the result
//   4. Re-enable raw mode, re-enter alternate screen, clear for a fresh frame
//   5. Set ConsoleMode back to Dashboard so the render task resumes
//
// run_wizard() must be called from tokio::task::spawn_blocking so it does not
// block the async runtime.
//
// kb_* functions are ported verbatim from the old console.rs.

use super::{ConsoleMode, SharedConsoleMode, WizardKind};
use crate::db::DbPool;
use crossterm::{cursor, execute, terminal};
use std::io::{stdout, BufRead, BufReader};

// ─── Entry point ─────────────────────────────────────────────────────────────

pub fn run_wizard(kind: &WizardKind, pool: &DbPool, mode: &SharedConsoleMode) {
    // 1. Exit raw mode.
    let _ = terminal::disable_raw_mode();
    let _ = execute!(stdout(), terminal::LeaveAlternateScreen, cursor::Show);

    // 2. Run the wizard, then consume the "press Enter" prompt — all within
    //    a single scope so the StdinLock (significant Drop) is released before
    //    we re-enter raw mode.
    {
        let stdin_handle = std::io::stdin();
        let mut reader = BufReader::new(stdin_handle.lock());

        match kind {
            WizardKind::CreateBoard => kb_create_board(pool, &mut reader),
            WizardKind::CreateAdmin => kb_create_admin(pool, &mut reader),
            WizardKind::DeleteThread => kb_delete_thread(pool, &mut reader),
        }

        // 3. Hold so the operator can read the result.
        {
            use std::io::Write as _;
            print!("\n  Press Enter to return to the dashboard\u{2026}");
            let _ = stdout().flush();
        }
        let mut buf = String::new();
        let _ = reader.read_line(&mut buf);
    } // StdinLock dropped here, before raw mode is re-entered.

    // 4. Re-enter raw mode and alternate screen.
    let _ = terminal::enable_raw_mode();
    let _ = execute!(
        stdout(),
        terminal::EnterAlternateScreen,
        terminal::Clear(terminal::ClearType::All),
        cursor::Hide,
        cursor::MoveTo(0, 0),
    );

    // 5. Reset mode so the render task resumes drawing.
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        handle.block_on(async {
            *mode.write().await = ConsoleMode::Dashboard;
        });
    }
}

// ─── ANSI / prompt helpers ────────────────────────────────────────────────────

fn c(code: &'static str) -> &'static str {
    if crate::logging::is_tty() {
        code
    } else {
        ""
    }
}

const RST: &str = "\x1b[0m";
const RED: &str = "\x1b[31m";
const GRN: &str = "\x1b[32m";
const YLW: &str = "\x1b[33m";
const CYN: &str = "\x1b[36m";
const BLD: &str = "\x1b[1m";

fn prompt_username(reader: &mut dyn BufRead) -> Option<String> {
    loop {
        crate::logging::console_prompt(&format!("  {}Username:{} ", c(CYN), c(RST)));
        let mut s = String::new();
        match reader.read_line(&mut s) {
            Ok(0) | Err(_) => {
                crate::logging::console_println(
                    "\n  Skipped — run: rustchan-cli admin create-admin <user> <pass>",
                );
                return None;
            }
            Ok(_) => {}
        }
        let u = s.trim().to_string();
        if u.is_empty() {
            crate::logging::console_println("  Username cannot be empty.");
            continue;
        }
        if u.len() > 32 {
            crate::logging::console_println("  Username must be 32 characters or fewer.");
            continue;
        }
        if !u
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
        {
            crate::logging::console_println(
                "  Username must be alphanumeric (underscores and hyphens allowed).",
            );
            continue;
        }
        return Some(u);
    }
}

fn prompt_password(reader: &mut dyn BufRead) -> Option<String> {
    loop {
        crate::logging::console_prompt(&format!("  {}Password (min 8 chars):{} ", c(CYN), c(RST)));
        let mut p1 = String::new();
        match reader.read_line(&mut p1) {
            Ok(0) | Err(_) => {
                crate::logging::console_println("\n  Skipped.");
                return None;
            }
            Ok(_) => {}
        }
        let p1 = p1.trim().to_string();
        if let Err(e) = crate::utils::crypto::validate_password(&p1) {
            crate::logging::console_println(&format!("  {}✗{} {e}", c(RED), c(RST)));
            continue;
        }
        crate::logging::console_prompt(&format!("  {}Confirm password:{}   ", c(CYN), c(RST)));
        let mut p2 = String::new();
        if reader.read_line(&mut p2).is_err() {
            crate::logging::console_println("\n  Skipped.");
            return None;
        }
        let p2 = p2.trim().to_string();
        if p1 != p2 {
            crate::logging::console_println(&format!(
                "  {}✗{} Passwords do not match. Try again.",
                c(RED),
                c(RST),
            ));
            continue;
        }
        return Some(p1);
    }
}

// ─── kb_create_board ─────────────────────────────────────────────────────────

#[allow(clippy::too_many_lines)]
pub fn kb_create_board(pool: &DbPool, reader: &mut dyn BufRead) {
    let prompt = |msg: &str, reader: &mut dyn BufRead| -> Option<String> {
        crate::logging::console_prompt(msg);
        let mut s = String::new();
        match reader.read_line(&mut s) {
            Ok(0) | Err(_) => None,
            Ok(_) => Some(s.trim().to_string()),
        }
    };

    let short = match prompt(
        &format!("  {}Short name (e.g. 'tech'):{} ", c(CYN), c(RST)),
        reader,
    ) {
        Some(v) if !v.is_empty() => v,
        _ => {
            crate::logging::console_println("  Aborted.");
            return;
        }
    };

    let short_lc = short.to_lowercase();
    if short_lc.is_empty()
        || short_lc.len() > 8
        || !short_lc.chars().all(|ch| ch.is_ascii_alphanumeric())
    {
        crate::logging::console_println(&format!(
            "  {}[err]{} Short name must be 1-8 alphanumeric characters.",
            c(RED),
            c(RST),
        ));
        return;
    }

    let name = match prompt(&format!("  {}Display name:{} ", c(CYN), c(RST)), reader) {
        Some(v) if !v.is_empty() => v,
        _ => {
            crate::logging::console_println("  Aborted.");
            return;
        }
    };

    let desc = prompt(
        &format!("  {}Description (blank = none):{} ", c(CYN), c(RST)),
        reader,
    )
    .unwrap_or_default();
    let nsfw_raw =
        prompt(&format!("  {}NSFW? [y/N]:{} ", c(CYN), c(RST)), reader).unwrap_or_default();
    let nsfw = matches!(nsfw_raw.to_lowercase().as_str(), "y" | "yes");

    let no_img = prompt(
        &format!("  {}Disable images? [y/N]:{} ", c(CYN), c(RST)),
        reader,
    )
    .unwrap_or_default();
    let no_vid = prompt(
        &format!("  {}Disable video?  [y/N]:{} ", c(CYN), c(RST)),
        reader,
    )
    .unwrap_or_default();
    let no_aud = prompt(
        &format!("  {}Disable audio?  [y/N]:{} ", c(CYN), c(RST)),
        reader,
    )
    .unwrap_or_default();

    let allow_images = !matches!(no_img.to_lowercase().as_str(), "y" | "yes");
    let allow_video = !matches!(no_vid.to_lowercase().as_str(), "y" | "yes");
    let allow_audio = !matches!(no_aud.to_lowercase().as_str(), "y" | "yes");

    let Ok(conn) = pool.get() else {
        crate::logging::console_println(&format!(
            "  {}[err]{} Could not get DB connection.",
            c(RED),
            c(RST)
        ));
        return;
    };

    match crate::db::create_board_with_media_flags(
        &conn,
        &short_lc,
        &name,
        &desc,
        nsfw,
        allow_images,
        allow_video,
        allow_audio,
    ) {
        Ok(id) => {
            tracing::info!(
                target: "console",
                board = %short_lc, name = %name, id = id,
                "Board created via console",
            );
            crate::logging::console_println(&format!(
                "  {}✓{} Board /{short_lc}/  — {name}{}  created (id={id}).",
                c(GRN),
                c(RST),
                if nsfw { " [NSFW]" } else { "" },
            ));
        }
        Err(e) => crate::logging::console_println(&format!("  {}[err]{} {e}", c(RED), c(RST))),
    }
    crate::logging::console_println("");
}

// ─── kb_create_admin ─────────────────────────────────────────────────────────

#[allow(clippy::too_many_lines)]
pub fn kb_create_admin(pool: &DbPool, reader: &mut dyn BufRead) {
    crate::logging::console_print_raw(&format!(
        "\n  {}── Create Admin Account ─────────────────────────────────{}\n\n",
        c(CYN),
        c(RST),
    ));

    if crate::logging::is_tty() {
        crate::logging::console_println(&format!(
            "  {}Note: password input is visible in terminal.{}",
            c(YLW),
            c(RST),
        ));
    }

    let Some(username) = prompt_username(reader) else {
        return;
    };
    let Some(password) = prompt_password(reader) else {
        return;
    };

    let Ok(hash) = crate::utils::crypto::hash_password(&password) else {
        crate::logging::console_println(&format!(
            "  {}[err]{} Failed to hash password.",
            c(RED),
            c(RST)
        ));
        return;
    };
    let Ok(conn) = pool.get() else {
        crate::logging::console_println(&format!(
            "  {}[err]{} Could not get DB connection.",
            c(RED),
            c(RST)
        ));
        return;
    };

    match crate::db::create_admin(&conn, &username, &hash) {
        Ok(id) => {
            tracing::info!(
                target: "console", username = %username, id = id,
                "Admin account created via console",
            );
            crate::logging::console_println(&format!(
                "  {}✓{} Admin '{}{username}{}' created (id={id}).",
                c(GRN),
                c(RST),
                c(BLD),
                c(RST),
            ));
        }
        Err(e) => crate::logging::console_println(&format!("  {}[err]{} {e}", c(RED), c(RST))),
    }
    crate::logging::console_println("");
}

// ─── kb_delete_thread ────────────────────────────────────────────────────────

pub fn kb_delete_thread(pool: &DbPool, reader: &mut dyn BufRead) {
    crate::logging::console_prompt(&format!("  {}Thread ID to delete:{} ", c(CYN), c(RST)));
    let mut s = String::new();
    if reader.read_line(&mut s).is_err() {
        return;
    }

    let Ok(thread_id) = s.trim().parse::<i64>() else {
        crate::logging::console_println(&format!(
            "  {}[err]{} '{}' is not a valid thread ID.",
            c(RED),
            c(RST),
            s.trim()
        ));
        return;
    };

    let Ok(conn) = pool.get() else {
        crate::logging::console_println(&format!(
            "  {}[err]{} Could not get DB connection.",
            c(RED),
            c(RST)
        ));
        return;
    };

    let exists: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM threads WHERE id = ?1",
            [thread_id],
            |r| r.get(0),
        )
        .unwrap_or(0);
    if exists == 0 {
        crate::logging::console_println(&format!(
            "  {}[err]{} Thread {thread_id} not found.",
            c(RED),
            c(RST)
        ));
        return;
    }

    crate::logging::console_prompt(&format!(
        "  {}Delete thread {thread_id} and all its posts? [y/N]:{} ",
        c(YLW),
        c(RST)
    ));
    let mut confirm = String::new();
    if reader.read_line(&mut confirm).is_err() {
        return;
    }
    if !matches!(confirm.trim().to_lowercase().as_str(), "y" | "yes") {
        crate::logging::console_println("  Aborted.");
        return;
    }

    match crate::db::delete_thread(&conn, thread_id) {
        Ok(paths) => {
            let n = paths.len();
            for p in &paths {
                crate::utils::files::delete_file(&crate::config::CONFIG.upload_dir, p);
            }
            tracing::info!(
                target: "console", thread_id = thread_id, files_removed = n,
                "Thread deleted via console",
            );
            crate::logging::console_println(&format!(
                "  {}✓{} Thread {thread_id} deleted ({n} file(s) removed).",
                c(GRN),
                c(RST),
            ));
        }
        Err(e) => crate::logging::console_println(&format!("  {}[err]{} {e}", c(RED), c(RST))),
    }
    crate::logging::console_println("");
}
