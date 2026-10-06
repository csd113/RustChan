//! End-to-end regression coverage for personal thread actions and reporting.

use anyhow::{ensure, Context as _, Result};
use axum::{
    body::{to_bytes, Body},
    http::{header, Request, StatusCode},
    response::Response,
    routing::{get, post},
    Router,
};
use tower::ServiceExt as _;

/// Seed a renderable thread with intentionally different thread/post identifiers.
fn seed_thread(conn: &rusqlite::Connection, board_id: i64) -> Result<(i64, i64)> {
    let thread_id = conn.query_row(
        "INSERT INTO threads (board_id, bumped_at) VALUES (?1, 100) RETURNING id",
        [board_id],
        |row| row.get(0),
    )?;
    let post_id = conn.query_row(
        "INSERT INTO posts (id, thread_id, board_id, body, body_html, deletion_token, is_op)
         VALUES (?1 + 100, ?1, ?2, 'action target', 'action target', 'token', 1) RETURNING id",
        rusqlite::params![thread_id, board_id],
        |row| row.get(0),
    )?;
    Ok((thread_id, post_id))
}

/// Exercise actual Axum extraction and handler responses with a stable viewer.
async fn request(router: &Router, method: &str, path: &str, body: String) -> Result<Response> {
    Ok(router
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header(header::HOST, "localhost")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .header(
                    header::COOKIE,
                    "csrf_token=csrf123; rustchan_visitor_id=actions-viewer",
                )
                .extension(crate::test_support::connect_info())
                .body(Body::from(body))?,
        )
        .await?)
}

/// Read a bounded rendered response for state and identifier assertions.
async fn html(response: Response) -> Result<String> {
    Ok(String::from_utf8(
        to_bytes(response.into_body(), 1_000_000).await?.to_vec(),
    )?)
}

/// Router covering the same preference, report, and rendering paths as production.
fn action_router(state: crate::middleware::AppState) -> Router {
    Router::new()
        .route(
            "/{board}/thread-preference",
            post(super::update_thread_preference),
        )
        .route("/{board}/catalog", get(super::catalog))
        .route("/{board}/hidden", get(super::hidden_threads))
        .route("/{board}", get(super::board_index))
        .route(
            "/{board}/thread/{id}",
            get(crate::handlers::thread::view_thread),
        )
        .route(
            "/report",
            post(super::file_report).layer(axum::extract::DefaultBodyLimit::max(65_536)),
        )
        .with_state(state)
}

/// Identity corresponding to the request fixture's loopback visitor cookie.
fn viewer_key() -> String {
    crate::utils::crypto::hash_ip(
        "visitor:actions-viewer",
        &crate::config::CONFIG.cookie_secret,
    )
}

#[tokio::test]
async fn personal_actions_are_explicit_idempotent_isolated_and_reversible() -> Result<()> {
    let state = crate::test_support::app_state();
    let conn = state.db.get()?;
    let board = crate::db::create_board(&conn, "actions", "Actions", "", false)?;
    let (thread, _post) = seed_thread(&conn, board)?;
    let router = action_router(state.clone());
    for action in ["pin", "pin", "hide", "hide", "unhide", "unpin", "unpin"] {
        let response = request(&router, "POST", "/actions/thread-preference", format!(
            "thread_id={thread}&board=actions&action={action}&_csrf=csrf123&return_to=%2F%2Fevil.example"
        )).await?;
        ensure!(response.status() == StatusCode::SEE_OTHER);
        ensure!(
            response
                .headers()
                .get(header::LOCATION)
                .context("redirect")?
                == "/actions/catalog"
        );
        let preference =
            crate::db::get_thread_preference(&conn, &viewer_key(), thread)?.unwrap_or_default();
        match action {
            "pin" | "unhide" => ensure!(preference.pinned && !preference.hidden),
            "hide" => ensure!(preference.pinned && preference.hidden),
            "unpin" => ensure!(!preference.pinned && !preference.hidden),
            _ => anyhow::bail!("unknown test action"),
        }
        ensure!(crate::db::get_thread_preference(&conn, "another-viewer", thread)?.is_none());
        ensure!(
            !crate::db::get_thread(&conn, thread)?
                .context("thread")?
                .sticky,
            "personal pin must never mutate global moderation state"
        );
    }
    let rows: i64 = conn.query_row("SELECT COUNT(*) FROM user_thread_preferences", [], |row| {
        row.get(0)
    })?;
    ensure!(rows == 0, "default preferences must be removed");
    Ok(())
}

