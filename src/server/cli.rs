// server/cli.rs — Command-line interface types and admin CLI handler.
//
// Defines the clap-based CLI structure (Cli, Command, AdminAction) and the
// synchronous `run_admin` function that executes admin subcommands against
// the database directly — no HTTP server is started.

use clap::{Parser, Subcommand};

// ─── CLI definition ───────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(
    name = "rustchan-cli",
    about = "Self-contained imageboard server",
    long_about = "RustChan Imageboard — single binary, zero dependencies.\n\
                  Data is stored in ./rustchan-data/ next to the binary.\n\
                  Run without arguments to start the server.",
    version
)]
pub struct Cli {
    /// TCP port to bind the main forum server (only used with `serve`)
    #[arg(long, short = 'p')]
    pub port: Option<u16>,

    /// Enable the `ChanNet` / `RustWave` API on a second port (only used with `serve`)
    #[arg(long = "chan-net")]
    pub chan_net: bool,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand)]
pub enum Command {
    Serve,
    Admin {
        #[command(subcommand)]
        action: AdminAction,
    },
}

#[derive(Subcommand)]
pub enum AdminAction {
    CreateAdmin {
        /// Admin username (1–64 ASCII alphanumeric / underscore characters)
        username: String,
    },
    ResetPassword {
        /// Admin username whose password will be reset
        username: String,
    },
    ListAdmins,
    CreateBoard {
        /// Short board identifier, 1–8 ASCII alphanumeric (e.g. "tech", "b")
        short: String,
        /// Human-readable board name (1–128 characters)
        name: String,
        /// Optional board description
        #[arg(default_value = None)]
        description: Option<String>,
        /// Mark board as NSFW
        #[arg(long)]
        nsfw: bool,
        /// Disable image uploads on this board (default: images allowed)
        #[arg(long = "no-images")]
        no_images: bool,
        /// Disable video uploads on this board (default: video allowed)
        #[arg(long = "no-videos")]
        no_videos: bool,
        /// Disable audio uploads on this board (default: audio allowed)
        #[arg(long = "no-audio")]
        no_audio: bool,
    },
    DeleteBoard {
        /// Short board identifier to delete
        short: String,
        /// Skip interactive confirmation
        #[arg(long, short = 'y')]
        yes: bool,
    },
    ListBoards,
    Ban {
        /// Full IP hash of the user to ban (64 hex characters)
        ip_hash: String,
        /// Reason for the ban
        reason: String,
        /// Ban duration in hours (omit or 0 for permanent, max 87600 = 10 years)
        hours: Option<u64>,
    },
    Unban {
        /// Numeric ban ID to lift (see `list-bans`)
        ban_id: u64,
    },
    ListBans,
}

// ─── Helper types ─────────────────────────────────────────────────────────────

/// Bitflags-style struct for board media permissions, replacing multiple bool
/// parameters to satisfy strict clippy lints.
#[derive(Clone, Copy)]
struct MediaFlags {
    flags: u8,
}

impl MediaFlags {
    const IMAGES: u8 = 0b0001;
    const VIDEO: u8 = 0b0010;
    const AUDIO: u8 = 0b0100;
    const NSFW: u8 = 0b1000;

    /// Build flags from a packed `u8` where each bit is pre-computed by the caller.
    const fn from_raw(flags: u8) -> Self {
        Self { flags }
    }

    const fn allow_images(self) -> bool {
        self.flags & Self::IMAGES != 0
    }

    const fn allow_video(self) -> bool {
        self.flags & Self::VIDEO != 0
    }

    const fn allow_audio(self) -> bool {
        self.flags & Self::AUDIO != 0
    }

    const fn nsfw(self) -> bool {
        self.flags & Self::NSFW != 0
    }

    const fn any_media(self) -> bool {
        self.flags & (Self::IMAGES | Self::VIDEO | Self::AUDIO) != 0
    }
}

/// Parameters for board creation.
struct CreateBoardParams<'a> {
    short: &'a str,
    name: &'a str,
    description: &'a str,
    media: MediaFlags,
}

/// Maximum ban duration in hours (10 years).
const MAX_BAN_HOURS: u64 = 87_600;

// ─── Validation helpers ───────────────────────────────────────────────────────

/// Validate an admin username: 1–64 ASCII alphanumeric or underscore.
fn validate_username(username: &str) -> anyhow::Result<()> {
    if username.is_empty() || username.len() > 64 {
        anyhow::bail!("Username must be 1-64 characters.");
    }
    if !username
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        anyhow::bail!("Username may only contain ASCII alphanumeric characters and underscores.");
    }
    Ok(())
}

