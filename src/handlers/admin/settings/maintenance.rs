use super::{
    admin_panel_error_redirect_anchor, admin_panel_redirect_anchor, checkbox_is_on, db, header,
    require_admin_post_origin_and_csrf, require_admin_session_sid, AppError, AppState, CookieJar,
    Form, HeaderMap, HeaderValue, Html, Query, Response, Result, State, CONFIG, SESSION_COOKIE,
};
use axum::response::IntoResponse as _;
use serde::Deserialize;
use std::sync::atomic::Ordering;

#[cfg(test)]
static PRE_REPAIR_BACKUP_FAILURE: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

#[derive(Deserialize)]
pub(crate) struct VacuumForm {
    #[serde(rename = "_csrf")]
    pub csrf: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct DbMaintenanceForm {
    #[serde(rename = "_csrf")]
    pub csrf: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct MediaSettingsForm {
    #[serde(rename = "_csrf")]
    pub csrf: Option<String>,
    pub ffmpeg_timeout_secs: Option<String>,
    pub media_auto_prune_enabled: Option<String>,
    pub media_max_active_content_size: Option<String>,
    pub media_max_active_content_size_unit: Option<String>,
}

#[derive(Deserialize, Default)]
pub(crate) struct DbRepairStatusQuery {
    pub job_id: Option<u64>,
}

fn create_pre_repair_backup(
    pool: &db::DbPool,
    progress: &std::sync::Arc<crate::middleware::BackupProgress>,
    job_id: u64,
) -> Result<String> {
    #[cfg(test)]
    {
        let backup_failure = PRE_REPAIR_BACKUP_FAILURE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if let Some(message) = backup_failure {
            return Err(AppError::Internal(anyhow::anyhow!(message)));
        }
    }

    crate::handlers::admin::create_pre_maintenance_backup_to_server(
        pool,
        progress,
        "db_repair",
        job_id,
        "Automatic DB + config safety backup before database repair.",
    )
}

fn parse_ffmpeg_timeout_secs_input(input: Option<&str>) -> Result<u64> {
    let raw = input
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| AppError::BadRequest("Video re-encoding timeout is required.".into()))?;
    let timeout_secs = raw.parse::<u64>().map_err(|_error| {
        AppError::BadRequest("Video re-encoding timeout must be a whole number of seconds.".into())
    })?;
    crate::config::validate_ffmpeg_timeout_secs(timeout_secs)
        .map_err(|error| AppError::BadRequest(error.to_string()))
}

fn parse_media_prune_size_input(
    enabled: bool,
    value: Option<&str>,
    unit: Option<&str>,
) -> Result<u64> {
    const MIB: u64 = 1024 * 1024;
    const GIB: u64 = 1024 * MIB;
    const MIN_ENABLED_BYTES: u64 = MIB;

    let raw = value.map_or("", str::trim);
    if raw.is_empty() {
        if enabled {
            return Err(AppError::BadRequest(
                "Maximum active content size is required when pruning is enabled.".into(),
            ));
        }
        return Ok(0);
    }
    if raw.starts_with('-') {
        return Err(AppError::BadRequest(
            "Maximum active content size cannot be negative.".into(),
        ));
    }
    let amount = raw.parse::<u64>().map_err(|_error| {
        AppError::BadRequest("Maximum active content size must be a whole number.".into())
    })?;
    let multiplier = match unit.unwrap_or("mib") {
        "bytes" => 1,
        "mib" => MIB,
        "gib" => GIB,
        _ => {
            return Err(AppError::BadRequest(
                "Maximum active content size unit is invalid.".into(),
            ));
        }
    };
    let bytes = amount
        .checked_mul(multiplier)
        .ok_or_else(|| AppError::BadRequest("Maximum active content size is too large.".into()))?;
    if enabled && bytes < MIN_ENABLED_BYTES {
        return Err(AppError::BadRequest(
            "Maximum active content size must be at least 1 MiB when pruning is enabled.".into(),
        ));
    }
    Ok(bytes)
}

pub(crate) async fn update_media_settings(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: HeaderMap,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    Form(form): Form<MediaSettingsForm>,
) -> Result<Response> {
    let session_id = jar.get(SESSION_COOKIE).map(|c| c.value().to_owned());
    require_admin_post_origin_and_csrf(&jar, &headers, Some(peer), form.csrf.as_deref())?;

    let timeout_secs = match parse_ffmpeg_timeout_secs_input(form.ffmpeg_timeout_secs.as_deref()) {
        Ok(timeout_secs) => timeout_secs,
        Err(AppError::BadRequest(message)) => {
            return Ok(admin_panel_error_redirect_anchor(&message, "maintenance").into_response());
        }
        Err(error) => return Err(error),
    };
    let prune_enabled = checkbox_is_on(form.media_auto_prune_enabled.as_deref());
    let prune_max_bytes = match parse_media_prune_size_input(
        prune_enabled,
        form.media_max_active_content_size.as_deref(),
        form.media_max_active_content_size_unit.as_deref(),
    ) {
        Ok(bytes) => bytes,
        Err(AppError::BadRequest(message)) => {
            return Ok(admin_panel_error_redirect_anchor(&message, "maintenance").into_response());
        }
        Err(error) => return Err(error),
    };

    tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<()> {
            let conn = pool.get()?;
            require_admin_session_sid(&conn, session_id.as_deref())?;
            crate::config::set_live_ffmpeg_timeout_secs(timeout_secs)?;
            db::set_media_prune_settings(&conn, prune_enabled, prune_max_bytes)?;
            crate::config::update_settings_file_ffmpeg_timeout(timeout_secs);
            crate::config::update_settings_file_media_pruning(prune_enabled, prune_max_bytes);
            let prune_report =
                crate::media::prune::run_configured_prune(&conn, &CONFIG.upload_dir)?;
            tracing::info!(
                target: "admin",
                ffmpeg_timeout_secs = timeout_secs,
                media_auto_prune_enabled = prune_enabled,
                media_max_active_content_size_bytes = prune_max_bytes,
                pruned_files = prune_report.removed_files,
                "FFmpeg media timeout updated"
            );
            Ok(())
        }
    })
    .await
    .map_err(|error| AppError::Internal(anyhow::anyhow!(error)))??;

    Ok(
        admin_panel_redirect_anchor("Media processing settings saved.", "maintenance")
            .into_response(),
    )
}

