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
    let html = tokio::task::spawn_blocking({
        let pool = state.db.clone();
        let progress = state.backup_progress.clone();
        let copies_to_keep = state.auto_full_backup_settings.snapshot().copies_to_keep;
        let csrf_clone = csrf.clone();
        move || -> Result<String> {
            {
                let conn = pool.get()?;
                super::require_admin_session_sid(&conn, session_id.as_deref())?;
            }

            let backup_result = create_pre_repair_backup(&pool, &progress, copies_to_keep);

            let conn = pool.get()?;

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

            Ok(crate::templates::admin_db_health_result_page(
                &report,
                true,
                &csrf_clone,
            ))
        }
    })
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))??;

    Ok((jar, Html(html)).into_response())
}

#[cfg(test)]
mod tests {
    use super::{admin_db_repair, PRE_REPAIR_BACKUP_FAILURE};
    use axum::{
        body::{to_bytes, Body},
        http::{header, Request, StatusCode},
        routing::post,
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
            .with_state(state.clone());
        let response = router
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

        {
            let mut failure = PRE_REPAIR_BACKUP_FAILURE
                .lock()
                .expect("backup failure mutex");
            *failure = None;
        }

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body bytes");
        let body = String::from_utf8(body.to_vec()).expect("utf8 body");

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
}