/// Validate board short name: 1–8 ASCII alphanumeric, lowercased.
fn validate_board_short(short: &str) -> anyhow::Result<String> {
    let short = short.to_lowercase();
    if short.is_empty() || short.len() > 8 || !short.chars().all(|c| c.is_ascii_alphanumeric()) {
        anyhow::bail!("Short name must be 1-8 ASCII alphanumeric chars (e.g. 'tech', 'b').");
    }
    Ok(short)
}

/// Validate board name: 1–128 non-empty characters.
fn validate_board_name(name: &str) -> anyhow::Result<()> {
    let name = name.trim();
    if name.is_empty() || name.len() > 128 {
        anyhow::bail!("Board name must be 1-128 characters.");
    }
    Ok(())
}

/// Validate an IP hash: must be exactly 64 hex characters.
fn validate_ip_hash(ip_hash: &str) -> anyhow::Result<()> {
    if ip_hash.len() != 64 || !ip_hash.chars().all(|c| c.is_ascii_hexdigit()) {
        anyhow::bail!(
            "IP hash must be exactly 64 hexadecimal characters. \
             Use `list-bans` to find existing hashes."
        );
    }
    Ok(())
}

/// Validate ban reason: 1–512 characters, non-empty.
fn validate_ban_reason(reason: &str) -> anyhow::Result<()> {
    let reason = reason.trim();
    if reason.is_empty() || reason.len() > 512 {
        anyhow::bail!("Ban reason must be 1-512 characters.");
    }
    Ok(())
}

// ─── Password I/O helpers ─────────────────────────────────────────────────────

/// Read a line from stdin with echo disabled (Unix) or plain read (fallback).
#[cfg(unix)]
fn read_password_from_stdin() -> anyhow::Result<String> {
    use std::io::BufRead;
    use std::os::unix::io::AsRawFd;

    let stdin = std::io::stdin();
    let fd = stdin.as_raw_fd();

    // Save current terminal settings.
    let old_termios = unsafe {
        let mut t = std::mem::zeroed::<libc::termios>();
        let t_ptr = std::ptr::from_mut::<libc::termios>(&mut t);
        if libc::tcgetattr(fd, t_ptr) != 0 {
            // Cannot get terminal attrs — fall back to plain read.
            let mut line = String::new();
            stdin.lock().read_line(&mut line)?;
            return Ok(line
                .trim_end_matches('\n')
                .trim_end_matches('\r')
                .to_string());
        }
        t
    };

    // Disable echo.
    let mut new_termios = old_termios;
    new_termios.c_lflag &= !libc::ECHO;
    unsafe {
        let new_ptr = std::ptr::from_ref::<libc::termios>(&new_termios);
        libc::tcsetattr(fd, libc::TCSANOW, new_ptr);
    }

    let mut line = String::new();
    let result = stdin.lock().read_line(&mut line);

    // Restore terminal settings regardless of read result.
    unsafe {
        let old_ptr = std::ptr::from_ref::<libc::termios>(&old_termios);
        libc::tcsetattr(fd, libc::TCSANOW, old_ptr);
    }

    result?;
    Ok(line
        .trim_end_matches('\n')
        .trim_end_matches('\r')
        .to_string())
}

#[cfg(not(unix))]
fn read_password_from_stdin() -> anyhow::Result<String> {
    use std::io::BufRead;

    let stdin = std::io::stdin();
    let mut line = String::new();
    stdin.lock().read_line(&mut line)?;
    Ok(line
        .trim_end_matches('\n')
        .trim_end_matches('\r')
        .to_string())
}

/// Prompt for a password interactively via stderr (no echo).
/// Falls back to plain stdin reading if terminal echo cannot be disabled.
fn prompt_password(prompt: &str) -> anyhow::Result<String> {
    use std::io::Write;

    eprint!("{prompt}");
    std::io::stderr().flush()?;

    let password = read_password_from_stdin()?;

    // Print a newline since the user's Enter was not echoed.
    eprintln!();

    if password.is_empty() {
        anyhow::bail!("Password cannot be empty.");
    }

    Ok(password)
}

