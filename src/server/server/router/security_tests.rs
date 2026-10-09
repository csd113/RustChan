//! Hostile requests exercise the assembled application, not only validators.

use super::build_router;
use crate::middleware::AppState;
use anyhow::{ensure, Context as _};
use axum::{
    body::{to_bytes, Body},
    http::{header, Request, StatusCode},
    Router,
};
use std::net::SocketAddr;
use tower::ServiceExt as _;

/// Build isolated board state while retaining the production middleware stack.
fn fixture() -> anyhow::Result<(AppState, Router)> {
    let state = crate::test_support::app_state();
    let _board_id = crate::db::create_board(&*state.db.get()?, "audit", "Audit", "", false)?;
    let router = build_router(state.clone(), false);
    Ok((state, router))
}

/// Give each scenario a distinct direct peer; forged headers cannot change it.
fn request(method: &str, path: &str, scenario: u8, body: Body) -> anyhow::Result<Request<Body>> {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header(header::HOST, "localhost")
        .header(header::ORIGIN, "http://localhost")
        .header(header::COOKIE, "csrf_token=csrf123")
        .body(body)?;
    let _previous_peer =
        request
            .extensions_mut()
            .insert(axum::extract::ConnectInfo(SocketAddr::from((
                [192, 0, 2, scenario],
                41000,
            ))));
    Ok(request)
}

#[tokio::test]
async fn posting_bursts_are_limited_without_censoring_identical_posts() -> anyhow::Result<()> {
    let (state, router) = fixture()?;
    for attempt in 0u8..7 {
        let (boundary, bytes) = crate::test_support::multipart_body(
            &[("_csrf", "csrf123"), ("body", "identical legitimate body")],
            None,
        );
        let mut req = request("POST", "/audit", 201, Body::from(bytes))?;
        drop(req.headers_mut().insert(
            header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={boundary}").parse()?,
        ));
        drop(
            req.headers_mut()
                .insert("x-forwarded-for", format!("203.0.113.{attempt}").parse()?),
        );
        let response = router.clone().oneshot(req).await?;
        ensure!(
            response.status()
                == if attempt < 6 {
                    StatusCode::SEE_OTHER
                } else {
                    StatusCode::TOO_MANY_REQUESTS
                },
            "posting attempt {attempt}: {}",
            response.status()
        );
    }
    let conn = state.db.get()?;
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM posts", [], |row| row.get(0))?;
    ensure!(
        count == 6,
        "separate identical posts should survive, rejected requests must not mutate"
    );
    drop(conn);
    let response = router
        .oneshot(request("GET", "/audit", 201, Body::empty())?)
        .await?;
    ensure!(
        response.status() == StatusCode::OK,
        "posting budget must not disable ordinary reads"
    );
    Ok(())
}

#[tokio::test]
async fn failed_reports_and_expensive_searches_have_separate_budgets() -> anyhow::Result<()> {
    let (_state, router) = fixture()?;
    for attempt in 0u8..31 {
        let mut req = request(
            "POST",
            "/report",
            202,
            Body::from("board=audit&post_id=0&thread_id=0&_csrf=csrf123"),
        )?;
        drop(req.headers_mut().insert(
            header::CONTENT_TYPE,
            "application/x-www-form-urlencoded".parse()?,
        ));
        let response = router.clone().oneshot(req).await?;
        ensure!(
            response.status()
                == if attempt < 30 {
                    StatusCode::BAD_REQUEST
                } else {
                    StatusCode::TOO_MANY_REQUESTS
                },
            "report attempt {attempt}: {}",
            response.status()
        );
    }
    for attempt in 0u8..21 {
        let response = router
            .clone()
            .oneshot(request(
                "GET",
                "/audit/search?q=ordinary",
                203,
                Body::empty(),
            )?)
            .await?;
        ensure!(
            response.status()
                == if attempt < 20 {
                    StatusCode::OK
                } else {
                    StatusCode::TOO_MANY_REQUESTS
                },
            "search attempt {attempt}: {}",
            response.status()
        );
    }
    ensure!(
        router
            .oneshot(request("GET", "/audit", 203, Body::empty())?)
            .await?
            .status()
            == StatusCode::OK
    );
    Ok(())
}