#[tokio::test]
async fn forged_personal_actions_cannot_mutate_another_board_or_bypass_view_access() -> Result<()> {
    let state = crate::test_support::app_state();
    let conn = state.db.get()?;
    let board = crate::db::create_board(&conn, "actions", "Actions", "", false)?;
    let other = crate::db::create_board(&conn, "other", "Other", "", false)?;
    let (thread, _post) = seed_thread(&conn, other)?;
    let router = action_router(state);
    for (body, status) in [
        (
            format!("thread_id={thread}&board=actions&action=pin&_csrf=bad"),
            StatusCode::FORBIDDEN,
        ),
        (
            format!("thread_id={thread}&board=act%2Fions&action=pin&_csrf=csrf123"),
            StatusCode::BAD_REQUEST,
        ),
        (
            format!("thread_id={thread}&board=actions&action=pin&_csrf=csrf123"),
            StatusCode::NOT_FOUND,
        ),
        (
            "thread_id=-1&board=actions&action=pin&_csrf=csrf123".to_owned(),
            StatusCode::BAD_REQUEST,
        ),
        (
            "thread_id=1&board=actions&action=sticky&_csrf=csrf123".to_owned(),
            StatusCode::BAD_REQUEST,
        ),
    ] {
        ensure!(
            request(&router, "POST", "/actions/thread-preference", body)
                .await?
                .status()
                == status
        );
    }
    let (private_thread, _private_post) = seed_thread(&conn, board)?;
    conn.execute(
        "UPDATE boards SET access_mode='view_password', access_password_hash='locked' WHERE id=?1",
        [board],
    )
    .map(|_rows| ())?;
    ensure!(
        request(
            &router,
            "POST",
            "/actions/thread-preference",
            format!("thread_id={private_thread}&board=actions&action=pin&_csrf=csrf123")
        )
        .await?
        .status()
            == StatusCode::FORBIDDEN
    );
    ensure!(
        request(&router, "GET", "/actions/thread-preference", String::new())
            .await?
            .status()
            == StatusCode::METHOD_NOT_ALLOWED
    );
    let rows: i64 = conn.query_row("SELECT COUNT(*) FROM user_thread_preferences", [], |row| {
        row.get(0)
    })?;
    ensure!(rows == 0);
    Ok(())
}

#[test]
fn personal_listing_filters_before_pagination_and_orders_all_pins_deterministically() -> Result<()>
{
    let pool = crate::db::init_test_pool()?;
    let conn = pool.get()?;
    let board = crate::db::create_board(&conn, "actions", "Actions", "", false)?;
    let mut ids = Vec::new();
    for _number in 0_i32..205_i32 {
        ids.push(seed_thread(&conn, board)?.0);
    }
    let first = *ids.first().context("first")?;
    let second = *ids.get(1).context("second")?;
    let sticky = *ids.get(2).context("sticky")?;
    let last = *ids.last().context("last")?;
    crate::db::set_thread_pinned(&conn, "viewer", first, true)?;
    crate::db::set_thread_pinned(&conn, "viewer", second, true)?;
    crate::db::set_thread_sticky(&conn, sticky, true)?;
    crate::db::set_thread_hidden(&conn, "viewer", last, true)?;
    let page = crate::db::get_threads_for_viewer(&conn, board, "viewer", false, 3, 0)?;
    ensure!(page.iter().map(|thread| thread.id).collect::<Vec<_>>() == [second, first, sticky]);
    let next = crate::db::get_threads_for_viewer(&conn, board, "viewer", false, 3, 3)?;
    ensure!(!next
        .iter()
        .any(|thread| [first, second, sticky, last].contains(&thread.id)));
    ensure!(crate::db::count_threads_for_viewer(&conn, board, "viewer", false)? == 204);
    ensure!(crate::db::get_threads_for_viewer(&conn, board, "viewer", false, -1, 0)?.len() == 204);
    crate::db::set_thread_pinned(&conn, "viewer", first, false)?;
    crate::db::set_thread_pinned(&conn, "viewer", second, false)?;
    let unpinned_page = crate::db::get_threads_for_viewer(&conn, board, "viewer", false, 1, 0)?;
    ensure!(unpinned_page.first().context("first page")?.id == sticky);
    let other = crate::db::get_threads_for_viewer(&conn, board, "other", false, -1, 0)?;
    ensure!(other.len() == 205 && other.first().context("other first")?.id == sticky);
    Ok(())
}