/// Securely zero a `String`'s backing buffer before dropping.
fn zeroize_string(s: &mut String) {
    // SAFETY: We overwrite every byte with 0 via volatile writes to prevent
    // the compiler from optimising the zeroing away, then truncate the length.
    let bytes = unsafe { s.as_mut_vec() };
    for b in bytes.iter_mut() {
        unsafe {
            std::ptr::write_volatile(std::ptr::from_mut::<u8>(b), 0);
        }
    }
    s.clear();
}

// ─── Media flag packing ───────────────────────────────────────────────────────

/// Pack an nsfw flag and a pre-built media byte into `MediaFlags`.
const fn pack_media_flags(nsfw: bool, media_byte: u8) -> MediaFlags {
    let raw = if nsfw {
        media_byte | MediaFlags::NSFW
    } else {
        media_byte
    };
    MediaFlags::from_raw(raw)
}

// ─── Admin CLI mode ───────────────────────────────────────────────────────────

pub fn run_admin(action: AdminAction) -> anyhow::Result<()> {
    let db_path = std::path::Path::new(&crate::config::CONFIG.database_path);

    // Normalise an empty parent to "." so that `create_dir_all("")` does not
    // fail with `NotFound` for bare filenames like "rustchan.db".
    let db_parent: std::path::PathBuf = match db_path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => std::path::PathBuf::from("."),
    };
    std::fs::create_dir_all(&db_parent).map_err(|e| {
        anyhow::anyhow!(
            "Cannot create database directory '{}': {e}",
            db_parent.display()
        )
    })?;

    let pool = crate::db::init_pool().map_err(|e| {
        anyhow::anyhow!("Cannot open database (is the server already running?): {e}")
    })?;
    let conn = pool.get().map_err(|e| {
        anyhow::anyhow!("Cannot acquire database connection (is the server already running?): {e}")
    })?;

    run_admin_action(&conn, action)?;

    drop(conn);
    drop(pool);

    Ok(())
}

fn run_admin_action(conn: &rusqlite::Connection, action: AdminAction) -> anyhow::Result<()> {
    match action {
        AdminAction::CreateAdmin { username } => run_create_admin(conn, &username),
        AdminAction::ResetPassword { username } => run_reset_password(conn, &username),
        AdminAction::ListAdmins => run_list_admins(conn),
        AdminAction::CreateBoard {
            short,
            name,
            description,
            nsfw,
            no_images,
            no_videos,
            no_audio,
        } => {
            // Pack media bools into a byte, then combine with nsfw via helper
            // that only takes 2 params (bool + u8), avoiding the 3-bool limit.
            let mut media_byte: u8 = 0;
            if !no_images {
                media_byte |= MediaFlags::IMAGES;
            }
            if !no_videos {
                media_byte |= MediaFlags::VIDEO;
            }
            if !no_audio {
                media_byte |= MediaFlags::AUDIO;
            }
            let params = CreateBoardParams {
                short: &short,
                name: &name,
                description: description.as_deref().unwrap_or(""),
                media: pack_media_flags(nsfw, media_byte),
            };
            run_create_board(conn, &params)
        }
        AdminAction::DeleteBoard { short, yes } => run_delete_board(conn, &short, yes),
        AdminAction::ListBoards => run_list_boards(conn),
        AdminAction::Ban {
            ip_hash,
            reason,
            hours,
        } => run_ban(conn, &ip_hash, &reason, hours),
        AdminAction::Unban { ban_id } => run_unban(conn, ban_id),
        AdminAction::ListBans => run_list_bans(conn),
    }
}

// ─── Individual command handlers ──────────────────────────────────────────────

fn run_create_admin(conn: &rusqlite::Connection, username: &str) -> anyhow::Result<()> {
    use crate::{db, utils::crypto};

    validate_username(username)?;

    if db::get_admin_by_username(conn, username)?.is_some() {
        anyhow::bail!("Admin '{username}' already exists.");
    }

    let mut password = prompt_password("Enter password: ")?;
    let mut confirm = prompt_password("Confirm password: ")?;

    if password != confirm {
        zeroize_string(&mut password);
        zeroize_string(&mut confirm);
        anyhow::bail!("Passwords do not match.");
    }
    zeroize_string(&mut confirm);

    let validate_result = crypto::validate_password(&password);
    if let Err(e) = validate_result {
        zeroize_string(&mut password);
        return Err(e);
    }

    let hash_result = crypto::hash_password(&password);
    zeroize_string(&mut password);
    let hash = hash_result?;

    let id = db::create_admin(conn, username, &hash)?;
    println!("[OK] Admin '{username}' created (id={id}).");
    Ok(())
}

