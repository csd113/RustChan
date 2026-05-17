use crate::{
    banner,
    models::{BannerAsset, BannerScope, BannerTargetType},
};
use anyhow::{Context as _, Result};
use rusqlite::{params, OptionalExtension as _};

const BANNER_SELECT_COLUMNS: &str = "ba.id, ba.scope_type, ba.board_id, b.short_name, \
    ba.storage_key, ba.width, ba.height, ba.file_size, ba.enabled, ba.sort_order, \
    ba.target_type, ba.target_value, ba.show_on_index, ba.show_on_catalog, ba.created_at";

fn map_banner_asset(row: &rusqlite::Row<'_>) -> rusqlite::Result<BannerAsset> {
    let scope_raw: String = row.get(1)?;
    let target_raw: String = row.get(10)?;
    Ok(BannerAsset {
        id: row.get(0)?,
        scope: BannerScope::from_db_str(&scope_raw).unwrap_or(BannerScope::Global),
        board_id: row.get(2)?,
        board_short: row.get(3)?,
        storage_key: row.get(4)?,
        width: row.get(5)?,
        height: row.get(6)?,
        file_size: row.get(7)?,
        enabled: row.get::<_, i32>(8)? != 0,
        sort_order: row.get(9)?,
        target_type: BannerTargetType::from_db_str(&target_raw).unwrap_or(BannerTargetType::None),
        target_value: row.get(11)?,
        show_on_index: row.get::<_, i32>(12)? != 0,
        show_on_catalog: row.get::<_, i32>(13)? != 0,
        created_at: row.get(14)?,
    })
}

/// Load a banner asset by id.
///
/// # Errors
/// Returns an error if the query fails.
pub fn get_banner_asset(
    conn: &rusqlite::Connection,
    banner_id: i64,
) -> Result<Option<BannerAsset>> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {BANNER_SELECT_COLUMNS}
         FROM banner_assets ba
         LEFT JOIN boards b ON b.id = ba.board_id
         WHERE ba.id = ?1"
    ))?;
    Ok(stmt
        .query_row(params![banner_id], map_banner_asset)
        .optional()?)
}

/// List all banner assets for a scope.
///
/// # Errors
/// Returns an error if the query fails.
pub fn list_banner_assets_for_scope(
    conn: &rusqlite::Connection,
    scope: BannerScope,
) -> Result<Vec<BannerAsset>> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {BANNER_SELECT_COLUMNS}
         FROM banner_assets ba
         LEFT JOIN boards b ON b.id = ba.board_id
         WHERE ba.scope_type = ?1
         ORDER BY ba.sort_order ASC, ba.id ASC"
    ))?;
    let assets = stmt
        .query_map(params![scope.as_str()], map_banner_asset)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(assets)
}

/// List all banner assets for a board.
///
/// # Errors
/// Returns an error if the query fails.
pub fn list_banner_assets_for_board(
    conn: &rusqlite::Connection,
    board_id: i64,
) -> Result<Vec<BannerAsset>> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {BANNER_SELECT_COLUMNS}
         FROM banner_assets ba
         LEFT JOIN boards b ON b.id = ba.board_id
         WHERE ba.scope_type = 'board' AND ba.board_id = ?1
         ORDER BY ba.sort_order ASC, ba.id ASC"
    ))?;
    let assets = stmt
        .query_map(params![board_id], map_banner_asset)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(assets)
}

/// Compute the next banner sort order for a scope.
///
/// # Errors
/// Returns an error if the query fails.
pub fn next_banner_sort_order(
    conn: &rusqlite::Connection,
    scope: BannerScope,
    board_id: Option<i64>,
) -> Result<i64> {
    let next = match scope {
        BannerScope::Board => conn.query_row(
            "SELECT COALESCE(MAX(sort_order) + 1, 1)
             FROM banner_assets
             WHERE scope_type = 'board' AND board_id = ?1",
            params![board_id],
            |row| row.get(0),
        )?,
        _ => conn.query_row(
            "SELECT COALESCE(MAX(sort_order) + 1, 1)
             FROM banner_assets
             WHERE scope_type = ?1",
            params![scope.as_str()],
            |row| row.get(0),
        )?,
    };
    Ok(next)
}

/// Insert a banner asset row.
///
/// # Errors
/// Returns an error if the storage key is invalid or the insert fails.
#[expect(clippy::too_many_arguments)]
pub fn insert_banner_asset(
    conn: &rusqlite::Connection,
    scope: BannerScope,
    board_id: Option<i64>,
    storage_key: &str,
    width: i64,
    height: i64,
    file_size: i64,
    enabled: bool,
    sort_order: i64,
    target_type: BannerTargetType,
    target_value: &str,
    show_on_index: bool,
    show_on_catalog: bool,
) -> Result<i64> {
    banner::validate_banner_storage_key(storage_key)?;
    let id = conn
        .query_row(
            "INSERT INTO banner_assets
             (scope_type, board_id, storage_key, width, height, file_size, enabled, sort_order,
              target_type, target_value, show_on_index, show_on_catalog)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
             RETURNING id",
            params![
                scope.as_str(),
                board_id,
                storage_key,
                width,
                height,
                file_size,
                i32::from(enabled),
                sort_order,
                target_type.as_str(),
                target_value,
                i32::from(show_on_index),
                i32::from(show_on_catalog),
            ],
            |row| row.get(0),
        )
        .context("Failed to insert banner asset")?;
    Ok(id)
}