#[tokio::test]
async fn form_and_uri_limits_reject_before_expensive_work() -> anyhow::Result<()> {
    let (_state, router) = fixture()?;
    for bytes in [65_535, 65_536, 65_537] {
        let mut req = request("POST", "/vote", 204, Body::from("x".repeat(bytes)))?;
        drop(req.headers_mut().insert(
            header::CONTENT_TYPE,
            "application/x-www-form-urlencoded".parse()?,
        ));
        let response = router.clone().oneshot(req).await?;
        ensure!(
            response.status()
                == if bytes > 65_536 {
                    StatusCode::PAYLOAD_TOO_LARGE
                } else {
                    StatusCode::UNPROCESSABLE_ENTITY
                },
            "body boundary {bytes}: {}",
            response.status()
        );
    }
    for path in ["/vote", "/report", "/appeal", "/preferences"] {
        let mut declared_form = request("POST", path, 204, Body::empty())?;
        drop(
            declared_form
                .headers_mut()
                .insert(header::CONTENT_LENGTH, "65537".parse()?),
        );
        ensure!(
            router.clone().oneshot(declared_form).await?.status() == StatusCode::PAYLOAD_TOO_LARGE,
            "form routes must not inherit upload allowances: {path}"
        );
    }
    for length in [8191usize, 8192, 8193] {
        let path = format!("/{}", "x".repeat(length.saturating_sub(1)));
        let response = router
            .clone()
            .oneshot(request("GET", &path, 205, Body::empty())?)
            .await?;
        ensure!(
            response.status()
                == if length > 8192 {
                    StatusCode::URI_TOO_LONG
                } else {
                    StatusCode::NOT_FOUND
                },
            "URI boundary {length}: {}",
            response.status()
        );
    }
    let mut req = request("POST", "/audit", 206, Body::empty())?;
    drop(
        req.headers_mut()
            .insert(header::CONTENT_LENGTH, "541065217".parse()?),
    );
    ensure!(router.oneshot(req).await?.status() == StatusCode::PAYLOAD_TOO_LARGE);
    Ok(())
}

#[tokio::test]
async fn malformed_hosts_ids_and_cross_site_requests_fail_safely() -> anyhow::Result<()> {
    let (_state, router) = fixture()?;
    for host in [
        "localhost@attacker.invalid",
        "localhost\\attacker.invalid",
        "localhost:abc",
        "localhost:65536",
    ] {
        let mut req = request("GET", "/audit", 207, Body::empty())?;
        drop(req.headers_mut().insert(header::HOST, host.parse()?));
        ensure!(
            router.clone().oneshot(req).await?.status() == StatusCode::BAD_REQUEST,
            "malformed Host {host}"
        );
    }
    for id in [
        "-1",
        "0",
        "9223372036854775807",
        "9223372036854775808",
        "not-an-id",
    ] {
        let response = router
            .clone()
            .oneshot(request(
                "GET",
                &format!("/audit/thread/{id}"),
                208,
                Body::empty(),
            )?)
            .await?;
        ensure!(
            matches!(
                response.status(),
                StatusCode::BAD_REQUEST | StatusCode::NOT_FOUND
            ),
            "malformed ID {id}: {}",
            response.status()
        );
    }
    let mut req = request("POST", "/audit", 209, Body::empty())?;
    drop(
        req.headers_mut()
            .insert(header::ORIGIN, "https://attacker.invalid".parse()?),
    );
    ensure!(router.clone().oneshot(req).await?.status() == StatusCode::FORBIDDEN);
    let mut cross_site_request = request("POST", "/report", 209, Body::empty())?;
    drop(
        cross_site_request
            .headers_mut()
            .insert("sec-fetch-site", "cross-site".parse()?),
    );
    ensure!(router.oneshot(cross_site_request).await?.status() == StatusCode::FORBIDDEN);
    Ok(())
}

