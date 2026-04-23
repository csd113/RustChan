// Route modules use broad imports on purpose so the handler code stays compact and close to the module API.
#![allow(clippy::wildcard_imports)]

use super::*;
use std::sync::atomic::Ordering;

#[cfg(test)]
static PRE_REPAIR_BACKUP_FAILURE: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

#[derive(Deserialize)]
pub struct VacuumForm {
    #[serde(rename = "_csrf")]
    pub csrf: Option<String>,
}

#[derive(Deserialize)]
pub struct DbMaintenanceForm {
    #[serde(rename = "_csrf")]
    pub csrf: Option<String>,
}

#[derive(Deserialize, Default)]
pub struct DbRepairStatusQuery {
    pub job_id: Option<u64>,
}

fn create_pre_repair_backup(
    pool: &crate::db::DbPool,
    progress: &std::sync::Arc<crate::middleware::BackupProgress>,
    copies_to_keep: u64,
) -> Result<String> {
    #[cfg(test)]
    {
        let backup_failure = PRE_REPAIR_BACKUP_FAILURE
            .lock()
            .expect("backup failure mutex")
            .clone();
        if let Some(message) = backup_failure {
            return Err(AppError::Internal(anyhow::anyhow!(message)));
        }
    }

    crate::handlers::admin::create_full_backup_to_server(
        pool,
        None,
        progress,
        copies_to_keep,
        pre_repair_backup_include_tor_hidden_service_keys(),
    )
}

fn pre_repair_backup_include_tor_hidden_service_keys() -> bool {
    crate::config::configured_tor_hidden_service_keys_dir().is_some_and(|path| path.is_dir())
}

pub async fn admin_vacuum(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<VacuumForm>,
) -> Result<Response> {
    let session_id = jar
        .get(super::SESSION_COOKIE)
        .map(|c| c.value().to_string());
    super::check_csrf_jar(&jar, form.csrf.as_deref())?;

    let (jar, csrf) = ensure_csrf(jar);

    let html = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        let csrf_clone = csrf.clone();
        move || -> Result<String> {
            let conn = pool.get()?;
            super::require_admin_session_sid(&conn, session_id.as_deref())?;

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
            ))
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    Ok((jar, Html(html)).into_response())
}

pub async fn admin_db_check(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<DbMaintenanceForm>,
) -> Result<Response> {
    let session_id = jar
        .get(super::SESSION_COOKIE)
        .map(|c| c.value().to_string());
    super::check_csrf_jar(&jar, form.csrf.as_deref())?;

    let (jar, csrf) = ensure_csrf(jar);
    let html = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        let csrf_clone = csrf.clone();
        move || -> Result<String> {
            let conn = pool.get()?;
            super::require_admin_session_sid(&conn, session_id.as_deref())?;

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
            ))
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    Ok((jar, Html(html)).into_response())
}