#[tokio::test]
async fn hide_reload_and_unhide_are_consistent_across_index_catalog_and_direct_thread() -> Result<()>
{
    let state = crate::test_support::app_state();
    let conn = state.db.get()?;
    let board = crate::db::create_board(&conn, "actions", "Actions", "", false)?;
    let (thread, _post) = seed_thread(&conn, board)?;
    let (second_thread, _second_post) = seed_thread(&conn, board)?;
    crate::templates::set_live_boards(crate::db::get_all_boards(&conn)?);
    let router = action_router(state);
    let before = request(&router, "GET", "/actions/catalog", String::new()).await?;
    let before_etag = before
        .headers()
        .get(header::ETAG)
        .context("catalog ETag")?
        .clone();
    ensure!(
        request(
            &router,
            "POST",
            "/actions/thread-preference",
            format!("thread_id={thread}&board=actions&action=hide&_csrf=csrf123")
        )
        .await?
        .status()
            == StatusCode::SEE_OTHER
    );
    ensure!(
        request(
            &router,
            "POST",
            "/actions/thread-preference",
            format!("thread_id={second_thread}&board=actions&action=hide&_csrf=csrf123")
        )
        .await?
        .status()
            == StatusCode::SEE_OTHER
    );
    crate::db::set_thread_pinned(&conn, &viewer_key(), thread, true)?;
    crate::db::set_thread_sticky(&conn, thread, true)?;
    crate::db::set_thread_locked(&conn, thread, true)?;
    conn.execute(
        "INSERT INTO posts (thread_id, board_id, body, body_html, deletion_token, is_op)
         VALUES (?1, ?2, 'hidden new reply', 'hidden new reply', 'reply', 0)",
        rusqlite::params![thread, board],
    )
    .map(|_rows| ())?;
    for path in ["/actions", "/actions/catalog"] {
        let response = request(&router, "GET", path, String::new()).await?;
        ensure!(response
            .headers()
            .get(header::CACHE_CONTROL)
            .context("cache policy")?
            .to_str()?
            .contains("private"));
        ensure!(response.headers().get(header::ETAG).context("ETag")? != before_etag);
        let page = html(response).await?;
        ensure!(
            !page.contains("action target") && !page.contains("hidden new reply"),
            "hidden OP and previews must leave listings together"
        );
        ensure!(page.contains("Hidden (2)") || page.contains("Hidden Threads: 2"));
    }
    let hidden = html(request(&router, "GET", "/actions/hidden", String::new()).await?).await?;
    ensure!(hidden.contains("action target") && hidden.contains("Unhide thread"));
    let direct = html(
        request(
            &router,
            "GET",
            &format!("/actions/thread/{thread}"),
            String::new(),
        )
        .await?,
    )
    .await?;
    ensure!(
        direct.contains("action target")
            && direct.contains("Unhide thread")
            && direct.contains("Unpin thread")
            && direct.contains("hidden new reply")
            && direct.contains("Hidden from your index")
    );
    ensure!(
        request(
            &router,
            "POST",
            "/actions/thread-preference",
            format!("thread_id={thread}&board=actions&action=unhide&_csrf=csrf123")
        )
        .await?
        .status()
            == StatusCode::SEE_OTHER
    );
    ensure!(
        html(request(&router, "GET", "/actions", String::new()).await?)
            .await?
            .contains("action target")
    );
    Ok(())
}