fn run_reset_password(conn: &rusqlite::Connection, username: &str) -> anyhow::Result<()> {
    use crate::{db, utils::crypto};

    validate_username(username)?;

    db::get_admin_by_username(conn, username)?
        .ok_or_else(|| anyhow::anyhow!("Admin '{username}' not found."))?;

    let mut new_password = prompt_password("Enter new password: ")?;
    let mut confirm = prompt_password("Confirm new password: ")?;

    if new_password != confirm {
        zeroize_string(&mut new_password);
        zeroize_string(&mut confirm);
        anyhow::bail!("Passwords do not match.");
    }
    zeroize_string(&mut confirm);

    let validate_result = crypto::validate_password(&new_password);
    if let Err(e) = validate_result {
        zeroize_string(&mut new_password);
        return Err(e);
    }

    let hash_result = crypto::hash_password(&new_password);
    zeroize_string(&mut new_password);
    let hash = hash_result?;

    if db::get_admin_by_username(conn, username)?.is_none() {
        anyhow::bail!("Admin '{username}' was deleted before the password could be updated.");
    }
    db::update_admin_password(conn, username, &hash)?;

    println!("[OK] Password updated for '{username}'.");
    Ok(())
}

fn run_list_admins(conn: &rusqlite::Connection) -> anyhow::Result<()> {
    use crate::db;
    use chrono::TimeZone;

    let rows = db::list_admins(conn)?;
    if rows.is_empty() {
        println!("No admins. Run: rustchan-cli admin create-admin <username>");
    } else {
        println!("{:<6} {:<24} Created (UTC)", "ID", "Username");
        println!("{}", "-".repeat(50));
        for (id, user, ts) in &rows {
            let date = chrono::Utc
                .timestamp_opt(*ts, 0)
                .single()
                .map_or_else(|| "?".to_string(), |d| d.format("%Y-%m-%d").to_string());
            println!("{id:<6} {user:<24} {date}");
        }
    }
    Ok(())
}

fn run_create_board(
    conn: &rusqlite::Connection,
    params: &CreateBoardParams<'_>,
) -> anyhow::Result<()> {
    use crate::db;

    let short = validate_board_short(params.short)?;
    validate_board_name(params.name)?;

    if params.description.len() > 1024 {
        anyhow::bail!("Board description must be at most 1024 characters.");
    }

    if !params.media.any_media() {
        eprintln!("Warning: all media types disabled -- this board will be text-only.");
    }

    if db::get_board_by_short(conn, &short)?.is_some() {
        anyhow::bail!("Board /{short}/ already exists.");
    }

    let name = params.name;
    let description = params.description;
    let nsfw = params.media.nsfw();
    let allow_images = params.media.allow_images();
    let allow_video = params.media.allow_video();
    let allow_audio = params.media.allow_audio();

    let id = db::create_board_with_media_flags(
        conn,
        &short,
        name,
        description,
        nsfw,
        allow_images,
        allow_video,
        allow_audio,
    )?;

    let nsfw_str = if nsfw { " [NSFW]" } else { "" };
    let img = if allow_images { "yes" } else { "no" };
    let vid = if allow_video { "yes" } else { "no" };
    let aud = if allow_audio { "yes" } else { "no" };
    println!("[OK] Board /{short}/ -- {name}{nsfw_str} created (id={id}).  images:{img} video:{vid} audio:{aud}");
    Ok(())
}

fn run_delete_board(
    conn: &rusqlite::Connection,
    short: &str,
    skip_confirm: bool,
) -> anyhow::Result<()> {
    use std::io::Write;

    let short = short.to_lowercase();

    let board = crate::db::get_board_by_short(conn, &short)?
        .ok_or_else(|| anyhow::anyhow!("Board /{short}/ not found."))?;

    if !skip_confirm {
        print!("Delete /{short}/ and ALL its content? Type 'yes' to confirm: ");
        std::io::stdout().flush()?;
        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        if input.trim() != "yes" {
            println!("Aborted.");
            return Ok(());
        }
    }

    if crate::db::get_board_by_short(conn, &short)?.is_none() {
        anyhow::bail!("Board /{short}/ was already deleted by another process.");
    }

    crate::db::delete_board(conn, board.id)?;

    let media_dir = std::path::Path::new("rustchan-data")
        .join("media")
        .join(&short);
    if media_dir.is_dir() {
        if let Err(e) = std::fs::remove_dir_all(&media_dir) {
            eprintln!(
                "Warning: board deleted but could not remove media directory '{}': {e}",
                media_dir.display()
            );
        }
    }

    println!("[OK] Board /{short}/ deleted.");
    Ok(())
}