#[tokio::test]
async fn search_rejects_overlong_unicode_and_huge_pages() -> anyhow::Result<()> {
    let (_state, router) = fixture()?;
    for path in [
        "/audit/search?q=word&page=9223372036854775807".to_owned(),
        format!(
            "/audit/search?q={}",
            crate::utils::redirect::encode_query_component(&"😀".repeat(257))
        ),
    ] {
        ensure!(
            router
                .clone()
                .oneshot(request("GET", &path, 210, Body::empty())?)
                .await?
                .status()
                == StatusCode::BAD_REQUEST
        );
    }
    let path = format!(
        "/audit/search?q={}",
        crate::utils::redirect::encode_query_component("<script>\"&e\u{0301}\u{202e}")
    );
    let response = router
        .oneshot(request("GET", &path, 210, Body::empty())?)
        .await?;
    ensure!(response.status() == StatusCode::OK);
    let html = String::from_utf8(
        to_bytes(response.into_body(), 2 * 1024 * 1024)
            .await?
            .to_vec(),
    )?;
    ensure!(
        html.contains("&lt;script&gt;"),
        "search echo must escape tags"
    );
    ensure!(!html.contains("<script>"));
    Ok(())
}

#[tokio::test]
async fn unauthenticated_admin_upload_is_rejected_before_reading_body() -> anyhow::Result<()> {
    let (_state, router) = fixture()?;
    let body = Body::from_stream(futures::stream::pending::<
        Result<axum::body::Bytes, std::io::Error>,
    >());
    let mut req = request("POST", "/admin/site/favicon", 211, body)?;
    drop(req.headers_mut().insert(
        header::CONTENT_TYPE,
        "multipart/form-data; boundary=audit-boundary".parse()?,
    ));
    let response = tokio::time::timeout(std::time::Duration::from_secs(2), router.oneshot(req))
        .await
        .context("unauthenticated upload read a stalled body")??;
    ensure!(response.status() == StatusCode::FORBIDDEN);
    Ok(())
}

#[tokio::test]
async fn thread_history_and_quote_destinations_work_without_javascript() -> anyhow::Result<()> {
    let (state, router) = fixture()?;
    let conn = state.db.get()?;
    let board = crate::db::get_board_by_short(&conn, "audit")?.context("board exists")?;
    conn.execute("INSERT INTO threads(id,board_id) VALUES(1,?1)", [board.id])
        .map(|_count| ())?;
    conn.execute("INSERT INTO posts(id,thread_id,board_id,body,body_html,deletion_token,is_op) VALUES(1,1,?1,'OP','OP','delete',1)", [board.id]).map(|_count| ())?;
    conn.execute("WITH RECURSIVE n(x) AS (SELECT 2 UNION ALL SELECT x+1 FROM n WHERE x<402) INSERT INTO posts(id,thread_id,board_id,body,body_html,deletion_token) SELECT x,1,?1,'reply','reply','delete' FROM n", [board.id]).map(|_count| ())?;
    conn.execute(
        "UPDATE posts SET body_html=?1 WHERE id=402",
        [crate::utils::sanitize::render_post_body("&gt;&gt;2", false)],
    )
    .map(|_count| ())?;
    drop(conn);
    let response = router
        .clone()
        .oneshot(request("GET", "/audit/thread/1", 212, Body::empty())?)
        .await?;
    ensure!(response.status() == StatusCode::OK);
    let html = String::from_utf8(
        to_bytes(response.into_body(), 8 * 1024 * 1024)
            .await?
            .to_vec(),
    )?;
    ensure!(html.contains("Older replies") && html.contains("/audit/post/2"));
    ensure!(!html.contains("id=\"p2\""));
    let quote_redirect = router
        .clone()
        .oneshot(request("GET", "/audit/post/2", 212, Body::empty())?)
        .await?;
    let destination = quote_redirect
        .headers()
        .get(header::LOCATION)
        .context("quote redirect")?
        .to_str()?
        .to_owned();
    ensure!(destination == "/audit/thread/1?post=2#p2");
    let historical_response = router
        .clone()
        .oneshot(request(
            "GET",
            "/audit/thread/1?post=2",
            212,
            Body::empty(),
        )?)
        .await?;
    let historical_html = String::from_utf8(
        to_bytes(historical_response.into_body(), 8 * 1024 * 1024)
            .await?
            .to_vec(),
    )?;
    ensure!(historical_html.contains("id=\"p2\"") && historical_html.contains("Latest replies"));
    let max_id_conn = state.db.get()?;
    max_id_conn.execute("INSERT INTO posts(id,thread_id,board_id,body,body_html,deletion_token) VALUES(?1,1,?2,'last ID','last ID','delete')", [i64::MAX, board.id]).map(|_count| ())?;
    drop(max_id_conn);
    let focused_response = router
        .oneshot(request(
            "GET",
            "/audit/thread/1?post=9223372036854775807",
            212,
            Body::empty(),
        )?)
        .await?;
    ensure!(focused_response.status() == StatusCode::OK);
    let focused_html = String::from_utf8(
        to_bytes(focused_response.into_body(), 8 * 1024 * 1024)
            .await?
            .to_vec(),
    )?;
    ensure!(focused_html.contains("id=\"p9223372036854775807\""));
    Ok(())
}