#[tokio::test]
async fn archived_targets_reject_new_preferences_but_allow_cleanup_and_deletion_cascades(
) -> Result<()> {
    let state = crate::test_support::app_state();
    let conn = state.db.get()?;
    let board = crate::db::create_board(&conn, "actions", "Actions", "", false)?;
    let (thread, _post) = seed_thread(&conn, board)?;
    crate::db::set_thread_hidden(&conn, &viewer_key(), thread, true)?;
    crate::db::set_thread_pinned(&conn, &viewer_key(), thread, true)?;
    ensure!(conn
        .execute("UPDATE user_thread_preferences SET hidden=2", [])
        .is_err());
    crate::db::set_thread_archived(&conn, thread, true)?;
    let router = action_router(state);
    for (action, status) in [
        ("pin", StatusCode::CONFLICT),
        ("hide", StatusCode::CONFLICT),
        ("unpin", StatusCode::SEE_OTHER),
        ("unhide", StatusCode::SEE_OTHER),
    ] {
        ensure!(
            request(
                &router,
                "POST",
                "/actions/thread-preference",
                format!("thread_id={thread}&board=actions&action={action}&_csrf=csrf123")
            )
            .await?
            .status()
                == status
        );
    }
    crate::db::set_thread_hidden(&conn, &viewer_key(), thread, true)?;
    conn.execute("DELETE FROM threads WHERE id=?1", [thread])
        .map(|_rows| ())?;
    ensure!(crate::db::get_thread_preference(&conn, &viewer_key(), thread)?.is_none());
    ensure!(
        request(
            &router,
            "POST",
            "/actions/thread-preference",
            format!("thread_id={thread}&board=actions&action=pin&_csrf=csrf123")
        )
        .await?
        .status()
            == StatusCode::NOT_FOUND
    );
    Ok(())
}

#[tokio::test]
async fn reports_validate_complete_targets_and_distinguish_duplicates() -> Result<()> {
    let state = crate::test_support::app_state();
    let conn = state.db.get()?;
    let board = crate::db::create_board(&conn, "actions", "Actions", "", false)?;
    let (thread, post_id) = seed_thread(&conn, board)?;
    let router = action_router(state);
    for (target, status) in [
        (
            format!("post_id={post_id}&thread_id={thread}&board=actions&_csrf=bad"),
            StatusCode::FORBIDDEN,
        ),
        (
            format!("post_id={post_id}&thread_id=999&board=actions&_csrf=csrf123"),
            StatusCode::BAD_REQUEST,
        ),
        (
            format!("post_id={post_id}&thread_id={thread}&board=act%2Fions&_csrf=csrf123"),
            StatusCode::BAD_REQUEST,
        ),
        (
            format!("post_id=-1&thread_id={thread}&board=actions&_csrf=csrf123"),
            StatusCode::BAD_REQUEST,
        ),
        (
            format!("post_id=999&thread_id={thread}&board=actions&_csrf=csrf123"),
            StatusCode::NOT_FOUND,
        ),
        (
            format!(
                "post_id={post_id}&thread_id={thread}&board=actions&reason=a%00b&_csrf=csrf123"
            ),
            StatusCode::BAD_REQUEST,
        ),
    ] {
        ensure!(request(&router, "POST", "/report", target).await?.status() == status);
    }
    for outcome in ["1", "duplicate"] {
        let response = request(
            &router,
            "POST",
            "/report",
            format!("post_id={post_id}&thread_id={thread}&board=actions&reason=&_csrf=csrf123"),
        )
        .await?;
        ensure!(response.status() == StatusCode::SEE_OTHER);
        ensure!(
            response
                .headers()
                .get(header::LOCATION)
                .context("report redirect")?
                .to_str()?
                == format!("/actions/thread/{thread}?reported={outcome}#p{post_id}")
        );
    }
    let reports: i64 = conn.query_row("SELECT COUNT(*) FROM reports", [], |row| row.get(0))?;
    ensure!(reports == 1);
    conn.execute("DELETE FROM threads WHERE id=?1", [thread])
        .map(|_rows| ())?;
    ensure!(crate::db::get_open_reports(&conn)?.is_empty());
    Ok(())
}

