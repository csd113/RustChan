//! Command-line interface types and the synchronous administration handler.

use clap::{Parser, Subcommand};

// ─── CLI definition ───────────────────────────────────────────────────────────

#[derive(Debug, Parser)]
#[command(
    name = "rustchan-cli",
    version,
    about = "Self-contained imageboard server",
    long_about = "RustChan Imageboard — single binary, zero dependencies.\n\
                  Config, database, logs, and uploads default to <exe-dir>/rustchan-data/.\n\
                  Use --data-dir with an absolute path to select another location.\n\
                  Run without arguments to start the server."
)]
/// Top-level command-line arguments.
pub struct Cli {
    /// Absolute directory for config, database, uploads, logs, and backups
    #[arg(long, global = true, value_name = "PATH")]
    pub data_dir: Option<std::path::PathBuf>,

    /// TCP port to bind the main forum server
    #[arg(long, short = 'p', global = true)]
    pub port: Option<u16>,

    /// Enable the `ChanNet` / `RustWave` API on a second port (see `chan_net_bind` in config)
    #[arg(long = "chan-net", global = true)]
    pub chan_net: bool,

    #[command(subcommand)]
    /// Optional server or administration command.
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
/// Top-level operating mode.
pub enum Command {
    /// Start the web server.
    Serve,
    /// Run one administration action without starting the server.
    Admin {
        #[command(subcommand)]
        /// Administration action to execute.
        action: AdminAction,
    },
}

#[derive(Subcommand)]
/// Database-backed administration action.
pub enum AdminAction {
    /// Create an administrator account.
    CreateAdmin {
        /// Login name for the new administrator.
        username: String,
        /// Initial administrator password.
        password: String,
    },
    /// Replace an administrator password.
    ResetPassword {
        /// Existing administrator login name.
        username: String,
        /// Replacement password.
        new_password: String,
    },
    /// List administrator accounts.
    ListAdmins,
    /// Create a board.
    CreateBoard {
        /// Short URL-safe board name.
        short: String,
        /// Human-readable board name.
        name: String,
        #[arg(default_value = "")]
        /// Optional board description.
        description: String,
        #[arg(long)]
        /// Whether the board contains not-safe-for-work material.
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
    /// Delete a board and its content.
    DeleteBoard {
        /// Short name of the board to delete.
        short: String,
    },
    /// List boards.
    ListBoards,
    /// Ban an IP hash.
    Ban {
        /// Privacy-preserving IP hash to ban.
        ip_hash: String,
        /// Operator-facing ban reason.
        reason: String,
        /// Optional ban duration in hours.
        hours: Option<i64>,
    },
    /// Remove a ban.
    Unban {
        /// Database identifier of the ban to remove.
        ban_id: i64,
    },
    /// List active bans.
    ListBans,
    /// Print database schema and `SQLite` version status.
    DbStatus,
}

impl std::fmt::Debug for AdminAction {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CreateAdmin {
                username,
                password: _,
            } => formatter
                .debug_struct("CreateAdmin")
                .field("username", username)
                .field("password", &"[REDACTED]")
                .finish(),
            Self::ResetPassword {
                username,
                new_password: _,
            } => formatter
                .debug_struct("ResetPassword")
                .field("username", username)
                .field("new_password", &"[REDACTED]")
                .finish(),
            Self::ListAdmins => formatter.write_str("ListAdmins"),
            Self::CreateBoard {
                short,
                name,
                description,
                nsfw,
                no_images,
                no_videos,
                audio,
                no_audio,
            } => formatter
                .debug_struct("CreateBoard")
                .field("short", short)
                .field("name", name)
                .field("description", description)
                .field("nsfw", nsfw)
                .field("no_images", no_images)
                .field("no_videos", no_videos)
                .field("audio", audio)
                .field("no_audio", no_audio)
                .finish(),
            Self::DeleteBoard { short } => formatter
                .debug_struct("DeleteBoard")
                .field("short", short)
                .finish(),
            Self::ListBoards => formatter.write_str("ListBoards"),
            Self::Ban {
                ip_hash,
                reason,
                hours,
            } => formatter
                .debug_struct("Ban")
                .field("ip_hash", ip_hash)
                .field("reason", reason)
                .field("hours", hours)
                .finish(),
            Self::Unban { ban_id } => formatter
                .debug_struct("Unban")
                .field("ban_id", ban_id)
                .finish(),
            Self::ListBans => formatter.write_str("ListBans"),
            Self::DbStatus => formatter.write_str("DbStatus"),
        }
    }
}

// ─── Admin CLI mode ───────────────────────────────────────────────────────────