fn run_list_boards(conn: &rusqlite::Connection) -> anyhow::Result<()> {
    let boards = crate::db::get_all_boards(conn)?;
    if boards.is_empty() {
        println!("No boards. Run: rustchan-cli admin create-board <short> <name>");
    } else {
        println!("{:<5} {:<12} {:<22} NSFW", "ID", "Short", "Name");
        println!("{}", "-".repeat(50));
        for b in &boards {
            let short_display = format!("/{}/", b.short_name);
            let nsfw_display = if b.nsfw { "yes" } else { "no" };
            println!(
                "{:<5} {:<12} {:<22} {nsfw_display}",
                b.id, short_display, b.name,
            );
        }
    }
    Ok(())
}

fn run_ban(
    conn: &rusqlite::Connection,
    ip_hash: &str,
    reason: &str,
    hours: Option<u64>,
) -> anyhow::Result<()> {
    use chrono::TimeZone;

    validate_ip_hash(ip_hash)?;
    validate_ban_reason(reason)?;

    let (expires, clamped) = match hours {
        Some(0) | None => (None, false),
        Some(h) => {
            let effective = h.min(MAX_BAN_HOURS);
            let clamped = effective < h;
            let seconds = effective.saturating_mul(3600);
            let now = chrono::Utc::now().timestamp();
            let seconds_i64 =
                i64::try_from(seconds).map_err(|_| anyhow::anyhow!("Ban duration overflow"))?;
            let expires_ts = now.saturating_add(seconds_i64);
            (Some(expires_ts), clamped)
        }
    };

    if clamped {
        eprintln!(
            "Warning: ban duration clamped to the maximum of {MAX_BAN_HOURS} hours (10 years)."
        );
    }

    let id = crate::db::add_ban(conn, ip_hash, reason, expires)?;

    let exp_str = expires
        .and_then(|ts| chrono::Utc.timestamp_opt(ts, 0).single())
        .map_or_else(
            || "permanent".to_string(),
            |d| d.format("%Y-%m-%d %H:%M UTC").to_string(),
        );
    println!("[OK] Ban #{id} added (expires: {exp_str}).");
    Ok(())
}

fn run_unban(conn: &rusqlite::Connection, ban_id: u64) -> anyhow::Result<()> {
    let ban_id_i64 =
        i64::try_from(ban_id).map_err(|_| anyhow::anyhow!("Ban ID {ban_id} is out of range."))?;

    let bans = crate::db::list_bans(conn)?;
    let ban_exists = bans.iter().any(|b| b.id == ban_id_i64);
    if !ban_exists {
        anyhow::bail!("Ban #{ban_id} not found or already lifted.");
    }

    crate::db::remove_ban(conn, ban_id_i64)?;

    println!("[OK] Ban #{ban_id} lifted.");
    Ok(())
}

fn run_list_bans(conn: &rusqlite::Connection) -> anyhow::Result<()> {
    use chrono::TimeZone;

    let bans = crate::db::list_bans(conn)?;
    if bans.is_empty() {
        println!("No active bans.");
    } else {
        println!(
            "{:<6} {:<18} {:<28} Expires",
            "ID", "IP Hash (partial)", "Reason"
        );
        println!("{}", "-".repeat(76));
        for b in &bans {
            let partial = b.ip_hash.get(..16).unwrap_or(b.ip_hash.as_str());
            let partial_display = if b.ip_hash.len() > 16 {
                format!("{partial}...")
            } else {
                partial.to_string()
            };
            let expires = b
                .expires_at
                .and_then(|ts| chrono::Utc.timestamp_opt(ts, 0).single())
                .map_or_else(
                    || "Permanent".to_string(),
                    |d| d.format("%Y-%m-%d %H:%M UTC").to_string(),
                );
            let ban_id = b.id;
            let reason = b.reason.as_deref().unwrap_or("");
            let reason_display = if reason.len() > 27 {
                format!("{}...", reason.get(..24).unwrap_or(reason))
            } else {
                reason.to_string()
            };
            println!("{ban_id:<6} {partial_display:<18} {reason_display:<28} {expires}");
        }
    }
    Ok(())
}