#[tokio::test]
async fn reports_bound_unicode_reasons_and_reject_oversized_requests_and_wrong_boards() -> Result<()>
{
    let state = crate::test_support::app_state();
    let conn = state.db.get()?;
    let board = crate::db::create_board(&conn, "actions", "Actions", "", false)?;
    let _other = crate::db::create_board(&conn, "other", "Other", "", false)?;
    let (thread, post_id) = seed_thread(&conn, board)?;
    let router = action_router(state);
    let fields = format!("post_id={post_id}&thread_id={thread}&board=actions&_csrf=csrf123");
    ensure!(
        request(
            &router,
            "POST",
            "/report",
            format!("{fields}&reason={}", "x".repeat(70_000))
        )
        .await?
        .status()
            == StatusCode::PAYLOAD_TOO_LARGE
    );
    ensure!(
        request(
            &router,
            "POST",
            "/report",
            format!("post_id={post_id}&thread_id={thread}&board=other&_csrf=csrf123")
        )
        .await?
        .status()
            == StatusCode::BAD_REQUEST
    );
    let reason = "日本語😀".repeat(100);
    ensure!(
        request(
            &router,
            "POST",
            "/report",
            format!("{fields}&reason={reason}")
        )
        .await?
        .status()
            == StatusCode::SEE_OTHER
    );
    let stored: String = conn.query_row("SELECT reason FROM reports", [], |row| row.get(0))?;
    ensure!(stored == reason.chars().take(256).collect::<String>());
    ensure!(stored.chars().count() == 256);
    Ok(())
}

#[tokio::test]
async fn report_budget_is_persistent_and_duplicate_does_not_consume_it() -> Result<()> {
    let state = crate::test_support::app_state();
    let conn = state.db.get()?;
    let board = crate::db::create_board(&conn, "actions", "Actions", "", false)?;
    let mut targets = Vec::new();
    for _number in 0_i32..21_i32 {
        targets.push(seed_thread(&conn, board)?);
    }
    for (_thread, post_id) in targets.iter().take(20) {
        let _submission = crate::db::file_report(&conn, *post_id, "", &viewer_key())?;
    }
    let router = action_router(state);
    let (thread, post_id) = targets.last().copied().context("last target")?;
    let response = request(
        &router,
        "POST",
        "/report",
        format!("post_id={post_id}&thread_id={thread}&board=actions&_csrf=csrf123"),
    )
    .await?;
    ensure!(response.status() == StatusCode::TOO_MANY_REQUESTS);
    ensure!(
        response
            .headers()
            .get(header::RETRY_AFTER)
            .context("retry header")?
            == "3600"
    );
    let (duplicate_thread, duplicate_post) = targets.first().copied().context("first target")?;
    ensure!(
        request(
            &router,
            "POST",
            "/report",
            format!(
                "post_id={duplicate_post}&thread_id={duplicate_thread}&board=actions&_csrf=csrf123"
            )
        )
        .await?
        .status()
            == StatusCode::SEE_OTHER
    );
    Ok(())
}