/// Write database status text to an arbitrary output stream.
fn write_db_status_output<W: std::io::Write>(
    mut writer: W,
    schema_status: &str,
    sqlite_version: &str,
) -> std::io::Result<()> {
    writeln!(writer, "Database schema: {schema_status}")?;
    writeln!(writer, "SQLite: {sqlite_version}")?;
    Ok(())
}

#[expect(
    clippy::too_many_lines,
    reason = "the command dispatch stays together so all CLI-side effects remain auditable"
)]
/// Execute an administration action directly against the database.
///
/// # Errors
///
/// Returns an error when configuration, database access, validation, terminal
/// input, or output fails.
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
        AdminAction::DbStatus => {
            let schema_status = db::database_schema_status_label(&conn);
            write_db_status_output(
                std::io::stdout().lock(),
                &schema_status,
                rusqlite::version(),
            )?;
        }
    }
    Ok(())
}

#[cfg(test)]
/// Command-line parsing and status-rendering tests.
mod tests {
    use super::{write_db_status_output, AdminAction, Cli, Command};
    use clap::Parser as _;

    #[test]
    /// Redacts plaintext passwords while retaining useful command context.
    fn admin_action_debug_redacts_plaintext_passwords() -> anyhow::Result<()> {
        const CREATE_PASSWORD: &str = "create-password-debug-sentinel";
        const RESET_PASSWORD: &str = "reset-password-debug-sentinel";

        let create_debug = format!(
            "{:?}",
            AdminAction::CreateAdmin {
                username: "alice".to_owned(),
                password: CREATE_PASSWORD.to_owned(),
            }
        );
        anyhow::ensure!(
            !create_debug.contains(CREATE_PASSWORD),
            "create-admin Debug output must not expose the password"
        );
        anyhow::ensure!(
            create_debug.contains("username: \"alice\"")
                && create_debug.contains("password: \"[REDACTED]\""),
            "create-admin Debug output should retain nonsecret context and a redaction marker"
        );

        let reset_debug = format!(
            "{:?}",
            AdminAction::ResetPassword {
                username: "bob".to_owned(),
                new_password: RESET_PASSWORD.to_owned(),
            }
        );
        anyhow::ensure!(
            !reset_debug.contains(RESET_PASSWORD),
            "reset-password Debug output must not expose the replacement password"
        );
        anyhow::ensure!(
            reset_debug.contains("username: \"bob\"")
                && reset_debug.contains("new_password: \"[REDACTED]\""),
            "reset-password Debug output should retain nonsecret context and a redaction marker"
        );
        Ok(())
    }

    #[test]
    /// Leaves audio disabled when no audio flag is supplied.
    fn create_board_audio_is_opt_in() -> anyhow::Result<()> {
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
            anyhow::bail!("arguments should parse as create-board");
        };

        anyhow::ensure!(!audio, "audio should remain disabled by default");
        anyhow::ensure!(
            !no_audio,
            "the compatibility disable flag should remain unset"
        );
        Ok(())
    }

    #[test]
    /// Enables audio when the explicit audio flag is supplied.
    fn create_board_audio_flag_enables_audio() -> anyhow::Result<()> {
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
            anyhow::bail!("arguments should parse as create-board");
        };

        anyhow::ensure!(audio, "the audio flag should enable audio uploads");
        Ok(())
    }

    #[test]
    /// Rejects conflicting audio enable and disable flags.
    fn create_board_audio_flags_conflict() -> anyhow::Result<()> {
        let Err(err) = Cli::try_parse_from([
            "rustchan-cli",
            "admin",
            "create-board",
            "tech",
            "Technology",
            "--audio",
            "--no-audio",
        ]) else {
            anyhow::bail!("conflicting audio flags should fail parsing");
        };

        anyhow::ensure!(
            err.kind() == clap::error::ErrorKind::ArgumentConflict,
            "Clap should classify conflicting audio flags as an argument conflict"
        );
        Ok(())
    }

    #[test]
    /// Exposes the database-status administration command.
    fn db_status_command_is_available() -> anyhow::Result<()> {
        let cli = Cli::parse_from(["rustchan-cli", "admin", "db-status"]);

        let Some(Command::Admin {
            action: AdminAction::DbStatus,
        }) = cli.command
        else {
            anyhow::bail!("arguments should parse as db-status");
        };
        Ok(())
    }

    #[test]
    /// Prints both the release schema label and `SQLite` version.
    fn db_status_output_uses_release_schema_version() -> anyhow::Result<()> {
        let mut out = Vec::new();

        write_db_status_output(&mut out, "1.4.0 baseline verified", "3.test")?;
        let output = String::from_utf8(out)?;
        anyhow::ensure!(
            output.contains("Database schema: 1.4.0 baseline verified"),
            "status output should include the release schema label"
        );
        anyhow::ensure!(
            output.contains("SQLite: 3.test"),
            "status output should include the SQLite version"
        );
        Ok(())
    }
}
