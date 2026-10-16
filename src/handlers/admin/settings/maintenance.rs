use super::{
    admin_panel_error_redirect_anchor, admin_panel_redirect_anchor, checkbox_is_on, db, header,
    require_admin_post_origin_and_csrf, require_admin_session_sid, AppError, AppState, CookieJar,
    Form, HeaderMap, HeaderValue, Html, Query, Response, Result, State, CONFIG, SESSION_COOKIE,
};
use axum::response::IntoResponse as _;
use serde::Deserialize;
use std::sync::atomic::Ordering;

#[derive(Deserialize)]
pub(in crate::server) struct VacuumForm {
    #[serde(rename = "_csrf")]
    pub csrf: Option<String>,
}

#[derive(Deserialize)]
pub(in crate::server) struct DbMaintenanceForm {
    #[serde(rename = "_csrf")]
    pub csrf: Option<String>,
}

#[derive(Deserialize)]
pub(in crate::server) struct MediaSettingsForm {
    #[serde(rename = "_csrf")]
    pub csrf: Option<String>,
    pub ffmpeg_timeout_secs: Option<String>,
    pub media_auto_prune_enabled: Option<String>,
    pub media_max_active_content_size: Option<String>,
    pub media_max_active_content_size_unit: Option<String>,
}

#[derive(Deserialize, Default)]
pub(in crate::server) struct DbRepairStatusQuery {
    pub job_id: Option<u64>,
}

fn create_pre_repair_backup(
    pool: &db::DbPool,
    progress: &std::sync::Arc<crate::middleware::BackupProgress>,
    job_id: u64,
) -> Result<String> {
    crate::handlers::admin::create_pre_maintenance_backup_to_server(
        pool,
        progress,
        "db_repair",
        job_id,
        "Automatic DB + config safety backup before database repair.",
    )
}

/// Run repair under the caller's maintenance guard, retaining stale-job diagnostics.
fn run_db_repair_job(
    pool: &db::DbPool,
    progress: &std::sync::Arc<crate::middleware::BackupProgress>,
    jobs: &crate::middleware::DbMaintenanceJobs,
    job_id: u64,
) {
    if !jobs.mark_phase(job_id, crate::middleware::DbMaintenanceJobPhase::Backup) {
        tracing::debug!(target: "admin", job_id, "Ignored backup phase update for stale database repair job");
    }
    let backup_result = create_pre_repair_backup(pool, progress, job_id);

    let conn = match pool.get() {
        Ok(conn) => conn,
        Err(error) => {
            if !jobs.mark_failed(
                job_id,
                format!("Could not open database connection after pre-repair backup: {error}"),
            ) {
                tracing::debug!(target: "admin", job_id, %error, "Ignored failure update for stale database repair job");
            }
            return;
        }
    };

    if !jobs.mark_phase(job_id, crate::middleware::DbMaintenanceJobPhase::Repair) {
        tracing::debug!(target: "admin", job_id, "Ignored repair phase update for stale database repair job");
    }
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
    if !jobs.mark_finished(job_id, report) {
        tracing::debug!(target: "admin", job_id, "Ignored completion update for stale database repair job");
    }
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

pub(in crate::server) async fn update_media_settings(
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
            let mut conn = pool.get()?;
            require_admin_session_sid(&conn, session_id.as_deref()).map(|_completed_value| ())?;
            let tx = conn.transaction().map_err(anyhow::Error::from)?;
            db::set_media_prune_settings(&tx, prune_enabled, prune_max_bytes)?;
            db::set_site_setting(&tx, "ffmpeg_timeout_secs", &timeout_secs.to_string())?;
            tx.commit().map_err(anyhow::Error::from)?;
            crate::config::set_live_ffmpeg_timeout_secs(timeout_secs)?;
            let prune_report = match crate::media::prune::run_configured_prune(&conn, &CONFIG.upload_dir) {
                Ok(report) => report,
                Err(error) => { tracing::warn!(target: "admin", error = %error, "Media settings saved; immediate pruning failed"); return Err(AppError::BadRequest("Media settings saved, but immediate pruning failed; review the log before retrying maintenance.".into())); }
            };
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

pub(in crate::server) async fn admin_vacuum(
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
            require_admin_session_sid(&conn, session_id.as_deref()).map(|_completed_value| ())?;

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

pub(in crate::server) async fn admin_db_check(
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
            require_admin_session_sid(&conn, session_id.as_deref()).map(|_completed_value| ())?;

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

pub(in crate::server) async fn admin_db_repair(
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
            require_admin_session_sid(&conn, session_id.as_deref()).map(|_completed_value| ())?;
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

    drop(tokio::spawn(async move {
        let job_status = db_maintenance_jobs.clone();
        let join_result = tokio::task::spawn_blocking(move || {
            let _maintenance_guard = maintenance_guard;
            run_db_repair_job(&pool, &progress, &db_maintenance_jobs, job_id);
        })
        .await;

        if let Err(error) = join_result {
            let failure_recorded =
                job_status.mark_failed(job_id, format!("Database repair task failed: {error}"));
            if !failure_recorded {
                tracing::debug!(target: "admin", job_id, %error, "Ignored task failure update for stale database repair job");
            }
        }
    }));

    Ok(render_db_repair_entry_response(
        &jar,
        &csrf,
        &state.db_maintenance_jobs.snapshot(),
    ))
}

pub(in crate::server) async fn admin_db_repair_progress_json(
    State(state): State<AppState>,
    jar: CookieJar,
    Query(query): Query<DbRepairStatusQuery>,
) -> Result<Response> {
    let session_id = jar.get(SESSION_COOKIE).map(|c| c.value().to_owned());

    tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<()> {
            let conn = pool.get()?;
            require_admin_session_sid(&conn, session_id.as_deref()).map(|_completed_value| ())?;
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
            started_at: _,
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
            started_at: _,
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
            started_at: _,
        } => serde_json::json!({
            "state": "running",
            "job_id": job_id,
            "label": "Rebuilding indexes and checking database health...",
            "percent": 82,
            "done": false,
        }),
        crate::middleware::DbMaintenanceJobStatus::Finished { job_id, report: _ } => {
            serde_json::json!({
                "state": "finished",
                "job_id": job_id,
                "label": "Maintenance rebuild complete. Opening report...",
                "percent": 100,
                "done": true,
                "redirect_url": db_repair_status_url(Some(job_id)),
            })
        }
        crate::middleware::DbMaintenanceJobStatus::Failed {
            job_id,
            message,
            finished_at: _,
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
            .map_or(55, |percent| 55_u64.saturating_add(percent)),
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

pub(in crate::server) async fn admin_db_repair_status(
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
            require_admin_session_sid(&conn, session_id.as_deref()).map(|_completed_value| ())?;
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
    job_id.map_or_else(
        || "/admin/db/repair".to_owned(),
        |job_id| format!("/admin/db/repair/status?job_id={job_id}"),
    )
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
        let _previous_value = response.headers_mut().insert(header::REFRESH, refresh);
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
            job_id,
            started_at,
            phase: _,
        } => render_db_repair_running_response(jar, csrf, *job_id, *started_at),
        crate::middleware::DbMaintenanceJobStatus::Idle
        | crate::middleware::DbMaintenanceJobStatus::Finished {
            job_id: _,
            report: _,
        }
        | crate::middleware::DbMaintenanceJobStatus::Failed {
            job_id: _,
            finished_at: _,
            message: _,
        } => (
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
            job_id,
            started_at,
            phase: _,
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