#[tokio::test]
async fn streaming_responses_hold_request_capacity_until_dropped() -> anyhow::Result<()> {
    let (mut state, _router) = fixture()?;
    state.request_work_gate = crate::middleware::WorkGate::new(2);
    let router = build_router(state, false);
    let first = router
        .clone()
        .oneshot(request("GET", "/audit", 213, Body::empty())?)
        .await?;
    let second = router
        .clone()
        .oneshot(request("GET", "/audit", 213, Body::empty())?)
        .await?;
    ensure!(first.status() == StatusCode::OK && second.status() == StatusCode::OK);
    ensure!(
        router
            .clone()
            .oneshot(request("GET", "/audit", 213, Body::empty())?)
            .await?
            .status()
            == StatusCode::SERVICE_UNAVAILABLE
    );
    drop(first);
    ensure!(
        router
            .oneshot(request("GET", "/audit", 213, Body::empty())?)
            .await?
            .status()
            == StatusCode::OK
    );
    drop(second);
    Ok(())
}

#[tokio::test]
async fn admin_asset_fields_are_bounded_and_duplicate_parts_are_rejected() -> anyhow::Result<()> {
    let (state, router) = fixture()?;
    let conn = state.db.get()?;
    let admin = crate::db::create_admin(&conn, "asset-admin", "fixture-hash")?;
    crate::db::create_session(&conn, "asset-session", admin, i64::MAX)?;
    drop(conn);
    for size in [65_535, 65_536, 65_537] {
        let (boundary, body) =
            crate::test_support::multipart_body(&[("_csrf", &"x".repeat(size))], None);
        let mut req = request("POST", "/admin/site/favicon", 214, Body::from(body))?;
        drop(req.headers_mut().insert(
            header::COOKIE,
            "csrf_token=csrf123; chan_admin_session=asset-session".parse()?,
        ));
        drop(req.headers_mut().insert(
            header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={boundary}").parse()?,
        ));
        let response = router.clone().oneshot(req).await?;
        ensure!(
            response.status()
                == if size > 65_536 {
                    StatusCode::PAYLOAD_TOO_LARGE
                } else {
                    StatusCode::FORBIDDEN
                },
            "admin field boundary {size}: {}",
            response.status()
        );
    }
    let (boundary, body) =
        crate::test_support::multipart_body(&[("_csrf", "a"), ("_csrf", "b")], None);
    let mut req = request("POST", "/admin/home/banner", 214, Body::from(body))?;
    drop(req.headers_mut().insert(
        header::COOKIE,
        "csrf_token=csrf123; chan_admin_session=asset-session".parse()?,
    ));
    drop(req.headers_mut().insert(
        header::CONTENT_TYPE,
        format!("multipart/form-data; boundary={boundary}").parse()?,
    ));
    ensure!(router.oneshot(req).await?.status() == StatusCode::BAD_REQUEST);
    ensure!(
        state.media_upload_gate.try_begin().is_ok(),
        "rejected assets must release media admission"
    );
    Ok(())
}
