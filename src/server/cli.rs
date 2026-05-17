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
                  Config, database, logs, and uploads live in <exe-dir>/rustchan-data/.\n\
                  Run without arguments to start the server."
)]
pub struct Cli {
    /// TCP port to bind the main forum server
    #[arg(long, short = 'p', global = true)]
    pub port: Option<u16>,

    /// Enable the `ChanNet` / `RustWave` API on a second port (see `chan_net_bind` in config)
    #[arg(long = "chan-net", global = true)]
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
        /// Enable audio uploads on this board (default: audio disabled)
        #[arg(long = "audio", conflicts_with = "no_audio")]
        audio: bool,
        /// Compatibility flag; audio uploads are already disabled by default
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

#[expect(clippy::too_many_lines)]
pub fn run_admin(action: AdminAction) -> anyhow::Result<()> {
    use crate::{db, utils::crypto};
    use chrono::TimeZone as _;
    use std::io::Write as _;

    let db_path = std::path::Path::new(&crate::config::CONFIG.database_path);
    let db_parent = super::parent_dir_or_current(db_path);
    std::fs::create_dir_all(&db_parent)?;

    let pool = db::init_pool()?;
    let conn = pool.get()?;

    match action {
        AdminAction::CreateAdmin { username, password } => {
            crypto::validate_password(&password)?;
            let hash = crypto::hash_password(&password)?;
            let id = db::create_admin(&conn, &username, &hash)?;
            writeln!(
                std::io::stdout().lock(),
                "✓ Admin '{username}' created (id={id})."
            )?;
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
            writeln!(
                std::io::stdout().lock(),
                "✓ Password updated for '{username}'."
            )?;
        }
        AdminAction::ListAdmins => {
            let rows = db::list_admins(&conn)?;
            if rows.is_empty() {
                writeln!(
                    std::io::stdout().lock(),
                    "No admins. Run: rustchan-cli admin create-admin <user> <pass>"
                )?;
            } else {
                writeln!(
                    std::io::stdout().lock(),
                    "{:<6} {:<24} Created",
                    "ID",
                    "Username"
                )?;
                writeln!(std::io::stdout().lock(), "{}", "-".repeat(45))?;
                for (id, user, ts) in &rows {
                    let date = chrono::Local
                        .timestamp_opt(*ts, 0)
                        .single()
                        .map_or_else(|| "?".to_owned(), |d| d.format("%Y-%m-%d").to_string());
                    writeln!(std::io::stdout().lock(), "{id:<6} {user:<24} {date}")?;
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
            audio,
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
            let allow_audio = audio && !no_audio;
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
            writeln!(
                std::io::stdout().lock(),
                "✓ Board /{short}/ — {name}{nsfw_str} created (id={id}).{media_info}"
            )?;
        }
        AdminAction::DeleteBoard { short } => {
            let board = db::get_board_by_short(&conn, &short)?
                .ok_or_else(|| anyhow::anyhow!("Board /{short}/ not found."))?;
            {
                let mut stdout = std::io::stdout().lock();
                write!(
                    stdout,
                    "Delete /{short}/ and ALL its content? Type 'yes' to confirm: "
                )?;
                stdout.flush()?;
            }
            let mut input = String::new();
            std::io::stdin().read_line(&mut input)?;
            if input.trim() != "yes" {
                writeln!(std::io::stdout().lock(), "Aborted.")?;
                return Ok(());
            }
            db::delete_board(&conn, board.id)?;
            writeln!(std::io::stdout().lock(), "✓ Board /{short}/ deleted.")?;
        }
        AdminAction::ListBoards => {
            let boards = db::get_all_boards(&conn)?;
            if boards.is_empty() {
                writeln!(
                    std::io::stdout().lock(),
                    "No boards. Run: rustchan-cli admin create-board <short> <n>"
                )?;
            } else {
                writeln!(
                    std::io::stdout().lock(),
                    "{:<5} {:<12} {:<22} NSFW",
                    "ID",
                    "Short",
                    "Name"
                )?;
                writeln!(std::io::stdout().lock(), "{}", "-".repeat(50))?;
                for b in &boards {
                    writeln!(
                        std::io::stdout().lock(),
                        "{:<5} /{:<11} {:<22} {}",
                        b.id,
                        format!("{}/", b.short_name),
                        b.name,
                        if b.nsfw { "yes" } else { "no" }
                    )?;
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
                .and_then(|ts| chrono::Local.timestamp_opt(ts, 0).single())
                .map_or_else(
                    || "permanent".to_owned(),
                    |d| d.format("%Y-%m-%d %H:%M").to_string(),
                );
            writeln!(
                std::io::stdout().lock(),
                "✓ Ban #{id} added (expires: {exp_str})."
            )?;
        }
        AdminAction::Unban { ban_id } => {
            db::remove_ban(&conn, ban_id)?;
            writeln!(std::io::stdout().lock(), "✓ Ban #{ban_id} lifted.")?;
        }
        AdminAction::ListBans => {
            let bans = db::list_bans(&conn)?;
            if bans.is_empty() {
                writeln!(std::io::stdout().lock(), "No active bans.")?;
            } else {
                writeln!(
                    std::io::stdout().lock(),
                    "{:<5} {:<18} {:<28} Expires",
                    "ID",
                    "IP Hash (partial)",
                    "Reason"
                )?;
                writeln!(std::io::stdout().lock(), "{}", "-".repeat(75))?;
                for b in &bans {
                    // Use .get(..16) for the same defensive
                    // safety as the ip_list slice above.
                    let partial = b.ip_hash.get(..16).unwrap_or(b.ip_hash.as_str());
                    let expires = b
                        .expires_at
                        .and_then(|ts| chrono::Local.timestamp_opt(ts, 0).single())
                        .map_or_else(
                            || "Permanent".to_owned(),
                            |d| d.format("%Y-%m-%d %H:%M").to_string(),
                        );
                    let ban_id = b.id;
                    let reason = b.reason.as_deref().unwrap_or("");
                    writeln!(
                        std::io::stdout().lock(),
                        "{ban_id:<5} {partial:<18} {reason:<28} {expires}"
                    )?;
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{AdminAction, Cli, Command};
    use clap::Parser as _;

    #[test]
    fn create_board_audio_is_opt_in() {
        let cli = Cli::parse_from([
            "rustchan-cli",
            "admin",
            "create-board",
            "tech",
            "Technology",
        ]);

        let Some(Command::Admin {
            action: AdminAction::CreateBoard {
                audio, no_audio, ..
            },
        }) = cli.command
        else {
            panic!("expected create-board command");
        };

        assert!(!audio);
        assert!(!no_audio);
    }

    #[test]
    fn create_board_audio_flag_enables_audio() {
        let cli = Cli::parse_from([
            "rustchan-cli",
            "admin",
            "create-board",
            "tech",
            "Technology",
            "--audio",
        ]);

        let Some(Command::Admin {
            action: AdminAction::CreateBoard { audio, .. },
        }) = cli.command
        else {
            panic!("expected create-board command");
        };

        assert!(audio);
    }

    #[test]
    fn create_board_audio_flags_conflict() {
        let Err(err) = Cli::try_parse_from([
            "rustchan-cli",
            "admin",
            "create-board",
            "tech",
            "Technology",
            "--audio",
            "--no-audio",
        ]) else {
            panic!("audio flags should conflict");
        };

        assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
    }
}
