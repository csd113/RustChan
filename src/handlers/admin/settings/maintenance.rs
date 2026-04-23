// Route modules use broad imports on purpose so the handler code stays compact and close to the module API.
#![allow(clippy::wildcard_imports)]

use super::*;

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

    crate::handlers::admin::create_full_backup_to_server(pool, None, progress, copies_to_keep)
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
    state.db_maintenance_jobs.mark_running();
    let progress = state.backup_progress.clone();
    let copies_to_keep = state.auto_full_backup_settings.snapshot().copies_to_keep;
    let pool = state.db.clone();
    let db_maintenance_jobs = state.db_maintenance_jobs.clone();

    tokio::spawn(async move {
        let job_status = db_maintenance_jobs.clone();
        let join_result = tokio::task::spawn_blocking(move || {
            let _maintenance_guard = maintenance_guard;
            let backup_result = create_pre_repair_backup(&pool, &progress, copies_to_keep);

            let conn = match pool.get() {
                Ok(conn) => conn,
                Err(error) => {
                    db_maintenance_jobs.mark_failed(format!(
                        "Could not open database connection after pre-repair backup: {error}"
                    ));
                    return;
                }
            };

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
            db_maintenance_jobs.mark_finished(report);
        })
        .await;

        if let Err(error) = join_result {
            job_status.mark_failed(format!("Database repair task failed: {error}"));
        }
    });

    Ok(render_db_repair_status_response(
        &jar,
        &csrf,
        state.db_maintenance_jobs.snapshot(),
    ))
}

pub async fn admin_db_repair_status(
    State(state): State<AppState>,
    jar: CookieJar,
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
    ))
}

fn render_db_repair_status_response(
    jar: &CookieJar,
    csrf: &str,
    status: crate::middleware::DbMaintenanceJobStatus,
) -> Response {
    let should_refresh = matches!(
        status,
        crate::middleware::DbMaintenanceJobStatus::Idle
            | crate::middleware::DbMaintenanceJobStatus::Running { .. }
    );
    let mut response = match status {
        crate::middleware::DbMaintenanceJobStatus::Idle => (
            jar.clone(),
            Html(crate::templates::admin_db_repair_running_page(csrf, 0)),
        )
            .into_response(),
        crate::middleware::DbMaintenanceJobStatus::Running { started_at } => (
            jar.clone(),
            Html(crate::templates::admin_db_repair_running_page(
                csrf, started_at,
            )),
        )
            .into_response(),
        crate::middleware::DbMaintenanceJobStatus::Finished { report } => (
            jar.clone(),
            Html(crate::templates::admin_db_health_result_page(
                &report, true, csrf,
            )),
        )
            .into_response(),
        crate::middleware::DbMaintenanceJobStatus::Failed {
            finished_at,
            message,
        } => (
            jar.clone(),
            Html(crate::templates::admin_db_repair_failed_page(
                csrf,
                &message,
                finished_at,
            )),
        )
            .into_response(),
    };

    if should_refresh {
        response
            .headers_mut()
            .insert(header::REFRESH, HeaderValue::from_static("3"));
    }

    response
}

#[cfg(test)]
mod tests {
    use super::{admin_db_repair, admin_db_repair_status, PRE_REPAIR_BACKUP_FAILURE};
    use axum::{
        body::{to_bytes, Body},
        http::{header, Request, StatusCode},
        routing::{get, post},
        Router,
    };
    use tower::ServiceExt as _;

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

    async fn repair_status_body(router: &Router) -> String {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/admin/db/repair/status")
                    .header(
                        header::COOKIE,
                        "csrf_token=csrf123; chan_admin_session=session123",
                    )
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("status response");
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("status body bytes");
        String::from_utf8(body.to_vec()).expect("utf8 status body")
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

        let router = Router::new()
            .route("/admin/db/repair", post(admin_db_repair))
            .route("/admin/db/repair/status", get(admin_db_repair_status))
            .with_state(state.clone());
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
        let state = crate::test_support::app_state();
        install_admin_session(&state);
        create_controlled_integrity_problem(&state);

        let router = Router::new()
            .route("/admin/db/check", post(super::admin_db_check))
            .route("/admin/db/repair", post(admin_db_repair))
            .route("/admin/db/repair/status", get(admin_db_repair_status))
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
}
