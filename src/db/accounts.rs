//! Transactional administrator account management with credential reauthentication.

use anyhow::{ensure, Context as _};
use rusqlite::Connection;

/// Account workflows; password changes also revoke the target's sessions.
#[derive(Debug, Clone, Copy, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountAction {
    /// Create a full-privilege administrator.
    Create,
    /// Change/reset an existing administrator password.
    Password,
}

/// Validate and commit an account change after authenticating the current session owner.
/// A false result means reauthentication failed and no mutation occurred.
///
/// # Errors
/// Rejects expired authorization, malformed input and database/cryptography failures.
pub fn manage_account(
    conn: &mut Connection,
    session: &str,
    action: AccountAction,
    username: &str,
    current_password: &str,
    new_password: &str,
) -> anyhow::Result<bool> {
    ensure!(
        username.len() <= 64 && !username.is_empty(),
        "username must be 1–64 bytes"
    );
    if matches!(action, AccountAction::Create) {
        ensure!(
            username
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'.')),
            "new usernames may contain letters, digits, dot, underscore and hyphen"
        );
    }
    ensure!(
        current_password.len() <= 1024 && new_password.len() <= 1024,
        "password is too long"
    );
    crate::utils::crypto::validate_password(new_password)?;
    let authorized = super::get_session(conn, session)?.context("administrator session expired")?;
    let actor =
        super::get_admin_name_by_id(conn, authorized.admin_id)?.context("administrator missing")?;
    let user = super::get_admin_by_username(conn, &actor)?.context("administrator missing")?;
    if !crate::utils::crypto::verify_password(current_password, &user.password_hash)? {
        return Ok(false);
    }
    // Password work runs before taking SQLite's single writer lock. Recheck
    // session and credentials under that lock to close the authorization race.
    let hash = crate::utils::crypto::hash_password(new_password)?;
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let current_session =
        super::get_session(&tx, session)?.context("administrator session expired")?;
    let current_user =
        super::get_admin_by_username(&tx, &actor)?.context("administrator missing")?;
    ensure!(
        current_session.admin_id == authorized.admin_id
            && current_user.password_hash == user.password_hash,
        "administrator authorization changed; retry after signing in again"
    );
    match action {
        AccountAction::Create => {
            ensure!(
                super::get_admin_by_username(&tx, username)?.is_none(),
                "username already exists"
            );
            super::create_admin(&tx, username, &hash).map(|_created_id| ())?;
        }
        AccountAction::Password => {
            let target =
                super::get_admin_by_username(&tx, username)?.context("administrator not found")?;
            super::update_admin_password(&tx, username, &hash)?;
            tx.execute(
                "DELETE FROM admin_sessions WHERE admin_id = ?1",
                [target.id],
            )
            .map(|_affected_rows| ())?;
        }
    }
    tx.commit()?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_changes_reauthenticate_and_revoke_only_target_sessions_atomically(
    ) -> anyhow::Result<()> {
        let mut conn = Connection::open_in_memory()?;
        conn.execute_batch("CREATE TABLE admin_users(id INTEGER PRIMARY KEY, username TEXT UNIQUE, password_hash TEXT, created_at INTEGER DEFAULT 0); CREATE TABLE admin_sessions(id TEXT PRIMARY KEY, admin_id INTEGER, created_at INTEGER DEFAULT 0, expires_at INTEGER);")?;
        let hash = crate::utils::crypto::hash_password("CurrentPass123")?;
        let actor = super::super::create_admin(&conn, "actor", &hash)?;
        let target = super::super::create_admin(&conn, "target", &hash)?;
        let expiry = chrono::Utc::now().timestamp() + 3600;
        super::super::create_session(&conn, "actor-session", actor, expiry)?;
        super::super::create_session(&conn, "target-session", target, expiry)?;
        ensure!(
            !manage_account(
                &mut conn,
                "actor-session",
                AccountAction::Password,
                "target",
                "incorrect",
                "Replacement123"
            )?,
            "wrong credentials must fail"
        );
        ensure!(
            super::super::get_session(&conn, "target-session")?.is_some(),
            "failed reset must preserve sessions"
        );
        ensure!(
            manage_account(
                &mut conn,
                "actor-session",
                AccountAction::Password,
                "target",
                "CurrentPass123",
                "Replacement123"
            )?,
            "valid reset should succeed"
        );
        ensure!(
            super::super::get_session(&conn, "target-session")?.is_none(),
            "reset must revoke target sessions"
        );
        ensure!(
            super::super::get_session(&conn, "actor-session")?.is_some(),
            "other sessions must survive"
        );
        let updated_target =
            super::super::get_admin_by_username(&conn, "target")?.context("target")?;
        ensure!(
            crate::utils::crypto::verify_password("Replacement123", &updated_target.password_hash)?,
            "stored password must match"
        );
        ensure!(
            manage_account(
                &mut conn,
                "expired-session",
                AccountAction::Create,
                "new",
                "CurrentPass123",
                "Replacement123"
            )
            .is_err(),
            "expired authorization must fail"
        );
        ensure!(
            super::super::get_admin_by_username(&conn, "new")?.is_none(),
            "no unauthorized account creation"
        );
        Ok(())
    }
}