pub(crate) async fn admin_vacuum(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: HeaderMap,
    secure_context: crate::middleware::SecureCookieContext,
    Form(form): Form<VacuumForm>,
) -> Result<Response> {
    let current_theme = crate::handlers::board::current_theme_from_jar(&jar);
    let session_id = jar.get(SESSION_COOKIE).map(|c| c.value().to_owned());
    require_admin_post_origin_and_csrf(&jar, &headers, secure_context.peer, form.csrf.as_deref())?;

    let (jar, csrf) = super::super::ensure_admin_csrf(
        jar,
        super::super::should_set_secure_cookie(&headers, secure_context),
    )?;
    let _maintenance_guard = state.maintenance_gate.try_begin("Database VACUUM")?;

    let html = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        let csrf_clone = csrf.clone();
        move || -> Result<String> {
            let conn = pool.get()?;
            require_admin_session_sid(&conn, session_id.as_deref())?;

            let size_before = db::get_db_size_bytes(&conn).unwrap_or(0);

            db::run_vacuum(&conn)?;

            let size_after = db::get_db_size_bytes(&conn).unwrap_or(0);

            let saved = size_before.saturating_sub(size_after);

            tracing::info!(
                target: "admin",
                before_bytes = size_before,
                after_bytes  = size_after,
                saved_bytes  = saved,
                "Admin ran VACUUM"
            );

            Ok(crate::templates::admin_vacuum_result_page(
                size_before,
                size_after,
                &csrf_clone,
                current_theme.as_deref(),
            ))
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    Ok((jar, Html(html)).into_response())
}

pub(crate) async fn admin_db_check(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: HeaderMap,
    secure_context: crate::middleware::SecureCookieContext,
    Form(form): Form<DbMaintenanceForm>,
) -> Result<Response> {
    let current_theme = crate::handlers::board::current_theme_from_jar(&jar);
    let session_id = jar.get(SESSION_COOKIE).map(|c| c.value().to_owned());
    require_admin_post_origin_and_csrf(&jar, &headers, secure_context.peer, form.csrf.as_deref())?;

    let (jar, csrf) = super::super::ensure_admin_csrf(
        jar,
        super::super::should_set_secure_cookie(&headers, secure_context),
    )?;
    let html = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        let csrf_clone = csrf.clone();
        move || -> Result<String> {
            let conn = pool.get()?;
            require_admin_session_sid(&conn, session_id.as_deref())?;

            let report = db::check_db_health(&conn);
            tracing::info!(
                target: "admin",
                ok = report.before.ok(),
                integrity = report.before.integrity.output(),
                foreign_keys = report.before.foreign_keys.output(),
                "Admin ran database health check"
            );

            Ok(crate::templates::admin_db_health_result_page(
                &report,
                false,
                &csrf_clone,
                None,
                current_theme.as_deref(),
            ))
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    Ok((jar, Html(html)).into_response())
}

pub(crate) async fn admin_db_repair(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: HeaderMap,
    secure_context: crate::middleware::SecureCookieContext,
    Form(form): Form<DbMaintenanceForm>,
) -> Result<Response> {
    let session_id = jar.get(SESSION_COOKIE).map(|c| c.value().to_owned());
    require_admin_post_origin_and_csrf(&jar, &headers, secure_context.peer, form.csrf.as_deref())?;

    let (jar, csrf) = super::super::ensure_admin_csrf(
        jar,
        super::super::should_set_secure_cookie(&headers, secure_context),
    )?;
    tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<()> {
            let conn = pool.get()?;
            require_admin_session_sid(&conn, session_id.as_deref())?;
            Ok(())
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    let maintenance_guard = state
        .maintenance_gate
        .try_begin("Database maintenance rebuild")?;
    let job_id = state.db_maintenance_jobs.mark_running();
    let progress = std::sync::Arc::clone(&state.backup_progress);
    let pool = state.db.clone();
    let db_maintenance_jobs = state.db_maintenance_jobs.clone();

    tokio::spawn(async move {
        let job_status = db_maintenance_jobs.clone();
        let join_result = tokio::task::spawn_blocking(move || {
            let _maintenance_guard = maintenance_guard;
            let _ = db_maintenance_jobs
                .mark_phase(job_id, crate::middleware::DbMaintenanceJobPhase::Backup);
            let backup_result = create_pre_repair_backup(&pool, &progress, job_id);

            let conn = match pool.get() {
                Ok(conn) => conn,
                Err(error) => {
                    let _ = db_maintenance_jobs.mark_failed(
                        job_id,
                        format!(
                            "Could not open database connection after pre-repair backup: {error}"
                        ),
                    );
                    return;
                }
            };

            let _ = db_maintenance_jobs
                .mark_phase(job_id, crate::middleware::DbMaintenanceJobPhase::Repair);
            let report = match backup_result {
                Ok(backup_id) => db::attempt_db_repair(
                    &conn,
                    Some(db::DbRepairBackup {
                        backup_id: backup_id.clone(),
                        backup_type: "DB + config".to_owned(),
                        backup_path: crate::config::backups_dir()
                            .join(&backup_id)
                            .display()
                            .to_string(),
                        verified: true,
                    }),
                ),
                Err(error) => {
                    let backup_error = error.to_string();
                    db::db_repair_aborted_for_backup_failure(&conn, &backup_error)
                }
            };
            tracing::info!(
                target: "admin",
                before_ok = report.before.ok(),
                after_ok = report.after.as_ref().map(db::DbHealthSnapshot::ok),
                steps = report.repair_steps.len(),
                backup = report
                    .repair_backup
                    .as_ref()
                    .map(|backup| backup.backup_id.as_str()),
                backup_error = report.repair_backup_error.as_deref(),
                "Admin ran database repair attempt"
            );
            let _ = db_maintenance_jobs.mark_finished(job_id, report);
        })
        .await;

        if let Err(error) = join_result {
            let _ = job_status.mark_failed(job_id, format!("Database repair task failed: {error}"));
        }
    });

    Ok(render_db_repair_entry_response(
        &jar,
        &csrf,
        &state.db_maintenance_jobs.snapshot(),
    ))
}

pub(crate) async fn admin_db_repair_progress_json(
    State(state): State<AppState>,
    jar: CookieJar,
    Query(query): Query<DbRepairStatusQuery>,
) -> Result<Response> {
    let session_id = jar.get(SESSION_COOKIE).map(|c| c.value().to_owned());

    tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<()> {
            let conn = pool.get()?;
            require_admin_session_sid(&conn, session_id.as_deref())?;
            Ok(())
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    let payload = db_repair_progress_payload(
        state.db_maintenance_jobs.snapshot(),
        state.backup_progress.as_ref(),
        query.job_id,
    );
    Ok((
        [(header::CONTENT_TYPE, "application/json".to_owned())],
        payload.to_string(),
    )
        .into_response())
}

fn db_repair_progress_payload(
    status: crate::middleware::DbMaintenanceJobStatus,
    backup_progress: &crate::middleware::BackupProgress,
    requested_job_id: Option<u64>,
) -> serde_json::Value {
    let current_job_id = status.job_id();
    if requested_job_id.is_some_and(|job_id| Some(job_id) != current_job_id) {
        let redirect_url = db_repair_status_url(current_job_id);
        let label = if current_job_id.is_some() {
            "A newer maintenance rebuild is active. Opening current status..."
        } else {
            "This maintenance rebuild is no longer active. Opening repair page..."
        };
        return serde_json::json!({
            "state": "stale",
            "job_id": current_job_id,
            "label": label,
            "percent": 100,
            "done": true,
            "redirect_url": redirect_url,
        });
    }

    let backup_phase = backup_progress.phase.load(Ordering::Relaxed);
    let backup_files_done = backup_progress.files_done.load(Ordering::Relaxed);
    let backup_files_total = backup_progress.files_total.load(Ordering::Relaxed);

    match status {
        crate::middleware::DbMaintenanceJobStatus::Idle => serde_json::json!({
            "state": "idle",
            "label": "No maintenance rebuild is running.",
            "percent": 0,
            "done": false,
        }),
        crate::middleware::DbMaintenanceJobStatus::Running {
            job_id,
            phase: crate::middleware::DbMaintenanceJobPhase::Starting,
            ..
        } => serde_json::json!({
            "state": "running",
            "job_id": job_id,
            "label": "Starting maintenance rebuild...",
            "percent": 5,
            "done": false,
        }),
        crate::middleware::DbMaintenanceJobStatus::Running {
            job_id,
            phase: crate::middleware::DbMaintenanceJobPhase::Backup,
            ..
        } => {
            let backup_percent =
                backup_percent(backup_phase, backup_files_done, backup_files_total);
            let label = backup_progress_label(backup_phase, backup_files_done, backup_files_total);
            serde_json::json!({
                "state": "running",
                "job_id": job_id,
                "label": label,
                "percent": backup_percent,
                "done": false,
            })
        }
        crate::middleware::DbMaintenanceJobStatus::Running {
            job_id,
            phase: crate::middleware::DbMaintenanceJobPhase::Repair,
            ..
        } => serde_json::json!({
            "state": "running",
            "job_id": job_id,
            "label": "Rebuilding indexes and checking database health...",
            "percent": 82,
            "done": false,
        }),
        crate::middleware::DbMaintenanceJobStatus::Finished { job_id, .. } => serde_json::json!({
            "state": "finished",
            "job_id": job_id,
            "label": "Maintenance rebuild complete. Opening report...",
            "percent": 100,
            "done": true,
            "redirect_url": db_repair_status_url(Some(job_id)),
        }),
        crate::middleware::DbMaintenanceJobStatus::Failed {
            job_id, message, ..
        } => serde_json::json!({
            "state": "failed",
            "job_id": job_id,
            "label": message,
            "percent": 100,
            "done": true,
            "redirect_url": db_repair_status_url(Some(job_id)),
        }),
    }
}

fn backup_percent(phase: u64, files_done: u64, files_total: u64) -> u64 {
    match phase {
        crate::middleware::backup_phase::SNAPSHOT_DB => 35,
        crate::middleware::backup_phase::COUNT_FILES => 45,
        crate::middleware::backup_phase::COMPRESS => files_done
            .saturating_mul(30)
            .checked_div(files_total)
            .map_or(55, |percent| 55 + percent),
        crate::middleware::backup_phase::DONE => 78,
        _ => 10,
    }
}

fn backup_progress_label(phase: u64, files_done: u64, files_total: u64) -> String {
    match phase {
        crate::middleware::backup_phase::SNAPSHOT_DB => {
            "Creating Backup v4 DB snapshot...".to_owned()
        }
        crate::middleware::backup_phase::COUNT_FILES => {
            "Writing Backup v4 maintenance metadata...".to_owned()
        }
        crate::middleware::backup_phase::COMPRESS => {
            if files_total == 0 {
                "Copying files into the Backup v4 folder...".to_owned()
            } else {
                format!(
                    "Copying files into the Backup v4 folder... {files_done}/{files_total} files"
                )
            }
        }
        crate::middleware::backup_phase::DONE => {
            "Backup v4 pre-maintenance backup complete...".to_owned()
        }
        _ => "Preparing Backup v4 pre-maintenance backup...".to_owned(),
    }
}

pub(crate) async fn admin_db_repair_status(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: HeaderMap,
    secure_context: crate::middleware::SecureCookieContext,
    Query(query): Query<DbRepairStatusQuery>,
) -> Result<Response> {
    let session_id = jar.get(SESSION_COOKIE).map(|c| c.value().to_owned());

    let (jar, csrf) = super::super::ensure_admin_csrf(
        jar,
        super::super::should_set_secure_cookie(&headers, secure_context),
    )?;
    tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<()> {
            let conn = pool.get()?;
            require_admin_session_sid(&conn, session_id.as_deref())?;
            Ok(())
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    Ok(render_db_repair_status_response(
        &jar,
        &csrf,
        &state.db_maintenance_jobs.snapshot(),
        query.job_id,
    ))
}

fn db_repair_status_url(job_id: Option<u64>) -> String {
    match job_id {
        Some(job_id) => format!("/admin/db/repair/status?job_id={job_id}"),
        None => "/admin/db/repair".to_owned(),
    }
}

fn render_db_repair_running_response(
    jar: &CookieJar,
    csrf: &str,
    job_id: u64,
    started_at: i64,
) -> Response {
    let current_theme = crate::handlers::board::current_theme_from_jar(jar);
    let refresh = format!("10; url={}", db_repair_status_url(Some(job_id)));
    let mut response = (
        jar.clone(),
        Html(crate::templates::admin_db_repair_running_page(
            csrf,
            job_id,
            started_at,
            current_theme.as_deref(),
        )),
    )
        .into_response();
    if let Ok(refresh) = HeaderValue::from_str(&refresh) {
        response.headers_mut().insert(header::REFRESH, refresh);
    }
    response
}

fn render_db_repair_entry_response(
    jar: &CookieJar,
    csrf: &str,
    status: &crate::middleware::DbMaintenanceJobStatus,
) -> Response {
    let current_theme = crate::handlers::board::current_theme_from_jar(jar);
    match status {
        crate::middleware::DbMaintenanceJobStatus::Running {
            job_id, started_at, ..
        } => render_db_repair_running_response(jar, csrf, *job_id, *started_at),
        crate::middleware::DbMaintenanceJobStatus::Idle
        | crate::middleware::DbMaintenanceJobStatus::Finished { .. }
        | crate::middleware::DbMaintenanceJobStatus::Failed { .. } => (
            jar.clone(),
            Html(crate::templates::admin_db_repair_idle_page(
                csrf,
                current_theme.as_deref(),
            )),
        )
            .into_response(),
    }
}

fn render_db_repair_status_response(
    jar: &CookieJar,
    csrf: &str,
    status: &crate::middleware::DbMaintenanceJobStatus,
    requested_job_id: Option<u64>,
) -> Response {
    let current_theme = crate::handlers::board::current_theme_from_jar(jar);
    if let Some(requested_job_id) = requested_job_id {
        if Some(requested_job_id) != status.job_id() {
            return (
                jar.clone(),
                Html(crate::templates::admin_db_repair_stale_page(
                    csrf,
                    requested_job_id,
                    status.job_id(),
                    current_theme.as_deref(),
                )),
            )
                .into_response();
        }
    }

    match status {
        crate::middleware::DbMaintenanceJobStatus::Idle => (
            jar.clone(),
            Html(crate::templates::admin_db_repair_idle_page(
                csrf,
                current_theme.as_deref(),
            )),
        )
            .into_response(),
        crate::middleware::DbMaintenanceJobStatus::Running {
            job_id, started_at, ..
        } => render_db_repair_running_response(jar, csrf, *job_id, *started_at),
        crate::middleware::DbMaintenanceJobStatus::Finished { job_id, report } => (
            jar.clone(),
            Html(crate::templates::admin_db_health_result_page(
                report,
                true,
                csrf,
                Some(*job_id),
                current_theme.as_deref(),
            )),
        )
            .into_response(),
        crate::middleware::DbMaintenanceJobStatus::Failed {
            job_id,
            finished_at,
            message,
        } => (
            jar.clone(),
            Html(crate::templates::admin_db_repair_failed_page(
                csrf,
                message,
                *finished_at,
                *job_id,
                current_theme.as_deref(),
            )),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        admin_db_repair, admin_db_repair_status, admin_vacuum, parse_ffmpeg_timeout_secs_input,
        parse_media_prune_size_input, update_media_settings, PRE_REPAIR_BACKUP_FAILURE,
    };
    use anyhow::{bail, ensure, Context as _};
    use axum::{
        body::{to_bytes, Body},
        http::{header, Request, StatusCode},
        response::Response,
        routing::{get, post},
        Router,
    };
    use tower::ServiceExt as _;

    static REPAIR_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    static MEDIA_SETTINGS_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    fn install_admin_session(state: &crate::middleware::AppState) -> anyhow::Result<()> {
        let conn = state.db.get().context("get database connection")?;
        let password_hash =
            crate::utils::crypto::hash_password("hunter2").context("hash admin password")?;
        let admin_id =
            crate::db::create_admin(&conn, "admin", &password_hash).context("create admin")?;
        crate::db::create_session(
            &conn,
            "session123",
            admin_id,
            chrono::Utc::now().timestamp() + 3600,
        )
        .context("create admin session")?;
        Ok(())
    }

    fn admin_signed_csrf() -> String {
        crate::utils::crypto::make_scoped_csrf_form_token(
            "csrf123",
            &crate::config::CONFIG.cookie_secret,
            "session123",
        )
    }

    fn posts_ai_trigger_sql(state: &crate::middleware::AppState) -> anyhow::Result<String> {
        let conn = state.db.get().context("get database connection")?;
        conn.query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'trigger' AND name = 'posts_ai'",
            [],
            |row| row.get(0),
        )
        .context("load posts_ai trigger SQL")
    }

    fn create_controlled_integrity_problem(
        state: &crate::middleware::AppState,
    ) -> anyhow::Result<()> {
        let unique = uuid::Uuid::new_v4().simple().to_string();
        let suffix = unique.get(..7).unwrap_or(&unique);
        let board_short = format!("f{suffix}");
        let conn = state.db.get().context("get database connection")?;
        let board_id = crate::db::create_board(&conn, &board_short, "Repair Test", "", false)
            .context("create repair-test board")?;
        let post = crate::db::NewPost {
            thread_id: 0,
            board_id,
            name: "anon".to_owned(),
            tripcode: None,
            subject: Some("repair thread".to_owned()),
            body: "repair test body".to_owned(),
            body_html: "repair test body".to_owned(),
            ip_hash: None,
            file_path: None,
            file_name: None,
            file_size: None,
            thumb_path: None,
            mime_type: None,
            media_type: None,
            audio_file_path: None,
            audio_file_name: None,
            audio_file_size: None,
            audio_mime_type: None,
            deletion_token: "repair-token".to_owned(),
            is_op: true,
        };
        crate::db::create_thread_with_optional_poll(
            &conn,
            board_id,
            Some("repair thread"),
            &post,
            "",
            None,
            None,
        )
        .context("create repair-test thread")?;

        conn.execute_batch(&format!(
            "PRAGMA foreign_keys=OFF; BEGIN; DELETE FROM boards WHERE short_name='{board_short}'; COMMIT; PRAGMA foreign_keys=ON;"
        ))
        .context("create controlled integrity problem")?;
        Ok(())
    }

    fn repair_router(state: crate::middleware::AppState) -> Router {
        Router::new()
            .route(
                "/admin/db/repair",
                get(admin_db_repair_status).post(admin_db_repair),
            )
            .route("/admin/db/repair/status", get(admin_db_repair_status))
            .route(
                "/admin/db/repair/progress",
                get(super::admin_db_repair_progress_json),
            )
            .with_state(state)
    }

    fn vacuum_router(state: crate::middleware::AppState) -> Router {
        Router::new()
            .route("/admin/vacuum", post(admin_vacuum))
            .with_state(state)
    }

    fn media_settings_router(state: crate::middleware::AppState) -> Router {
        Router::new()
            .route("/admin/media/settings", post(update_media_settings))
            .with_state(state)
    }

    async fn admin_get(router: &Router, uri: &str) -> anyhow::Result<Response> {
        router
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(uri)
                    .header(
                        header::COOKIE,
                        "csrf_token=csrf123; chan_admin_session=session123",
                    )
                    .body(Body::empty())
                    .context("build admin GET request")?,
            )
            .await
            .context("receive admin GET response")
    }

    async fn response_body(response: Response) -> anyhow::Result<String> {
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .context("read response body")?;
        String::from_utf8(body.to_vec()).context("decode UTF-8 response body")
    }

    async fn repair_status_body(router: &Router) -> anyhow::Result<String> {
        let response = admin_get(router, "/admin/db/repair/status").await?;
        ensure!(response.status() == StatusCode::OK);
        response_body(response).await
    }

    async fn wait_for_repair_result(router: &Router) -> anyhow::Result<String> {
        for _ in 0..100 {
            let body = repair_status_body(router).await?;
            if !body.contains("maintenance rebuild running") {
                return Ok(body);
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        bail!("database repair did not finish")
    }

    #[test]
    fn parse_ffmpeg_timeout_secs_rejects_blank_or_out_of_range_values() -> anyhow::Result<()> {
        ensure!(parse_ffmpeg_timeout_secs_input(None).is_err());
        ensure!(parse_ffmpeg_timeout_secs_input(Some("")).is_err());
        ensure!(parse_ffmpeg_timeout_secs_input(Some("29")).is_err());
        ensure!(parse_ffmpeg_timeout_secs_input(Some("86401")).is_err());
        ensure!(parse_ffmpeg_timeout_secs_input(Some("600"))? == 600);
        Ok(())
    }

    #[test]
    fn parse_media_prune_size_rejects_invalid_or_negative_values() -> anyhow::Result<()> {
        ensure!(parse_media_prune_size_input(true, Some("-1"), Some("mib")).is_err());
        ensure!(parse_media_prune_size_input(true, Some("abc"), Some("mib")).is_err());
        ensure!(parse_media_prune_size_input(true, Some("0"), Some("mib")).is_err());
        ensure!(parse_media_prune_size_input(true, Some("1024"), Some("bytes")).is_err());
        ensure!(
            parse_media_prune_size_input(true, Some("2"), Some("gib"))? == 2 * 1024 * 1024 * 1024
        );
        ensure!(parse_media_prune_size_input(false, Some("0"), Some("mib"))? == 0);
        Ok(())
    }

    #[tokio::test]
    async fn media_settings_save_updates_live_timeout_and_settings_file() -> anyhow::Result<()> {
        let _guard = MEDIA_SETTINGS_TEST_LOCK.lock().await;
        let state = crate::test_support::app_state();
        install_admin_session(&state)?;

        let settings_path = crate::config::data_dir().join("settings.toml");
        let previous_settings = std::fs::read_to_string(&settings_path).ok();
        let previous_timeout = crate::config::ffmpeg_timeout_secs();
        let parent = settings_path
            .parent()
            .context("settings path has no parent")?;
        std::fs::create_dir_all(parent).context("create settings parent")?;
        std::fs::write(
            &settings_path,
            format!(
                "forum_name = \"RustChan\"\nffmpeg_timeout_secs = {previous_timeout}\nmedia_auto_prune_enabled = false\nmedia_max_active_content_size_bytes = 0\n"
            ),
        )
        .context("write settings fixture")?;

        let router = media_settings_router(state.clone());
        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/admin/media/settings")
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .header(header::HOST, "localhost")
                    .header(header::ORIGIN, "http://localhost")
                    .header(
                        header::COOKIE,
                        "csrf_token=csrf123; chan_admin_session=session123",
                    )
                    .extension(crate::test_support::connect_info())
                    .body(Body::from(format!(
                        "_csrf={}&ffmpeg_timeout_secs=1800&media_auto_prune_enabled=1&media_max_active_content_size=2&media_max_active_content_size_unit=mib",
                        admin_signed_csrf()
                    )))
                    .context("build media-settings request")?,
            )
            .await
            .context("receive media-settings response")?;

        ensure!(response.status() == StatusCode::SEE_OTHER);
        let location = response
            .headers()
            .get(header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .context("response omitted location header")?;
        ensure!(location.contains("flash="));
        ensure!(location.contains("Media"));
        ensure!(location.contains("processing"));
        ensure!(location.contains("#maintenance"));

        ensure!(crate::config::ffmpeg_timeout_secs() == 1_800);
        let updated_settings = std::fs::read_to_string(&settings_path).context("read settings")?;
        ensure!(updated_settings.contains("ffmpeg_timeout_secs = 1800\n"));
        ensure!(updated_settings.contains("media_auto_prune_enabled = true\n"));
        ensure!(updated_settings.contains("media_max_active_content_size_bytes = 2097152\n"));
        let conn = state.db.get().context("get settings database connection")?;
        ensure!(crate::db::get_media_auto_prune_enabled(&conn));
        ensure!(crate::db::get_media_max_active_content_size_bytes(&conn) == 2_097_152);
        let reloaded = crate::config::Config::from_env();
        ensure!(reloaded.ffmpeg_timeout_secs == 1_800);
        ensure!(reloaded.initial_media_auto_prune_enabled);
        ensure!(reloaded.initial_media_max_active_content_size_bytes == 2_097_152);
        drop(conn);

        crate::config::set_live_ffmpeg_timeout_secs(previous_timeout).context("restore timeout")?;
        match previous_settings {
            Some(contents) => {
                std::fs::write(&settings_path, contents).context("restore settings")?;
            }
            None => {
                drop(std::fs::remove_file(&settings_path));
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn admin_db_repair_aborts_before_mutation_when_pre_repair_backup_fails(
    ) -> anyhow::Result<()> {
        let _guard = REPAIR_TEST_LOCK.lock().await;
        let state = crate::test_support::app_state();
        install_admin_session(&state)?;

        let sentinel_trigger_sql =
            "CREATE TRIGGER posts_ai AFTER INSERT ON posts BEGIN SELECT RAISE(IGNORE); END";
        {
            let conn = state.db.get().context("get database connection")?;
            conn.execute_batch(&format!("DROP TRIGGER posts_ai; {sentinel_trigger_sql};"))
                .context("install sentinel trigger")?;
        }

        {
            let mut failure = PRE_REPAIR_BACKUP_FAILURE
                .lock()
                .map_err(|_| anyhow::anyhow!("backup failure mutex was poisoned"))?;
            *failure = Some("simulated pre-repair backup failure".to_owned());
        }

        let router = repair_router(state.clone());
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/admin/db/repair")
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .header(header::HOST, "localhost")
                    .header(header::ORIGIN, "http://localhost")
                    .header(
                        header::COOKIE,
                        "csrf_token=csrf123; chan_admin_session=session123",
                    )
                    .extension(crate::test_support::connect_info())
                    .body(Body::from(format!("_csrf={}", admin_signed_csrf())))
                    .context("build repair request")?,
            )
            .await
            .context("receive repair response")?;

        ensure!(response.status() == StatusCode::OK);
        let expected_refresh = format!(
            "10; url={}",
            super::db_repair_status_url(state.db_maintenance_jobs.snapshot().job_id())
        );
        ensure!(
            response
                .headers()
                .get(header::REFRESH)
                .and_then(|value| value.to_str().ok())
                == Some(expected_refresh.as_str())
        );
        let started_body = to_bytes(response.into_body(), usize::MAX)
            .await
            .context("read repair-started body")?;
        let started_body =
            String::from_utf8(started_body.to_vec()).context("decode repair-started body")?;
        ensure!(started_body.contains("[ database repair ]"));

        let body = wait_for_repair_result(&router).await?;

        {
            let mut failure = PRE_REPAIR_BACKUP_FAILURE
                .lock()
                .map_err(|_| anyhow::anyhow!("backup failure mutex was poisoned"))?;
            *failure = None;
        }

        ensure!(body.contains("[ database repair ]"));
        ensure!(body.contains("Repair was not run because the pre-repair backup failed."));
        ensure!(body.contains("Pre-repair backup failed:"));
        ensure!(body.contains("simulated pre-repair backup failure"));
        ensure!(body.contains("<strong>Repair run:</strong> No"));
        ensure!(body.contains("No repair or maintenance actions were run."));
        ensure!(body.contains("No maintenance steps were run."));
        ensure!(body.contains("// repair outcome"));
        ensure!(body.contains("// maintenance actions run"));
        ensure!(body.contains("back to admin panel"));

        ensure!(posts_ai_trigger_sql(&state)? == sentinel_trigger_sql);
        Ok(())
    }

    #[expect(
        clippy::too_many_lines,
        reason = "the end-to-end test keeps its fixture setup and ordered assertions in one scenario"
    )]
    #[tokio::test]
    async fn admin_db_repair_reports_problem_when_integrity_issue_remains_after_repair(
    ) -> anyhow::Result<()> {
        let _guard = REPAIR_TEST_LOCK.lock().await;
        let state = crate::test_support::app_state();
        install_admin_session(&state)?;
        create_controlled_integrity_problem(&state)?;

        let router = Router::new()
            .route("/admin/db/check", post(super::admin_db_check))
            .route(
                "/admin/db/repair",
                get(admin_db_repair_status).post(admin_db_repair),
            )
            .route("/admin/db/repair/status", get(admin_db_repair_status))
            .route(
                "/admin/db/repair/progress",
                get(super::admin_db_repair_progress_json),
            )
            .with_state(state.clone());

        let check_response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/admin/db/check")
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .header(header::HOST, "localhost")
                    .header(header::ORIGIN, "http://localhost")
                    .header(
                        header::COOKIE,
                        "csrf_token=csrf123; chan_admin_session=session123",
                    )
                    .extension(crate::test_support::connect_info())
                    .body(Body::from(format!("_csrf={}", admin_signed_csrf())))
                    .context("build database-check request")?,
            )
            .await
            .context("receive database-check response")?;

        ensure!(check_response.status() == StatusCode::OK);
        let check_body = to_bytes(check_response.into_body(), usize::MAX)
            .await
            .context("read database-check body")?;
        let check_body =
            String::from_utf8(check_body.to_vec()).context("decode database-check body")?;
        ensure!(check_body.contains("Database health checks found a problem."));

        let repair_response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/admin/db/repair")
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .header(header::HOST, "localhost")
                    .header(header::ORIGIN, "http://localhost")
                    .header(
                        header::COOKIE,
                        "csrf_token=csrf123; chan_admin_session=session123",
                    )
                    .extension(crate::test_support::connect_info())
                    .body(Body::from(format!("_csrf={}", admin_signed_csrf())))
                    .context("build database-repair request")?,
            )
            .await
            .context("receive database-repair response")?;

        ensure!(repair_response.status() == StatusCode::OK);
        let expected_refresh = format!(
            "10; url={}",
            super::db_repair_status_url(state.db_maintenance_jobs.snapshot().job_id())
        );
        ensure!(
            repair_response
                .headers()
                .get(header::REFRESH)
                .and_then(|value| value.to_str().ok())
                == Some(expected_refresh.as_str())
        );
        let started_body = to_bytes(repair_response.into_body(), usize::MAX)
            .await
            .context("read repair-started body")?;
        let started_body =
            String::from_utf8(started_body.to_vec()).context("decode repair-started body")?;
        ensure!(started_body.contains("[ database repair ]"));

        let repair_body = wait_for_repair_result(&router).await?;

        ensure!(repair_body.contains("[ database repair ]"));
        ensure!(
            repair_body.contains("Created pre-repair DB + config backup:"),
            "{repair_body}"
        );
        ensure!(repair_body.contains("<strong>Repair run:</strong> Yes"));
        ensure!(repair_body.contains("Repair finished, but the database still reports a problem."));
        ensure!(repair_body.contains("<strong>Pre-repair backup:</strong> <code>"));
        ensure!(repair_body.contains("<strong>Pre-repair backup type:</strong> DB + config"));
        ensure!(!repair_body
            .contains("Maintenance completed. Database health checks passed afterward."));
        ensure!(!repair_body.contains(
            "The final database health check passed after the repair run, so the detected problem was cleared."
        ));

        let conn = state.db.get().context("get database connection")?;
        let remaining_problem: i64 = conn
            .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
                row.get(0)
            })
            .context("run foreign-key check")?;
        ensure!(
            remaining_problem > 0,
            "controlled integrity issue should remain"
        );
        Ok(())
    }

    #[tokio::test]
    async fn admin_db_repair_get_shows_status_instead_of_method_not_allowed() -> anyhow::Result<()>
    {
        let _guard = REPAIR_TEST_LOCK.lock().await;
        let state = crate::test_support::app_state();
        install_admin_session(&state)?;

        let router = repair_router(state);

        let response = admin_get(&router, "/admin/db/repair").await?;

        ensure!(response.status() == StatusCode::OK);
        let body = response_body(response).await?;
        ensure!(body.contains("[ database repair ]"));
        Ok(())
    }

    #[tokio::test]
    async fn admin_db_repair_idle_get_page_is_not_running() -> anyhow::Result<()> {
        let _guard = REPAIR_TEST_LOCK.lock().await;
        let state = crate::test_support::app_state();
        install_admin_session(&state)?;
        let router = repair_router(state);

        let response = admin_get(&router, "/admin/db/repair").await?;

        ensure!(response.status() == StatusCode::OK);
        ensure!(response.headers().get(header::REFRESH).is_none());
        let body = response_body(response).await?;
        ensure!(body.contains("No maintenance rebuild is running."));
        ensure!(!body.contains("maintenance rebuild running"));
        ensure!(!body.contains("Maintenance rebuild started at <code>0</code>"));
        ensure!(!body.contains("data-db-repair-progress"));
        ensure!(!body.contains("data-db-repair-progress-url"));
        Ok(())
    }

    #[tokio::test]
    async fn admin_db_repair_idle_status_page_is_not_running() -> anyhow::Result<()> {
        let _guard = REPAIR_TEST_LOCK.lock().await;
        let state = crate::test_support::app_state();
        install_admin_session(&state)?;
        let router = repair_router(state);

        let response = admin_get(&router, "/admin/db/repair/status").await?;

        ensure!(response.status() == StatusCode::OK);
        ensure!(response.headers().get(header::REFRESH).is_none());
        let body = response_body(response).await?;
        ensure!(body.contains("No maintenance rebuild is running."));
        ensure!(!body.contains("maintenance rebuild running"));
        ensure!(!body.contains("Maintenance rebuild started at <code>0</code>"));
        ensure!(!body.contains("data-db-repair-progress"));
        Ok(())
    }

    #[tokio::test]
    async fn admin_db_repair_running_status_page_carries_job_identity() -> anyhow::Result<()> {
        let _guard = REPAIR_TEST_LOCK.lock().await;
        let state = crate::test_support::app_state();
        install_admin_session(&state)?;
        let job_id = state.db_maintenance_jobs.mark_running();
        let router = repair_router(state);

        let response = admin_get(&router, "/admin/db/repair/status").await?;

        ensure!(response.status() == StatusCode::OK);
        let expected_refresh = format!("10; url=/admin/db/repair/status?job_id={job_id}");
        ensure!(
            response
                .headers()
                .get(header::REFRESH)
                .and_then(|value| value.to_str().ok())
                == Some(expected_refresh.as_str())
        );
        let body = response_body(response).await?;
        ensure!(body.contains("maintenance rebuild running"));
        ensure!(body.contains(&format!(
            r#"data-db-repair-progress-url="/admin/db/repair/progress?job_id={job_id}""#
        )));
        ensure!(body.contains(&format!(r#"data-db-repair-job-id="{job_id}""#)));
        ensure!(body.contains(&format!(
            r#"href="/admin/db/repair/status?job_id={job_id}""#
        )));
        Ok(())
    }

    #[tokio::test]
    async fn admin_db_repair_terminal_status_is_bound_to_specific_job_id() -> anyhow::Result<()> {
        let _guard = REPAIR_TEST_LOCK.lock().await;
        let state = crate::test_support::app_state();
        install_admin_session(&state)?;
        let conn = state.db.get().context("get database connection")?;
        let first_job_id = state.db_maintenance_jobs.mark_running();
        ensure!(
            state
                .db_maintenance_jobs
                .mark_finished(first_job_id, crate::db::check_db_health(&conn)),
            "the active first repair job should accept its completion report"
        );
        let second_job_id = state.db_maintenance_jobs.mark_running();
        drop(conn);
        let router = repair_router(state);

        let stale_progress = admin_get(
            &router,
            &format!("/admin/db/repair/progress?job_id={first_job_id}"),
        )
        .await?;
        ensure!(stale_progress.status() == StatusCode::OK);
        let stale_progress = response_body(stale_progress).await?;
        let stale_progress: serde_json::Value =
            serde_json::from_str(&stale_progress).context("parse stale-progress JSON")?;
        ensure!(
            stale_progress
                .get("state")
                .and_then(serde_json::Value::as_str)
                == Some("stale")
        );
        ensure!(
            stale_progress
                .get("job_id")
                .and_then(serde_json::Value::as_u64)
                == Some(second_job_id)
        );
        let expected_redirect = format!("/admin/db/repair/status?job_id={second_job_id}");
        ensure!(
            stale_progress
                .get("redirect_url")
                .and_then(serde_json::Value::as_str)
                == Some(expected_redirect.as_str())
        );

        let stale_status = admin_get(
            &router,
            &format!("/admin/db/repair/status?job_id={first_job_id}"),
        )
        .await?;
        ensure!(stale_status.status() == StatusCode::OK);
        ensure!(stale_status.headers().get(header::REFRESH).is_none());
        let stale_status_body = response_body(stale_status).await?;
        ensure!(stale_status_body.contains(&format!(
            "This page is for maintenance rebuild <code>{first_job_id}</code>"
        )));
        ensure!(stale_status_body.contains(&format!(
            r#"href="/admin/db/repair/status?job_id={second_job_id}""#
        )));
        Ok(())
    }

    #[tokio::test]
    async fn admin_db_repair_finished_status_page_shows_matching_run_id() -> anyhow::Result<()> {
        let _guard = REPAIR_TEST_LOCK.lock().await;
        let state = crate::test_support::app_state();
        install_admin_session(&state)?;
        let conn = state.db.get().context("get database connection")?;
        let job_id = state.db_maintenance_jobs.mark_running();
        ensure!(
            state
                .db_maintenance_jobs
                .mark_finished(job_id, crate::db::check_db_health(&conn)),
            "the active repair job should accept its completion report"
        );
        drop(conn);
        let router = repair_router(state);

        let response =
            admin_get(&router, &format!("/admin/db/repair/status?job_id={job_id}")).await?;

        ensure!(response.status() == StatusCode::OK);
        ensure!(response.headers().get(header::REFRESH).is_none());
        let body = response_body(response).await?;
        ensure!(body.contains("[ database repair ]"));
        ensure!(body.contains(&format!("<strong>Run id:</strong> <code>{job_id}</code>")));
        Ok(())
    }

    #[tokio::test]
    async fn admin_vacuum_is_blocked_while_other_maintenance_is_active() -> anyhow::Result<()> {
        let _guard = REPAIR_TEST_LOCK.lock().await;
        let state = crate::test_support::app_state();
        install_admin_session(&state)?;
        let router = vacuum_router(state.clone());
        let _maintenance_guard = state
            .maintenance_gate
            .try_begin("Full backup creation")
            .context("begin competing maintenance operation")?;

        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/admin/vacuum")
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .header(header::HOST, "localhost")
                    .header(header::ORIGIN, "http://localhost")
                    .header(
                        header::COOKIE,
                        "csrf_token=csrf123; chan_admin_session=session123",
                    )
                    .extension(crate::test_support::connect_info())
                    .body(Body::from(format!("_csrf={}", admin_signed_csrf())))
                    .context("build vacuum request")?,
            )
            .await
            .context("receive vacuum response")?;

        ensure!(response.status() == StatusCode::CONFLICT);
        let body = response_body(response).await?;
        ensure!(body.contains("Full backup creation is already running"));
        Ok(())
    }
}