#[test]
fn menu_csrf_rejects_foreign_signed_tokens_and_preserves_cookie_disabled_same_origin_forms(
) -> Result<()> {
    use axum_extra::extract::cookie::{Cookie, CookieJar};
    let jar = CookieJar::new().add(Cookie::new("csrf_token", "viewer"));
    let own =
        crate::utils::crypto::make_csrf_form_token("viewer", &crate::config::CONFIG.cookie_secret);
    let foreign = crate::utils::crypto::make_csrf_form_token(
        "attacker",
        &crate::config::CONFIG.cookie_secret,
    );
    let mut headers = axum::http::HeaderMap::new();
    let _old_host = headers.insert(
        header::HOST,
        axum::http::HeaderValue::from_static("example.test"),
    );
    let _old_origin = headers.insert(
        header::ORIGIN,
        axum::http::HeaderValue::from_static("http://example.test"),
    );
    let peer = crate::middleware::SecureCookieContext::default();
    ensure!(super::check_menu_action_csrf(&jar, &headers, peer, Some(&own)).is_ok());
    ensure!(super::check_menu_action_csrf(&jar, &headers, peer, Some(&foreign)).is_err());
    ensure!(
        super::check_menu_action_csrf(&CookieJar::new(), &headers, peer, Some(&foreign)).is_ok()
    );
    let _same_origin = headers.insert(
        header::ORIGIN,
        axum::http::HeaderValue::from_static("http://evil.test"),
    );
    ensure!(super::check_menu_action_csrf(&jar, &headers, peer, Some(&own)).is_err());
    ensure!(
        super::check_menu_action_csrf(&CookieJar::new(), &headers, peer, Some(&foreign)).is_err()
    );
    let _origin = headers.remove(header::ORIGIN);
    ensure!(super::check_menu_action_csrf(&jar, &headers, peer, Some(&own)).is_ok());
    ensure!(
        super::check_menu_action_csrf(&CookieJar::new(), &headers, peer, Some(&foreign)).is_err()
    );
    let _privacy_origin =
        headers.insert(header::ORIGIN, axum::http::HeaderValue::from_static("null"));
    ensure!(super::check_menu_action_csrf(&jar, &headers, peer, Some(&own)).is_ok());
    ensure!(
        super::check_menu_action_csrf(&CookieJar::new(), &headers, peer, Some(&foreign)).is_err()
    );
    let _same_site = headers.insert(
        "sec-fetch-site",
        axum::http::HeaderValue::from_static("same-origin"),
    );
    ensure!(
        super::check_menu_action_csrf(&CookieJar::new(), &headers, peer, Some(&foreign)).is_ok()
    );
    let _old_site = headers.insert(
        "sec-fetch-site",
        axum::http::HeaderValue::from_static("cross-site"),
    );
    ensure!(super::check_menu_action_csrf(&jar, &headers, peer, Some(&own)).is_err());
    Ok(())
}

#[tokio::test]
async fn concurrent_menu_requests_preserve_flags_deduplicate_reports_and_cascade_deleted_targets(
) -> Result<()> {
    let state = crate::test_support::app_state();
    let conn = state.db.get()?;
    let board = crate::db::create_board(&conn, "actions", "Actions", "", false)?;
    let (thread, post_id) = seed_thread(&conn, board)?;
    let router = action_router(state.clone());
    let (pin, hide) = tokio::join!(
        request(
            &router,
            "POST",
            "/actions/thread-preference",
            format!("thread_id={thread}&board=actions&action=pin&_csrf=csrf123")
        ),
        request(
            &router,
            "POST",
            "/actions/thread-preference",
            format!("thread_id={thread}&board=actions&action=hide&_csrf=csrf123")
        ),
    );
    ensure!(pin?.status() == StatusCode::SEE_OTHER && hide?.status() == StatusCode::SEE_OTHER);
    let preference =
        crate::db::get_thread_preference(&conn, &viewer_key(), thread)?.context("preference")?;
    ensure!(preference.pinned && preference.hidden);
    let report_body = format!("post_id={post_id}&thread_id={thread}&board=actions&_csrf=csrf123");
    let (first_report, second_report) = tokio::join!(
        request(&router, "POST", "/report", report_body.clone()),
        request(&router, "POST", "/report", report_body),
    );
    ensure!(
        first_report?.status() == StatusCode::SEE_OTHER
            && second_report?.status() == StatusCode::SEE_OTHER
    );
    let reports: i64 = conn.query_row("SELECT COUNT(*) FROM reports", [], |row| row.get(0))?;
    ensure!(reports == 1);
    let delete_pool = state.db.clone();
    let (delete, unhide) = tokio::join!(
        tokio::task::spawn_blocking(move || -> Result<()> {
            let deletion_conn = delete_pool.get()?;
            deletion_conn
                .execute("DELETE FROM threads WHERE id=?1", [thread])
                .map(|_rows| ())?;
            Ok(())
        }),
        request(
            &router,
            "POST",
            "/actions/thread-preference",
            format!("thread_id={thread}&board=actions&action=unhide&_csrf=csrf123")
        ),
    );
    delete??;
    ensure!([StatusCode::SEE_OTHER, StatusCode::NOT_FOUND].contains(&unhide?.status()));
    ensure!(crate::db::get_thread_preference(&conn, &viewer_key(), thread)?.is_none());
    ensure!(crate::db::get_open_reports(&conn)?.is_empty());
    Ok(())
}