/// Update the mutable metadata for a banner asset.
///
/// # Errors
/// Returns an error if the banner is missing or the update fails.
pub fn update_banner_asset_meta(
    conn: &rusqlite::Connection,
    banner_id: i64,
    enabled: bool,
    target_type: BannerTargetType,
    target_value: &str,
    show_on_index: bool,
    show_on_catalog: bool,
) -> Result<()> {
    let affected = conn.execute(
        "UPDATE banner_assets
         SET enabled = ?1,
             target_type = ?2,
             target_value = ?3,
             show_on_index = ?4,
             show_on_catalog = ?5
         WHERE id = ?6",
        params![
            i32::from(enabled),
            target_type.as_str(),
            target_value,
            i32::from(show_on_index),
            i32::from(show_on_catalog),
            banner_id,
        ],
    )?;
    if affected == 0 {
        anyhow::bail!("Banner id {banner_id} not found");
    }
    Ok(())
}

/// Delete a banner asset and return the removed row.
///
/// # Errors
/// Returns an error if the banner does not exist or the delete fails.
pub fn delete_banner_asset(conn: &rusqlite::Connection, banner_id: i64) -> Result<BannerAsset> {
    let asset = get_banner_asset(conn, banner_id)?
        .ok_or_else(|| anyhow::anyhow!("Banner id {banner_id} not found"))?;
    conn.execute(
        "DELETE FROM banner_assets WHERE id = ?1",
        params![banner_id],
    )?;
    Ok(asset)
}

/// Delete all banner assets attached to a board.
///
/// # Errors
/// Returns an error if the query fails.
pub fn delete_board_banner_assets(
    conn: &rusqlite::Connection,
    board_id: i64,
) -> Result<Vec<BannerAsset>> {
    let assets = list_banner_assets_for_board(conn, board_id)?;
    conn.execute(
        "DELETE FROM banner_assets WHERE scope_type = 'board' AND board_id = ?1",
        params![board_id],
    )?;
    Ok(assets)
}

/// Reorder banner assets within a scope.
///
/// # Errors
/// Returns an error if the banner is missing or the transaction fails.
pub fn move_banner_asset(
    conn: &mut rusqlite::Connection,
    banner_id: i64,
    move_up: bool,
) -> Result<()> {
    let tx = conn.transaction()?;
    let asset = get_banner_asset(&tx, banner_id)?
        .ok_or_else(|| anyhow::anyhow!("Banner id {banner_id} not found"))?;
    let ordered_ids = if asset.scope == BannerScope::Board {
        tx.prepare_cached(
            "SELECT id FROM banner_assets
             WHERE scope_type = 'board' AND board_id = ?1
             ORDER BY sort_order ASC, id ASC",
        )?
        .query_map(params![asset.board_id], |row| row.get::<_, i64>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?
    } else {
        tx.prepare_cached(
            "SELECT id FROM banner_assets
             WHERE scope_type = ?1
             ORDER BY sort_order ASC, id ASC",
        )?
        .query_map(params![asset.scope.as_str()], |row| row.get::<_, i64>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?
    };
    let mut ordered_ids = ordered_ids;
    let index = ordered_ids
        .iter()
        .position(|candidate| *candidate == banner_id)
        .ok_or_else(|| anyhow::anyhow!("Banner id {banner_id} not found"))?;
    let swap_with = if move_up {
        index.checked_sub(1)
    } else if index + 1 < ordered_ids.len() {
        Some(index + 1)
    } else {
        None
    };
    let Some(target_index) = swap_with else {
        tx.commit()?;
        return Ok(());
    };
    ordered_ids.swap(index, target_index);
    {
        let mut update =
            tx.prepare_cached("UPDATE banner_assets SET sort_order = ?1 WHERE id = ?2")?;
        for (position, id) in ordered_ids.iter().enumerate() {
            let sort_order =
                i64::try_from(position).context("banner sort_order must fit in i64")? + 1;
            update.execute(params![sort_order, id])?;
        }
    }
    tx.commit()?;
    Ok(())
}

pub fn get_banner_external_links_enabled(conn: &rusqlite::Connection) -> bool {
    super::get_site_setting(conn, "banner_external_links_enabled")
        .unwrap_or_else(|error| {
            tracing::warn!(
                target: "db",
                %error,
                "Failed to read banner_external_links_enabled setting"
            );
            None
        })
        .is_some_and(|value| value == "1" || value.eq_ignore_ascii_case("true"))
}

pub fn get_banner_rotation_interval_minutes(conn: &rusqlite::Connection) -> i64 {
    super::get_site_setting(conn, "banner_rotation_interval_minutes")
        .unwrap_or_else(|error| {
            tracing::warn!(
                target: "db",
                %error,
                "Failed to read banner_rotation_interval_minutes setting"
            );
            None
        })
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(0)
        .clamp(0, 43_200)
}
