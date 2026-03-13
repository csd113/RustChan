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
                  Run without arguments to start the server."
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand)]
pub enum Command {
    Serve {
        #[arg(long, short = 'p')]
        port: Option<u16>,
    },
    Admin {
        #[command(subcommand)]
        action: AdminAction,
    },
}

#[derive(Subcommand)]
pub enum AdminAction {
    CreateAdmin {
        username: String,
        password: String,
    },
    ResetPassword {
        username: String,
        new_password: String,
    },
    ListAdmins,
    CreateBoard {
        short: String,
        name: String,
        #[arg(default_value = "")]
        description: String,
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
        short: String,
    },
    ListBoards,
    Ban {
        ip_hash: String,
        reason: String,
        hours: Option<i64>,
    },
    Unban {
        ban_id: i64,
    },
    ListBans,
}

// ─── Admin CLI mode ───────────────────────────────────────────────────────────

#[allow(clippy::too_many_lines)]
#[allow(clippy::arithmetic_side_effects)]
pub fn run_admin(action: AdminAction) -> anyhow::Result<()> {
    use crate::{db, utils::crypto};
    use chrono::TimeZone;
    use std::io::Write;

    let db_path = std::path::Path::new(&crate::config::CONFIG.database_path);

    // FIX[AUDIT-6]: Apply the same Fix #9 empty-parent guard used in
    // `run_server`.  The original code used a plain `if let Some(parent)`
    // check, which does NOT handle the case where `Path::parent()` returns
    // `Some("")` for a bare filename (e.g. "rustchan.db").
    // `create_dir_all("")` fails with `NotFound`, so we normalise an empty
    // parent to `"."` just as `run_server` does.
    let db_parent: std::path::PathBuf = match db_path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => std::path::PathBuf::from("."),
    };
    std::fs::create_dir_all(&db_parent)?;

    let pool = db::init_pool()?;
    let conn = pool.get()?;

    match action {
        AdminAction::CreateAdmin { username, password } => {
            crypto::validate_password(&password)?;
            let hash = crypto::hash_password(&password)?;
            let id = db::create_admin(&conn, &username, &hash)?;
            println!("✓ Admin '{username}' created (id={id}).");
        }
        AdminAction::ResetPassword {
            username,
            new_password,
        } => {
            crypto::validate_password(&new_password)?;
            db::get_admin_by_username(&conn, &username)?
                .ok_or_else(|| anyhow::anyhow!("Admin '{username}' not found."))?;
            let hash = crypto::hash_password(&new_password)?;
            db::update_admin_password(&conn, &username, &hash)?;
            println!("✓ Password updated for '{username}'.");
        }
        AdminAction::ListAdmins => {
            let rows = db::list_admins(&conn)?;
            if rows.is_empty() {
                println!("No admins. Run: rustchan-cli admin create-admin <user> <pass>");
            } else {
                println!("{:<6} {:<24} Created", "ID", "Username");
                println!("{}", "-".repeat(45));
                for (id, user, ts) in &rows {
                    let date = chrono::Utc
                        .timestamp_opt(*ts, 0)
                        .single()
                        .map_or_else(|| "?".to_string(), |d| d.format("%Y-%m-%d").to_string());
                    println!("{id:<6} {user:<24} {date}");
                }
            }
        }
        AdminAction::CreateBoard {
            short,
            name,
            description,
            nsfw,
            no_images,
            no_videos,
            no_audio,
        } => {
            let short = short.to_lowercase();
            if short.is_empty()
                || short.len() > 8
                || !short.chars().all(|c| c.is_ascii_alphanumeric())
            {
                anyhow::bail!("Short name must be 1-8 alphanumeric chars (e.g. 'tech', 'b').");
            }
            let allow_images = !no_images;
            let allow_video = !no_videos;
            let allow_audio = !no_audio;
            let id = db::create_board_with_media_flags(
                &conn,
                &short,
                &name,
                &description,
                nsfw,
                allow_images,
                allow_video,
                allow_audio,
            )?;
            let nsfw_str = if nsfw { " [NSFW]" } else { "" };
            let media_info = format!(
                "  images:{} video:{} audio:{}",
                if allow_images { "yes" } else { "no" },
                if allow_video { "yes" } else { "no" },
                if allow_audio { "yes" } else { "no" },
            );
            println!("✓ Board /{short}/ — {name}{nsfw_str} created (id={id}).{media_info}");
        }
        AdminAction::DeleteBoard { short } => {
            let board = db::get_board_by_short(&conn, &short)?
                .ok_or_else(|| anyhow::anyhow!("Board /{short}/ not found."))?;
            print!("Delete /{short}/ and ALL its content? Type 'yes' to confirm: ");
            std::io::stdout().flush()?;
            let mut input = String::new();
            std::io::stdin().read_line(&mut input)?;
            if input.trim() != "yes" {
                println!("Aborted.");
                return Ok(());
            }
            db::delete_board(&conn, board.id)?;
            println!("✓ Board /{short}/ deleted.");
        }
        AdminAction::ListBoards => {
            let boards = db::get_all_boards(&conn)?;
            if boards.is_empty() {
                println!("No boards. Run: rustchan-cli admin create-board <short> <n>");
            } else {
                println!("{:<5} {:<12} {:<22} NSFW", "ID", "Short", "Name");
                println!("{}", "-".repeat(50));
                for b in &boards {
                    println!(
                        "{:<5} /{:<11} {:<22} {}",
                        b.id,
                        format!("{}/", b.short_name),
                        b.name,
                        if b.nsfw { "yes" } else { "no" }
                    );
                }
            }
        }
        AdminAction::Ban {
            ip_hash,
            reason,
            hours,
        } => {
            let expires = hours
                .filter(|&h| h > 0)
                .map(|h| chrono::Utc::now().timestamp() + h.min(87_600).saturating_mul(3600));
            let id = db::add_ban(&conn, &ip_hash, &reason, expires)?;
            let exp_str = expires
                .and_then(|ts| chrono::Utc.timestamp_opt(ts, 0).single())
                .map_or_else(
                    || "permanent".to_string(),
                    |d| d.format("%Y-%m-%d %H:%M UTC").to_string(),
                );
            println!("✓ Ban #{id} added (expires: {exp_str}).");
        }
        AdminAction::Unban { ban_id } => {
            db::remove_ban(&conn, ban_id)?;
            println!("✓ Ban #{ban_id} lifted.");
        }
        AdminAction::ListBans => {
            let bans = db::list_bans(&conn)?;
            if bans.is_empty() {
                println!("No active bans.");
            } else {
                println!(
                    "{:<5} {:<18} {:<28} Expires",
                    "ID", "IP Hash (partial)", "Reason"
                );
                println!("{}", "-".repeat(75));
                for b in &bans {
                    // FIX[AUDIT-3]: Use .get(..16) for the same defensive
                    // safety as the ip_list slice above.
                    let partial = b.ip_hash.get(..16).unwrap_or(b.ip_hash.as_str());
                    let expires = b
                        .expires_at
                        .and_then(|ts| chrono::Utc.timestamp_opt(ts, 0).single())
                        .map_or_else(
                            || "Permanent".to_string(),
                            |d| d.format("%Y-%m-%d %H:%M").to_string(),
                        );
                    let ban_id = b.id;
                    let reason = b.reason.as_deref().unwrap_or("");
                    println!("{ban_id:<5} {partial:<18} {reason:<28} {expires}");
                }
            }
        }
    }
    Ok(())
}