pub async fn admin_db_repair(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<DbMaintenanceForm>,
) -> Result<Response> {
    let session_id = jar
        .get(super::SESSION_COOKIE)
        .map(|c| c.value().to_string());
    super::check_csrf_jar(&jar, form.csrf.as_deref())?;

    let (jar, csrf) = ensure_csrf(jar);
    tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<()> {
            let conn = pool.get()?;
            super::require_admin_session_sid(&conn, session_id.as_deref())?;
            Ok(())
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    let maintenance_guard = state
        .maintenance_gate
        .try_begin("Database maintenance rebuild")?;
    let job_id = state.db_maintenance_jobs.mark_running();
    let progress = state.backup_progress.clone();
    let copies_to_keep = state.auto_full_backup_settings.snapshot().copies_to_keep;
    let pool = state.db.clone();
    let db_maintenance_jobs = state.db_maintenance_jobs.clone();

    tokio::spawn(async move {
        let job_status = db_maintenance_jobs.clone();
        let join_result = tokio::task::spawn_blocking(move || {
            let _maintenance_guard = maintenance_guard;
            let _ = db_maintenance_jobs
                .mark_phase(job_id, crate::middleware::DbMaintenanceJobPhase::Backup);
            let backup_result = create_pre_repair_backup(&pool, &progress, copies_to_keep);

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
                Ok(filename) => db::attempt_db_repair(&conn, Some(db::DbRepairBackup { filename })),
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
                backup = report.repair_backup.as_ref().map(|backup| backup.filename.as_str()),
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
        state.db_maintenance_jobs.snapshot(),
    ))
}

pub async fn admin_db_repair_progress_json(
    State(state): State<AppState>,
    jar: CookieJar,
    Query(query): Query<DbRepairStatusQuery>,
) -> Result<Response> {
    let session_id = jar
        .get(super::SESSION_COOKIE)
        .map(|c| c.value().to_string());

    tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<()> {
            let conn = pool.get()?;
            super::require_admin_session_sid(&conn, session_id.as_deref())?;
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
        [(header::CONTENT_TYPE, "application/json".to_string())],
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
        crate::middleware::backup_phase::SNAPSHOT_DB => 15,
        crate::middleware::backup_phase::COUNT_FILES => 30,
        crate::middleware::backup_phase::COMPRESS => files_done
            .saturating_mul(30)
            .checked_div(files_total)
            .map_or(45, |percent| 45 + percent),
        crate::middleware::backup_phase::DONE => 78,
        _ => 10,
    }
}

fn backup_progress_label(phase: u64, files_done: u64, files_total: u64) -> String {
    match phase {
        crate::middleware::backup_phase::SNAPSHOT_DB => {
            "Creating pre-repair database snapshot...".to_string()
        }
        crate::middleware::backup_phase::COUNT_FILES => {
            "Counting files for the pre-repair backup...".to_string()
        }
        crate::middleware::backup_phase::COMPRESS => {
            if files_total == 0 {
                "Compressing pre-repair backup...".to_string()
            } else {
                format!("Compressing pre-repair backup... {files_done}/{files_total} files")
            }
        }
        crate::middleware::backup_phase::DONE => "Pre-repair backup complete...".to_string(),
        _ => "Preparing pre-repair backup...".to_string(),
    }
}

pub async fn admin_db_repair_status(
    State(state): State<AppState>,
    jar: CookieJar,
    Query(query): Query<DbRepairStatusQuery>,
) -> Result<Response> {
    let session_id = jar
        .get(super::SESSION_COOKIE)
        .map(|c| c.value().to_string());

    let (jar, csrf) = ensure_csrf(jar);
    tokio::task::spawn_blocking({
        let pool = state.db.clone();
        move || -> Result<()> {
            let conn = pool.get()?;
            super::require_admin_session_sid(&conn, session_id.as_deref())?;
            Ok(())
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    Ok(render_db_repair_status_response(
        &jar,
        &csrf,
        state.db_maintenance_jobs.snapshot(),
        query.job_id,
    ))
}

fn db_repair_status_url(job_id: Option<u64>) -> String {
    match job_id {
        Some(job_id) => format!("/admin/db/repair/status?job_id={job_id}"),
        None => "/admin/db/repair".to_string(),
    }
}

fn render_db_repair_running_response(
    jar: &CookieJar,
    csrf: &str,
    job_id: u64,
    started_at: i64,
) -> Response {
    let refresh = format!("10; url={}", db_repair_status_url(Some(job_id)));
    let mut response = (
        jar.clone(),
        Html(crate::templates::admin_db_repair_running_page(
            csrf, job_id, started_at,
        )),
    )
        .into_response();
    response.headers_mut().insert(
        header::REFRESH,
        HeaderValue::from_str(&refresh).expect("valid db repair refresh header"),
    );
    response
}

fn render_db_repair_entry_response(
    jar: &CookieJar,
    csrf: &str,
    status: crate::middleware::DbMaintenanceJobStatus,
) -> Response {
    match status {
        crate::middleware::DbMaintenanceJobStatus::Running {
            job_id, started_at, ..
        } => render_db_repair_running_response(jar, csrf, job_id, started_at),
        crate::middleware::DbMaintenanceJobStatus::Idle
        | crate::middleware::DbMaintenanceJobStatus::Finished { .. }
        | crate::middleware::DbMaintenanceJobStatus::Failed { .. } => (
            jar.clone(),
            Html(crate::templates::admin_db_repair_idle_page(csrf)),
        )
            .into_response(),
    }
}

fn render_db_repair_status_response(
    jar: &CookieJar,
    csrf: &str,
    status: crate::middleware::DbMaintenanceJobStatus,
    requested_job_id: Option<u64>,
) -> Response {
    if requested_job_id.is_some_and(|job_id| Some(job_id) != status.job_id()) {
        return (
            jar.clone(),
            Html(crate::templates::admin_db_repair_stale_page(
                csrf,
                requested_job_id.expect("requested job id for stale page"),
                status.job_id(),
            )),
        )
            .into_response();
    }

    match status {
        crate::middleware::DbMaintenanceJobStatus::Idle => (
            jar.clone(),
            Html(crate::templates::admin_db_repair_idle_page(csrf)),
        )
            .into_response(),
        crate::middleware::DbMaintenanceJobStatus::Running {
            job_id, started_at, ..
        } => render_db_repair_running_response(jar, csrf, job_id, started_at),
        crate::middleware::DbMaintenanceJobStatus::Finished { job_id, report } => (
            jar.clone(),
            Html(crate::templates::admin_db_health_result_page(
                &report,
                true,
                csrf,
                Some(job_id),
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
                &message,
                finished_at,
                job_id,
            )),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::{admin_db_repair, admin_db_repair_status, PRE_REPAIR_BACKUP_FAILURE};
    use axum::{
        body::{to_bytes, Body},
        http::{header, Request, StatusCode},
        response::Response,
        routing::{get, post},
        Router,
    };
    use tower::ServiceExt as _;

    static REPAIR_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    fn install_admin_session(state: &crate::middleware::AppState) {
        let conn = state.db.get().expect("db connection");
        let password_hash = crate::utils::crypto::hash_password("hunter2").expect("hash password");
        let admin_id =
            crate::db::create_admin(&conn, "admin", &password_hash).expect("create admin");
        crate::db::create_session(
            &conn,
            "session123",
            admin_id,
            chrono::Utc::now().timestamp() + 3600,
        )
        .expect("create session");
    }

    fn posts_ai_trigger_sql(state: &crate::middleware::AppState) -> String {
        let conn = state.db.get().expect("db connection");
        conn.query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'trigger' AND name = 'posts_ai'",
            [],
            |row| row.get(0),
        )
        .expect("posts_ai trigger sql")
    }

    fn create_controlled_integrity_problem(state: &crate::middleware::AppState) {
        let board_short = format!("f{}", &uuid::Uuid::new_v4().simple().to_string()[..7]);
        let conn = state.db.get().expect("db connection");
        let board_id = crate::db::create_board(&conn, &board_short, "Repair Test", "", false)
            .expect("create board");
        let post = crate::db::NewPost {
            thread_id: 0,
            board_id,
            name: "anon".to_string(),
            tripcode: None,
            subject: Some("repair thread".to_string()),
            body: "repair test body".to_string(),
            body_html: "repair test body".to_string(),
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
            deletion_token: "repair-token".to_string(),
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
        .expect("create thread");

        conn.execute_batch(&format!(
            "PRAGMA foreign_keys=OFF; BEGIN; DELETE FROM boards WHERE short_name='{board_short}'; COMMIT; PRAGMA foreign_keys=ON;"
        ))
        .expect("create controlled integrity problem");
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

    async fn admin_get(router: &Router, uri: &str) -> Response {
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
                    .expect("request"),
            )
            .await
            .expect("response")
    }

    async fn response_body(response: Response) -> String {
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body bytes");
        String::from_utf8(body.to_vec()).expect("utf8 body")
    }

    async fn repair_status_body(router: &Router) -> String {
        let response = admin_get(router, "/admin/db/repair/status").await;
        assert_eq!(response.status(), StatusCode::OK);
        response_body(response).await
    }

    async fn wait_for_repair_result(router: &Router) -> String {
        for _ in 0..100 {
            let body = repair_status_body(router).await;
            if !body.contains("maintenance rebuild running") {
                return body;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        panic!("database repair did not finish");
    }

    #[tokio::test]
    async fn admin_db_repair_aborts_before_mutation_when_pre_repair_backup_fails() {
        let _guard = REPAIR_TEST_LOCK.lock().await;
        let state = crate::test_support::app_state();
        install_admin_session(&state);

        let sentinel_trigger_sql =
            "CREATE TRIGGER posts_ai AFTER INSERT ON posts BEGIN SELECT RAISE(IGNORE); END";
        {
            let conn = state.db.get().expect("db connection");
            conn.execute_batch(&format!("DROP TRIGGER posts_ai; {sentinel_trigger_sql};"))
                .expect("install sentinel trigger");
        }

        {
            let mut failure = PRE_REPAIR_BACKUP_FAILURE
                .lock()
                .expect("backup failure mutex");
            *failure = Some("simulated pre-repair backup failure".to_string());
        }

        let router = repair_router(state.clone());
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/admin/db/repair")
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .header(
                        header::COOKIE,
                        "csrf_token=csrf123; chan_admin_session=session123",
                    )
                    .body(Body::from("_csrf=csrf123"))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        let expected_refresh = format!(
            "10; url={}",
            super::db_repair_status_url(state.db_maintenance_jobs.snapshot().job_id())
        );
        assert_eq!(
            response
                .headers()
                .get(header::REFRESH)
                .and_then(|value| value.to_str().ok()),
            Some(expected_refresh.as_str())
        );
        let started_body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("started body bytes");
        let started_body = String::from_utf8(started_body.to_vec()).expect("utf8 started body");
        assert!(started_body.contains("[ database repair ]"));

        let body = wait_for_repair_result(&router).await;

        {
            let mut failure = PRE_REPAIR_BACKUP_FAILURE
                .lock()
                .expect("backup failure mutex");
            *failure = None;
        }

        assert!(body.contains("[ database repair ]"));
        assert!(body.contains("Repair was not run because the pre-repair backup failed."));
        assert!(body.contains("Pre-repair backup failed:"));
        assert!(body.contains("simulated pre-repair backup failure"));
        assert!(body.contains("<strong>Repair run:</strong> No"));
        assert!(body.contains("No repair or maintenance actions were run."));
        assert!(body.contains("No maintenance steps were run."));
        assert!(body.contains("// repair outcome"));
        assert!(body.contains("// maintenance actions run"));
        assert!(body.contains("back to admin panel"));

        assert_eq!(posts_ai_trigger_sql(&state), sentinel_trigger_sql);
    }

    #[tokio::test]
    async fn admin_db_repair_reports_problem_when_integrity_issue_remains_after_repair() {
        let _guard = REPAIR_TEST_LOCK.lock().await;
        let state = crate::test_support::app_state();
        install_admin_session(&state);
        create_controlled_integrity_problem(&state);

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
                    .header(
                        header::COOKIE,
                        "csrf_token=csrf123; chan_admin_session=session123",
                    )
                    .body(Body::from("_csrf=csrf123"))
                    .expect("request"),
            )
            .await
            .expect("check response");

        assert_eq!(check_response.status(), StatusCode::OK);
        let check_body = to_bytes(check_response.into_body(), usize::MAX)
            .await
            .expect("check body bytes");
        let check_body = String::from_utf8(check_body.to_vec()).expect("utf8 check body");
        assert!(check_body.contains("Database health checks found a problem."));

        let repair_response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/admin/db/repair")
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .header(
                        header::COOKIE,
                        "csrf_token=csrf123; chan_admin_session=session123",
                    )
                    .body(Body::from("_csrf=csrf123"))
                    .expect("request"),
            )
            .await
            .expect("repair response");

        assert_eq!(repair_response.status(), StatusCode::OK);
        let expected_refresh = format!(
            "10; url={}",
            super::db_repair_status_url(state.db_maintenance_jobs.snapshot().job_id())
        );
        assert_eq!(
            repair_response
                .headers()
                .get(header::REFRESH)
                .and_then(|value| value.to_str().ok()),
            Some(expected_refresh.as_str())
        );
        let started_body = to_bytes(repair_response.into_body(), usize::MAX)
            .await
            .expect("repair started body bytes");
        let started_body = String::from_utf8(started_body.to_vec()).expect("utf8 repair body");
        assert!(started_body.contains("[ database repair ]"));

        let repair_body = wait_for_repair_result(&router).await;

        assert!(repair_body.contains("[ database repair ]"));
        assert!(
            repair_body.contains("Created pre-repair full backup:"),
            "{repair_body}"
        );
        assert!(repair_body.contains("<strong>Repair run:</strong> Yes"));
        assert!(repair_body.contains("Repair finished, but the database still reports a problem."));
        assert!(repair_body.contains("<strong>Pre-repair backup:</strong> <code>"));
        assert!(!repair_body
            .contains("Maintenance completed. Database health checks passed afterward."));
        assert!(!repair_body.contains(
            "The final database health check passed after the repair run, so the detected problem was cleared."
        ));

        let conn = state.db.get().expect("db connection");
        let remaining_problem: i64 = conn
            .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
                row.get(0)
            })
            .expect("foreign key check");
        assert!(
            remaining_problem > 0,
            "controlled integrity issue should remain"
        );
    }

    #[tokio::test]
    async fn admin_db_repair_get_shows_status_instead_of_method_not_allowed() {
        let _guard = REPAIR_TEST_LOCK.lock().await;
        let state = crate::test_support::app_state();
        install_admin_session(&state);

        let router = repair_router(state);

        let response = admin_get(&router, "/admin/db/repair").await;

        assert_eq!(response.status(), StatusCode::OK);
        let body = response_body(response).await;
        assert!(body.contains("[ database repair ]"));
    }

    #[tokio::test]
    async fn admin_db_repair_idle_get_page_is_not_running() {
        let _guard = REPAIR_TEST_LOCK.lock().await;
        let state = crate::test_support::app_state();
        install_admin_session(&state);
        let router = repair_router(state);

        let response = admin_get(&router, "/admin/db/repair").await;

        assert_eq!(response.status(), StatusCode::OK);
        assert!(response.headers().get(header::REFRESH).is_none());
        let body = response_body(response).await;
        assert!(body.contains("No maintenance rebuild is running."));
        assert!(!body.contains("maintenance rebuild running"));
        assert!(!body.contains("Maintenance rebuild started at <code>0</code>"));
        assert!(!body.contains("data-db-repair-progress"));
        assert!(!body.contains("data-db-repair-progress-url"));
    }

    #[tokio::test]
    async fn admin_db_repair_idle_status_page_is_not_running() {
        let _guard = REPAIR_TEST_LOCK.lock().await;
        let state = crate::test_support::app_state();
        install_admin_session(&state);
        let router = repair_router(state);

        let response = admin_get(&router, "/admin/db/repair/status").await;

        assert_eq!(response.status(), StatusCode::OK);
        assert!(response.headers().get(header::REFRESH).is_none());
        let body = response_body(response).await;
        assert!(body.contains("No maintenance rebuild is running."));
        assert!(!body.contains("maintenance rebuild running"));
        assert!(!body.contains("Maintenance rebuild started at <code>0</code>"));
        assert!(!body.contains("data-db-repair-progress"));
    }

    #[tokio::test]
    async fn admin_db_repair_running_status_page_carries_job_identity() {
        let _guard = REPAIR_TEST_LOCK.lock().await;
        let state = crate::test_support::app_state();
        install_admin_session(&state);
        let job_id = state.db_maintenance_jobs.mark_running();
        let router = repair_router(state);

        let response = admin_get(&router, "/admin/db/repair/status").await;

        assert_eq!(response.status(), StatusCode::OK);
        let expected_refresh = format!("10; url=/admin/db/repair/status?job_id={job_id}");
        assert_eq!(
            response
                .headers()
                .get(header::REFRESH)
                .and_then(|value| value.to_str().ok()),
            Some(expected_refresh.as_str())
        );
        let body = response_body(response).await;
        assert!(body.contains("maintenance rebuild running"));
        assert!(body.contains(&format!(
            r#"data-db-repair-progress-url="/admin/db/repair/progress?job_id={job_id}""#
        )));
        assert!(body.contains(&format!(r#"data-db-repair-job-id="{job_id}""#)));
        assert!(body.contains(&format!(
            r#"href="/admin/db/repair/status?job_id={job_id}""#
        )));
    }

    #[tokio::test]
    async fn admin_db_repair_terminal_status_is_bound_to_specific_job_id() {
        let _guard = REPAIR_TEST_LOCK.lock().await;
        let state = crate::test_support::app_state();
        install_admin_session(&state);
        let conn = state.db.get().expect("db connection");
        let first_job_id = state.db_maintenance_jobs.mark_running();
        state
            .db_maintenance_jobs
            .mark_finished(first_job_id, crate::db::check_db_health(&conn));
        let second_job_id = state.db_maintenance_jobs.mark_running();
        let router = repair_router(state);

        let stale_progress = admin_get(
            &router,
            &format!("/admin/db/repair/progress?job_id={first_job_id}"),
        )
        .await;
        assert_eq!(stale_progress.status(), StatusCode::OK);
        let stale_progress = response_body(stale_progress).await;
        let stale_progress: serde_json::Value =
            serde_json::from_str(&stale_progress).expect("stale progress json");
        assert_eq!(
            stale_progress
                .get("state")
                .and_then(serde_json::Value::as_str),
            Some("stale")
        );
        assert_eq!(
            stale_progress
                .get("job_id")
                .and_then(serde_json::Value::as_u64),
            Some(second_job_id)
        );
        let expected_redirect = format!("/admin/db/repair/status?job_id={second_job_id}");
        assert_eq!(
            stale_progress
                .get("redirect_url")
                .and_then(serde_json::Value::as_str),
            Some(expected_redirect.as_str())
        );

        let stale_status = admin_get(
            &router,
            &format!("/admin/db/repair/status?job_id={first_job_id}"),
        )
        .await;
        assert_eq!(stale_status.status(), StatusCode::OK);
        assert!(stale_status.headers().get(header::REFRESH).is_none());
        let stale_status_body = response_body(stale_status).await;
        assert!(stale_status_body.contains(&format!(
            "This page is for maintenance rebuild <code>{first_job_id}</code>"
        )));
        assert!(stale_status_body.contains(&format!(
            r#"href="/admin/db/repair/status?job_id={second_job_id}""#
        )));
    }

    #[tokio::test]
    async fn admin_db_repair_finished_status_page_shows_matching_run_id() {
        let _guard = REPAIR_TEST_LOCK.lock().await;
        let state = crate::test_support::app_state();
        install_admin_session(&state);
        let conn = state.db.get().expect("db connection");
        let job_id = state.db_maintenance_jobs.mark_running();
        state
            .db_maintenance_jobs
            .mark_finished(job_id, crate::db::check_db_health(&conn));
        let router = repair_router(state);

        let response =
            admin_get(&router, &format!("/admin/db/repair/status?job_id={job_id}")).await;

        assert_eq!(response.status(), StatusCode::OK);
        assert!(response.headers().get(header::REFRESH).is_none());
        let body = response_body(response).await;
        assert!(body.contains("[ database repair ]"));
        assert!(body.contains(&format!("<strong>Run id:</strong> <code>{job_id}</code>")));
    }
}
