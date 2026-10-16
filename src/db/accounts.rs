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
