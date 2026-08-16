use axum::{
    body::{to_bytes, Body},
    http::{header, HeaderMap, Request, StatusCode},
    response::IntoResponse as _,
    routing::{get, post},
    Router,
};
use axum_extra::extract::cookie::CookieJar;
use std::collections::HashMap;
use tower::ServiceExt as _;

use anyhow::Context as _;

macro_rules! ensure_eq {
    ($left:expr_2021, $right:expr_2021 $(,)?) => {{
        match (&$left, &$right) {
            (left, right) => anyhow::ensure!(
                *left == *right,
                "assertion failed: `(left == right)`\n  left: `{left:?}`\n right: `{right:?}`"
            ),
        }
    }};
    ($left:expr_2021, $right:expr_2021, $($message:tt)+) => {{
        match (&$left, &$right) {
            (left, right) => anyhow::ensure!(
                *left == *right,
                "assertion failed: `(left == right)`\n  left: `{left:?}`\n right: `{right:?}`: {}",
                format_args!($($message)+)
            ),
        }
    }};
}

macro_rules! ensure_ne {
    ($left:expr_2021, $right:expr_2021 $(,)?) => {{
        match (&$left, &$right) {
            (left, right) => anyhow::ensure!(
                *left != *right,
                "assertion failed: `(left != right)`\n  left: `{left:?}`\n right: `{right:?}`"
            ),
        }
    }};
}

fn seed_post_password_board(
    state: &crate::middleware::AppState,
) -> anyhow::Result<(i64, i64, i64)> {
    let conn = state.db.get().context("db connection")?;
    let board_id =
        crate::db::create_board(&conn, "secret", "Secret", "", false).context("create board")?;
    let password_hash =
        crate::utils::crypto::hash_password("swordfish").context("hash password")?;
    conn.execute(
        "UPDATE boards SET access_mode = ?1, access_password_hash = ?2, allow_editing = 1, allow_self_delete = 1 WHERE id = ?3",
        rusqlite::params!["post_password", password_hash, board_id],
    )
    .context("update board access")?;
    let post = crate::db::NewPost {
        thread_id: 0,
        board_id,
        name: "anon".to_owned(),
        tripcode: None,
        subject: Some("subject".to_owned()),
        body: "protected posting body".to_owned(),
        body_html: "protected posting body".to_owned(),
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
        deletion_token: "edit-token".to_owned(),
        is_op: true,
    };
    let poll = crate::db::threads::PollInsert {
        question: "pick one",
        options: &["yes".to_owned(), "no".to_owned()],
        expires_at: chrono::Utc::now().timestamp() + 3600,
    };
    let (thread_id, post_id, poll_id) = crate::db::create_thread_with_optional_poll(
        &conn,
        board_id,
        Some("subject"),
        &post,
        "",
        Some(&poll),
        None,
    )
    .context("create thread")?;
    let option_id: i64 = conn
        .query_row(
            "SELECT id FROM poll_options WHERE poll_id = ?1 ORDER BY id LIMIT 1",
            rusqlite::params![poll_id.context("poll id")?],
            |row| row.get(0),
        )
        .context("poll option id")?;
    Ok((thread_id, post_id, option_id))
}

fn set_new_activity_settings(
    state: &crate::middleware::AppState,
    homepage_thread_enabled: bool,
    homepage_reply_enabled: bool,
    thread_enabled: bool,
) -> anyhow::Result<()> {
    let conn = state.db.get().context("db connection")?;
    crate::db::set_site_setting(
        &conn,
        "homepage_new_thread_badges_enabled",
        if homepage_thread_enabled { "1" } else { "0" },
    )
    .context("set homepage activity setting")?;
    crate::db::set_site_setting(
        &conn,
        "homepage_new_reply_badges_enabled",
        if homepage_reply_enabled { "1" } else { "0" },
    )
    .context("set homepage reply activity setting")?;
    crate::db::set_site_setting(
        &conn,
        "thread_new_reply_badges_enabled",
        if thread_enabled { "1" } else { "0" },
    )
    .context("set thread activity setting")?;
    Ok(())
}

fn install_preference_test_themes() {
    crate::templates::set_live_default_theme("forest");
    crate::templates::set_live_themes(vec![
        crate::models::Theme {
            slug: "forest".to_owned(),
            display_name: "Forest".to_owned(),
            description: "Forest theme".to_owned(),
            swatch_hex: "#123456".to_owned(),
            enabled: true,
            sort_order: 10,
            is_builtin: true,
            custom_css: String::new(),
        },
        crate::models::Theme {
            slug: "blue-sky".to_owned(),
            display_name: "Blue Sky".to_owned(),
            description: "Blue Sky theme".to_owned(),
            swatch_hex: "#87ceeb".to_owned(),
            enabled: true,
            sort_order: 20,
            is_builtin: true,
            custom_css: String::new(),
        },
    ]);
}

fn seed_board_with_thread(
    state: &crate::middleware::AppState,
    short_name: &str,
    body: &str,
) -> anyhow::Result<(i64, i64)> {
    let conn = state.db.get().context("db connection")?;
    let board_id =
        crate::db::create_board(&conn, short_name, "Board", "", false).context("create board")?;
    crate::templates::set_live_boards(crate::db::get_all_boards(&conn).context("load boards")?);
    let post = crate::db::NewPost {
        thread_id: 0,
        board_id,
        name: "anon".to_owned(),
        tripcode: None,
        subject: Some("subject".to_owned()),
        body: body.to_owned(),
        body_html: body.to_owned(),
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
        deletion_token: "token".to_owned(),
        is_op: true,
    };
    let (thread_id, _post_id, _) =
        crate::db::create_thread_with_optional_poll(&conn, board_id, None, &post, "", None, None)
            .context("create thread")?;
    Ok((board_id, thread_id))
}

fn create_thread_on_board(
    state: &crate::middleware::AppState,
    board_id: i64,
    body: &str,
) -> anyhow::Result<i64> {
    let conn = state.db.get().context("db connection")?;
    let post = crate::db::NewPost {
        thread_id: 0,
        board_id,
        name: "anon".to_owned(),
        tripcode: None,
        subject: Some("subject".to_owned()),
        body: body.to_owned(),
        body_html: body.to_owned(),
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
        deletion_token: "token".to_owned(),
        is_op: true,
    };
    let (thread_id, _post_id, _) =
        crate::db::create_thread_with_optional_poll(&conn, board_id, None, &post, "", None, None)
            .context("create thread")?;
    Ok(thread_id)
}

fn create_reply_on_thread(
    state: &crate::middleware::AppState,
    board_id: i64,
    thread_id: i64,
    body: &str,
) -> anyhow::Result<()> {
    let conn = state.db.get().context("db connection")?;
    let reply = crate::db::NewPost {
        thread_id,
        board_id,
        name: "anon".to_owned(),
        tripcode: None,
        subject: None,
        body: body.to_owned(),
        body_html: body.to_owned(),
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
        deletion_token: "token".to_owned(),
        is_op: false,
    };
    crate::db::create_reply_with_thread_update(&conn, &reply, "", true, None)
        .context("create reply")?;
    Ok(())
}

fn activity_router(state: crate::middleware::AppState) -> Router {
    Router::new()
        .route("/", get(super::index))
        .route("/{board}", get(super::board_index))
        .route("/{board}/catalog", get(super::catalog))
        .route(
            "/{board}/thread/{id}",
            get(crate::handlers::thread::view_thread),
        )
        .route(
            "/{board}/thread/{id}/updates",
            get(crate::handlers::thread::thread_updates),
        )
        .with_state(state)
}

fn update_cookie_store(store: &mut HashMap<String, String>, headers: &HeaderMap) {
    for value in &headers.get_all(header::SET_COOKIE) {
        let Ok(value) = value.to_str() else {
            continue;
        };
        let Some((name, cookie_value)) = value
            .split(';')
            .next()
            .and_then(|pair| pair.split_once('='))
        else {
            continue;
        };
        if cookie_value.is_empty() {
            store.remove(name);
        } else {
            store.insert(name.to_owned(), cookie_value.to_owned());
        }
    }
}

fn cookie_header(store: &HashMap<String, String>) -> Option<String> {
    if store.is_empty() {
        return None;
    }
    let mut cookies = store
        .iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect::<Vec<_>>();
    cookies.sort();
    Some(cookies.join("; "))
}

#[test]
fn activity_restore_js_uses_explicit_page_markers() -> anyhow::Result<()> {
    let js = include_str!("../../../static/main.js");

    anyhow::ensure!(js.contains("document.querySelector('[data-activity-page]')"));
    anyhow::ensure!(js.contains("pageHasActivityLifecycle()"));
    anyhow::ensure!(!js.contains("document.querySelector('.board-index-header')"));
    Ok(())
}

#[test]
fn board_activity_cookie_removal_keeps_root_path_attributes() -> anyhow::Result<()> {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::COOKIE,
        "rustchan_board_activity=v1|1.100.1.200"
            .parse()
            .context("cookie header")?,
    );
    let jar = CookieJar::from_headers(&headers);
    let jar = super::prune_board_activity_markers(jar, &std::collections::HashSet::new());
    let response = (jar, StatusCode::NO_CONTENT).into_response();
    let set_cookie = response
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find(|value| value.starts_with("rustchan_board_activity="))
        .context("board activity removal cookie")?;

    anyhow::ensure!(set_cookie.contains("Path=/"));
    anyhow::ensure!(set_cookie.contains("SameSite=Lax"));
    anyhow::ensure!(set_cookie.contains("Max-Age=0"));
    Ok(())
}

#[test]
fn thread_activity_cookie_removal_keeps_root_path_attributes() -> anyhow::Result<()> {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::COOKIE,
        "rustchan_thread_activity=v1|1.2.200"
            .parse()
            .context("cookie header")?,
    );
    let jar = CookieJar::from_headers(&headers);
    let jar = super::remember_visible_thread_activity(jar, std::iter::empty());
    let response = (jar, StatusCode::NO_CONTENT).into_response();
    let set_cookie = response
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find(|value| value.starts_with("rustchan_thread_activity="))
        .context("thread activity removal cookie")?;

    anyhow::ensure!(set_cookie.contains("Path=/"));
    anyhow::ensure!(set_cookie.contains("SameSite=Lax"));
    anyhow::ensure!(set_cookie.contains("Max-Age=0"));
    Ok(())
}

async fn response_body_string(response: axum::response::Response) -> anyhow::Result<String> {
    String::from_utf8(
        to_bytes(response.into_body(), usize::MAX)
            .await
            .context("response body")?
            .to_vec(),
    )
    .context("utf8 body")
}

#[test]
fn protected_board_without_password_hash_fails_closed() -> anyhow::Result<()> {
    let board = crate::models::Board {
        access_mode: crate::models::BoardAccessMode::ViewPassword,
        access_password_hash: String::new(),
        ..crate::test_fixtures::sample_board()
    };
    anyhow::ensure!(!super::can_view_board(&board, false, None));
    anyhow::ensure!(!super::can_post_to_board(&board, false, None));
    Ok(())
}

#[tokio::test]
async fn post_password_board_remains_viewable_without_unlock() -> anyhow::Result<()> {
    let state = crate::test_support::app_state();
    let (thread_id, _, _) = seed_post_password_board(&state)?;

    let router = Router::new()
        .route("/{board}", get(super::board_index))
        .route("/{board}/catalog", get(super::catalog))
        .route(
            "/{board}/thread/{id}",
            get(crate::handlers::thread::view_thread),
        )
        .with_state(state);

    for uri in [
        "/secret".to_owned(),
        "/secret/catalog".to_owned(),
        format!("/secret/thread/{thread_id}"),
    ] {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(uri)
                    .extension(crate::test_support::connect_info())
                    .body(Body::empty())
                    .context("request")?,
            )
            .await
            .context("response")?;
        ensure_eq!(response.status(), StatusCode::OK);
    }
    Ok(())
}

#[expect(
    clippy::too_many_lines,
    reason = "the end-to-end test keeps its fixture setup and ordered assertions in one scenario"
)]
#[tokio::test]
async fn post_password_board_write_actions_require_unlock() -> anyhow::Result<()> {
    let state = crate::test_support::app_state();
    let (thread_id, post_id, option_id) = seed_post_password_board(&state)?;
    let router = Router::new()
        .route("/{board}", post(super::create_thread))
        .route(
            "/{board}/thread/{id}",
            post(crate::handlers::thread::post_reply),
        )
        .route(
            "/{board}/post/{id}/edit",
            get(crate::handlers::thread::edit_post_get),
        )
        .route(
            "/{board}/post/{id}/edit",
            post(crate::handlers::thread::edit_post_post),
        )
        .route(
            "/{board}/post/{id}/delete",
            get(crate::handlers::thread::delete_post_get),
        )
        .route(
            "/{board}/post/{id}/delete",
            post(crate::handlers::thread::delete_own_post),
        )
        .route("/vote", post(crate::handlers::thread::vote_handler))
        .with_state(state);

    let (boundary, body) =
        crate::test_support::multipart_body(&[("_csrf", "csrf123"), ("body", "new thread")], None);
    let create_response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/secret")
                .header(
                    header::CONTENT_TYPE,
                    format!("multipart/form-data; boundary={boundary}"),
                )
                .header(header::COOKIE, "csrf_token=csrf123")
                .extension(crate::test_support::connect_info())
                .body(Body::from(body))
                .context("request")?,
        )
        .await
        .context("create response")?;
    ensure_eq!(create_response.status(), StatusCode::SEE_OTHER);
    ensure_eq!(
        create_response
            .headers()
            .get(header::LOCATION)
            .and_then(|value| value.to_str().ok()),
        Some("/secret/unlock?return_to=%2Fsecret")
    );

    let (boundary, body) =
        crate::test_support::multipart_body(&[("_csrf", "csrf123"), ("body", "reply")], None);
    let reply_response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/secret/thread/{thread_id}"))
                .header(
                    header::CONTENT_TYPE,
                    format!("multipart/form-data; boundary={boundary}"),
                )
                .header(header::COOKIE, "csrf_token=csrf123")
                .extension(crate::test_support::connect_info())
                .body(Body::from(body))
                .context("request")?,
        )
        .await
        .context("reply response")?;
    ensure_eq!(reply_response.status(), StatusCode::SEE_OTHER);
    ensure_eq!(
        reply_response
            .headers()
            .get(header::LOCATION)
            .and_then(|value| value.to_str().ok()),
        Some(format!("/secret/unlock?return_to=%2Fsecret%2Fthread%2F{thread_id}").as_str())
    );

    let edit_get_response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/secret/post/{post_id}/edit"))
                .body(Body::empty())
                .context("request")?,
        )
        .await
        .context("edit get response")?;
    ensure_eq!(edit_get_response.status(), StatusCode::FORBIDDEN);

    let edit_post_response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/secret/post/{post_id}/edit"))
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .header(
                    header::COOKIE,
                    format!(
                        "csrf_token=csrf123; rustchan_owned_posts={}",
                        crate::handlers::board::remember_owned_post_until(
                            CookieJar::new(),
                            "secret",
                            thread_id,
                            post_id,
                            "edit-token",
                            chrono::Utc::now().timestamp()
                                + crate::handlers::board::SELF_DELETE_WINDOW_SECS,
                        )
                        .get("rustchan_owned_posts")
                        .context("owned posts cookie")?
                        .value()
                    ),
                )
                .extension(crate::test_support::connect_info())
                .body(Body::from("body=changed&_csrf=csrf123"))
                .context("request")?,
        )
        .await
        .context("edit post response")?;
    ensure_eq!(edit_post_response.status(), StatusCode::SEE_OTHER);
    ensure_eq!(
        edit_post_response
            .headers()
            .get(header::LOCATION)
            .and_then(|value| value.to_str().ok()),
        Some(format!("/secret/unlock?return_to=%2Fsecret%2Fthread%2F{thread_id}").as_str())
    );

    let vote_response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/vote")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .header(header::COOKIE, "csrf_token=csrf123")
                .extension(crate::test_support::connect_info())
                .body(Body::from(format!("option_id={option_id}&_csrf=csrf123")))
                .context("request")?,
        )
        .await
        .context("vote response")?;
    ensure_eq!(vote_response.status(), StatusCode::SEE_OTHER);
    ensure_eq!(
        vote_response
            .headers()
            .get(header::LOCATION)
            .and_then(|value| value.to_str().ok()),
        Some(format!("/secret/unlock?return_to=%2Fsecret%2Fthread%2F{thread_id}%23poll").as_str())
    );
    Ok(())
}

#[expect(
    clippy::too_many_lines,
    reason = "the end-to-end test keeps its fixture setup and ordered assertions in one scenario"
)]
#[tokio::test]
async fn self_delete_requires_owned_post_cookie() -> anyhow::Result<()> {
    let state = crate::test_support::app_state();
    let conn = state.db.get().context("db connection")?;
    let board_id =
        crate::db::create_board(&conn, "test", "Test", "", false).context("create board")?;
    conn.execute(
        "UPDATE boards SET allow_self_delete = 1 WHERE id = ?1",
        rusqlite::params![board_id],
    )
    .context("enable self delete")?;
    let op = crate::db::NewPost {
        thread_id: 0,
        board_id,
        name: "anon".to_owned(),
        tripcode: None,
        subject: Some("subject".to_owned()),
        body: "body".to_owned(),
        body_html: "body".to_owned(),
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
        deletion_token: "op-token".to_owned(),
        is_op: true,
    };
    let (thread_id, _op_id, _) =
        crate::db::create_thread_with_optional_poll(&conn, board_id, None, &op, "", None, None)
            .context("create thread")?;
    let reply = crate::db::NewPost {
        thread_id,
        board_id,
        name: "anon".to_owned(),
        tripcode: None,
        subject: None,
        body: "reply".to_owned(),
        body_html: "reply".to_owned(),
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
        deletion_token: "reply-token".to_owned(),
        is_op: false,
    };
    let reply_id = crate::db::create_reply_with_thread_update(&conn, &reply, "", false, None)
        .context("create reply")?;
    drop(conn);

    let router = Router::new()
        .route(
            "/{board}/post/{id}/delete",
            post(crate::handlers::thread::delete_own_post),
        )
        .with_state(state);

    let forbidden = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/test/post/{reply_id}/delete"))
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .header(header::COOKIE, "csrf_token=csrf123")
                .body(Body::from("_csrf=csrf123"))
                .context("request")?,
        )
        .await
        .context("response")?;
    ensure_eq!(forbidden.status(), StatusCode::FORBIDDEN);

    let owned_cookie_jar = crate::handlers::board::remember_owned_post_until(
        CookieJar::new(),
        "test",
        thread_id,
        reply_id,
        "reply-token",
        chrono::Utc::now().timestamp() + crate::handlers::board::SELF_DELETE_WINDOW_SECS,
    );
    let owned_cookie = owned_cookie_jar
        .get("rustchan_owned_posts")
        .context("owned posts cookie")?;
    let allowed = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/test/post/{reply_id}/delete"))
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .header(
                    header::COOKIE,
                    format!(
                        "csrf_token=csrf123; rustchan_owned_posts={}",
                        owned_cookie.value()
                    ),
                )
                .body(Body::from("_csrf=csrf123"))
                .context("request")?,
        )
        .await
        .context("response")?;
    ensure_eq!(allowed.status(), StatusCode::SEE_OTHER);
    ensure_eq!(
        allowed
            .headers()
            .get(header::LOCATION)
            .and_then(|value| value.to_str().ok()),
        Some(format!("/test/thread/{thread_id}").as_str())
    );
    Ok(())
}

#[tokio::test]
async fn search_returns_results_without_500() -> anyhow::Result<()> {
    let state = crate::test_support::app_state();
    {
        let conn = state.db.get().context("db connection")?;
        let board_id =
            crate::db::create_board(&conn, "test", "Test", "", false).context("create board")?;
        let post = crate::db::NewPost {
            thread_id: 0,
            board_id,
            name: "anon".to_owned(),
            tripcode: None,
            subject: Some("subject".to_owned()),
            body: "rust search body".to_owned(),
            body_html: "rust search body".to_owned(),
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
            deletion_token: "token".to_owned(),
            is_op: true,
        };
        crate::db::create_thread_with_optional_poll(&conn, board_id, None, &post, "", None, None)
            .context("create thread")?;
    }

    let router = Router::new()
        .route("/{board}/search", get(super::search))
        .with_state(state);

    let response = router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/test/search?q=rust")
                .body(Body::empty())
                .context("request")?,
        )
        .await
        .context("response")?;

    ensure_eq!(response.status(), StatusCode::OK);
    let body = String::from_utf8(
        to_bytes(response.into_body(), usize::MAX)
            .await
            .context("response body")?
            .to_vec(),
    )
    .context("utf8 body")?;
    anyhow::ensure!(body.contains("rust search body"));
    Ok(())
}

#[tokio::test]
async fn search_without_q_param_returns_empty_results_page() -> anyhow::Result<()> {
    let state = crate::test_support::app_state();
    {
        let conn = state.db.get().context("db connection")?;
        crate::db::create_board(&conn, "test", "Test", "", false).context("create board")?;
    }

    let router = Router::new()
        .route("/{board}/search", get(super::search))
        .with_state(state);

    let response = router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/test/search")
                .body(Body::empty())
                .context("request")?,
        )
        .await
        .context("response")?;

    ensure_eq!(response.status(), StatusCode::OK);
    let body = String::from_utf8(
        to_bytes(response.into_body(), usize::MAX)
            .await
            .context("response body")?
            .to_vec(),
    )
    .context("utf8 body")?;
    anyhow::ensure!(body.contains("no results found."));
    Ok(())
}

#[tokio::test]
async fn locked_board_search_returns_forbidden_unlock_page() -> anyhow::Result<()> {
    let state = crate::test_support::app_state();
    {
        let conn = state.db.get().context("db connection")?;
        crate::db::create_board(&conn, "slock", "Secret", "", false).context("create board")?;
        let password_hash =
            crate::utils::crypto::hash_password("swordfish").context("hash password")?;
        conn.execute(
            "UPDATE boards SET access_mode = ?1, access_password_hash = ?2 WHERE short_name = 'slock'",
            rusqlite::params!["view_password", password_hash],
        )
        .context("update board access")?;
    }

    let router = Router::new()
        .route("/{board}/search", get(super::search))
        .with_state(state);

    let response = router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/slock/search?q=rust")
                .body(Body::empty())
                .context("request")?,
        )
        .await
        .context("response")?;

    ensure_eq!(response.status(), StatusCode::FORBIDDEN);
    ensure_eq!(
        response
            .headers()
            .get(header::CACHE_CONTROL)
            .and_then(|value| value.to_str().ok()),
        Some(crate::cache::CACHE_CONTROL_PRIVATE_NO_STORE)
    );
    let body = String::from_utf8(
        to_bytes(response.into_body(), usize::MAX)
            .await
            .context("response body")?
            .to_vec(),
    )
    .context("utf8 body")?;
    anyhow::ensure!(body.contains("action=\"/slock/unlock\""));
    Ok(())
}

#[tokio::test]
async fn create_thread_accepts_valid_multipart_submission() -> anyhow::Result<()> {
    let state = crate::test_support::app_state();
    {
        let conn = state.db.get().context("db connection")?;
        crate::db::create_board(&conn, "test", "Test", "", false).context("create board")?;
    }

    let router = Router::new()
        .route("/{board}", post(super::create_thread))
        .with_state(state.clone());
    let (boundary, body) =
        crate::test_support::multipart_body(&[("_csrf", "csrf123"), ("body", "hello world")], None);

    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/test")
                .header(
                    header::CONTENT_TYPE,
                    format!("multipart/form-data; boundary={boundary}"),
                )
                .header(header::COOKIE, "csrf_token=csrf123")
                .extension(crate::test_support::connect_info())
                .body(Body::from(body))
                .context("request")?,
        )
        .await
        .context("response")?;

    ensure_eq!(response.status(), StatusCode::SEE_OTHER);
    let location = response
        .headers()
        .get(header::LOCATION)
        .and_then(|value| value.to_str().ok())
        .context("location header")?;
    anyhow::ensure!(location.starts_with("/test/thread/"));
    Ok(())
}

#[tokio::test]
async fn create_thread_xhr_returns_explicit_redirect_header() -> anyhow::Result<()> {
    let state = crate::test_support::app_state();
    {
        let conn = state.db.get().context("db connection")?;
        crate::db::create_board(&conn, "test", "Test", "", false).context("create board")?;
    }

    let router = Router::new()
        .route("/{board}", post(super::create_thread))
        .with_state(state);
    let (boundary, body) =
        crate::test_support::multipart_body(&[("_csrf", "csrf123"), ("body", "hello xhr")], None);

    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/test")
                .header(
                    header::CONTENT_TYPE,
                    format!("multipart/form-data; boundary={boundary}"),
                )
                .header(header::COOKIE, "csrf_token=csrf123")
                .header("X-Requested-With", "XMLHttpRequest")
                .extension(crate::test_support::connect_info())
                .body(Body::from(body))
                .context("request")?,
        )
        .await
        .context("response")?;

    ensure_eq!(response.status(), StatusCode::NO_CONTENT);
    let redirect = response
        .headers()
        .get("x-rustchan-redirect")
        .and_then(|value| value.to_str().ok())
        .context("xhr redirect header")?;
    anyhow::ensure!(redirect.starts_with("/test/thread/"));
    let owned_cookie = response
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find(|value| value.starts_with("rustchan_owned_posts="))
        .context("owned-post cookie")?;
    anyhow::ensure!(owned_cookie.contains("HttpOnly"));
    anyhow::ensure!(owned_cookie.contains("SameSite=Lax"));
    anyhow::ensure!(
        !owned_cookie.contains("Secure"),
        "plain HTTP localhost responses must not mark own-post cookies Secure"
    );
    Ok(())
}

#[tokio::test]
async fn create_thread_xhr_validation_failure_returns_json_error() -> anyhow::Result<()> {
    let state = crate::test_support::app_state();
    {
        let conn = state.db.get().context("db connection")?;
        crate::db::create_board(&conn, "test", "Test", "", false).context("create board")?;
    }

    let router = Router::new()
        .route("/{board}", post(super::create_thread))
        .with_state(state);
    let (boundary, body) =
        crate::test_support::multipart_body(&[("_csrf", "csrf123"), ("body", "")], None);

    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/test")
                .header(
                    header::CONTENT_TYPE,
                    format!("multipart/form-data; boundary={boundary}"),
                )
                .header(header::COOKIE, "csrf_token=csrf123")
                .header("X-Requested-With", "XMLHttpRequest")
                .extension(crate::test_support::connect_info())
                .body(Body::from(body))
                .context("request")?,
        )
        .await
        .context("response")?;

    ensure_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    ensure_eq!(
        response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("application/json; charset=utf-8")
    );
    ensure_eq!(
        response
            .headers()
            .get("x-rustchan-error-status")
            .and_then(|value| value.to_str().ok()),
        Some(StatusCode::UNPROCESSABLE_ENTITY.as_str())
    );

    let body = String::from_utf8(
        to_bytes(response.into_body(), usize::MAX)
            .await
            .context("response body")?
            .to_vec(),
    )
    .context("utf8 body")?;
    anyhow::ensure!(body.contains("\"error\""));
    Ok(())
}

#[test]
fn database_busy_xhr_error_includes_retry_contract() -> anyhow::Result<()> {
    let response = super::xhr_post_error_response(crate::error::AppError::DbBusy)
        .context("database busy response")?;

    ensure_eq!(response.status(), StatusCode::OK);
    ensure_eq!(
        response
            .headers()
            .get("x-rustchan-error-status")
            .and_then(|value| value.to_str().ok()),
        Some(StatusCode::SERVICE_UNAVAILABLE.as_str())
    );
    ensure_eq!(
        response
            .headers()
            .get(header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok()),
        Some("1")
    );
    Ok(())
}

#[tokio::test]
async fn contended_reply_returns_bounded_503_with_retry_after() -> anyhow::Result<()> {
    let state = crate::test_support::app_state();
    let (_board_id, thread_id) = seed_board_with_thread(&state, "busy", "original post")?;
    let router = Router::new()
        .route(
            "/{board}/thread/{id}",
            post(crate::handlers::thread::post_reply),
        )
        .with_state(state.clone());
    let lock = state.db.get().context("write-lock connection")?;
    lock.execute_batch("BEGIN IMMEDIATE")
        .context("hold database write lock")?;
    let submission_token = uuid::Uuid::new_v4().to_string();
    let (boundary, body) = crate::test_support::multipart_body(
        &[
            ("_csrf", "csrf123"),
            ("submission_token", &submission_token),
            ("body", "contended reply"),
        ],
        None,
    );

    let started = std::time::Instant::now();
    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/busy/thread/{thread_id}"))
                .header(
                    header::CONTENT_TYPE,
                    format!("multipart/form-data; boundary={boundary}"),
                )
                .header(header::COOKIE, "csrf_token=csrf123")
                .extension(crate::test_support::connect_info())
                .body(Body::from(body))
                .context("request")?,
        )
        .await
        .context("response")?;
    let elapsed = started.elapsed();

    ensure_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    ensure_eq!(
        response
            .headers()
            .get(header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok()),
        Some("1")
    );
    anyhow::ensure!(
        elapsed < std::time::Duration::from_secs(3),
        "busy response took {elapsed:?}"
    );

    lock.execute_batch("ROLLBACK")
        .context("release write lock")?;
    let conn = state.db.get().context("verification connection")?;
    let post_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM posts WHERE thread_id = ?1",
            rusqlite::params![thread_id],
            |row| row.get(0),
        )
        .context("post count")?;
    let integrity: String = conn
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .context("database integrity")?;
    ensure_eq!(post_count, 1);
    ensure_eq!(integrity, "ok");
    Ok(())
}

#[tokio::test]
async fn create_thread_rejects_mime_mismatch_with_415_inline_error() -> anyhow::Result<()> {
    let state = crate::test_support::app_state();
    {
        let conn = state.db.get().context("db connection")?;
        crate::db::create_board(&conn, "test", "Test", "", false).context("create board")?;
    }

    let router = Router::new()
        .route("/{board}", post(super::create_thread))
        .with_state(state);
    let (boundary, body) = crate::test_support::multipart_body(
        &[("_csrf", "csrf123"), ("body", "bad media")],
        Some(("file", "fake.png", b"plain text", "image/png")),
    );

    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/test")
                .header(
                    header::CONTENT_TYPE,
                    format!("multipart/form-data; boundary={boundary}"),
                )
                .header(header::COOKIE, "csrf_token=csrf123")
                .extension(crate::test_support::connect_info())
                .body(Body::from(body))
                .context("request")?,
        )
        .await
        .context("response")?;

    ensure_eq!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    let body = String::from_utf8(
        to_bytes(response.into_body(), usize::MAX)
            .await
            .context("response body")?
            .to_vec(),
    )
    .context("utf8 body")?;
    anyhow::ensure!(body.contains("post-error-banner"));
    anyhow::ensure!(body.contains("File type not allowed"));
    Ok(())
}

#[tokio::test]
async fn create_thread_rejects_truncated_png_with_422_inline_error() -> anyhow::Result<()> {
    let state = crate::test_support::app_state();
    {
        let conn = state.db.get().context("db connection")?;
        crate::db::create_board(&conn, "test", "Test", "", false).context("create board")?;
    }

    let router = Router::new()
        .route("/{board}", post(super::create_thread))
        .with_state(state);
    let truncated_png = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR";
    let (boundary, body) = crate::test_support::multipart_body(
        &[("_csrf", "csrf123"), ("body", "bad png")],
        Some(("file", "truncated.png", truncated_png, "image/png")),
    );

    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/test")
                .header(
                    header::CONTENT_TYPE,
                    format!("multipart/form-data; boundary={boundary}"),
                )
                .header(header::COOKIE, "csrf_token=csrf123")
                .extension(crate::test_support::connect_info())
                .body(Body::from(body))
                .context("request")?,
        )
        .await
        .context("response")?;

    ensure_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body = String::from_utf8(
        to_bytes(response.into_body(), usize::MAX)
            .await
            .context("response body")?
            .to_vec(),
    )
    .context("utf8 body")?;
    anyhow::ensure!(body.contains("post-error-banner"));
    anyhow::ensure!(body.contains("image header is malformed"));
    Ok(())
}

#[tokio::test]
async fn homepage_and_thread_badges_default_to_enabled() -> anyhow::Result<()> {
    let state = crate::test_support::app_state();
    let conn = state.db.get().context("db connection")?;

    anyhow::ensure!(crate::db::get_homepage_new_thread_badges_enabled(&conn));
    anyhow::ensure!(crate::db::get_homepage_new_reply_badges_enabled(&conn));
    anyhow::ensure!(crate::db::get_thread_new_reply_badges_enabled(&conn));
    Ok(())
}

#[tokio::test]
async fn absent_homepage_reply_badge_setting_defaults_to_enabled() -> anyhow::Result<()> {
    let state = crate::test_support::app_state();
    let conn = state.db.get().context("db connection")?;
    conn.execute(
        "DELETE FROM site_settings WHERE key = 'homepage_new_reply_badges_enabled'",
        [],
    )
    .context("delete setting")?;

    anyhow::ensure!(crate::db::get_homepage_new_reply_badges_enabled(&conn));
    Ok(())
}

#[tokio::test]
async fn homepage_reply_toggle_off_suppresses_only_homepage_reply_badges() -> anyhow::Result<()> {
    let state = crate::test_support::app_state();
    set_new_activity_settings(&state, true, false, true)?;
    let (board_id, thread_id) = seed_board_with_thread(&state, "tech", "op")?;
    let router = activity_router(state.clone());
    let mut cookies = HashMap::new();

    let baseline = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/tech")
                .extension(crate::test_support::connect_info())
                .body(Body::empty())
                .context("request")?,
        )
        .await
        .context("response")?;
    update_cookie_store(&mut cookies, baseline.headers());
    create_thread_on_board(&state, board_id, "new thread")?;
    create_reply_on_thread(&state, board_id, thread_id, "reply")?;

    let home_response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/")
                .header(
                    header::COOKIE,
                    cookie_header(&cookies).context("baseline cookies")?,
                )
                .body(Body::empty())
                .context("request")?,
        )
        .await
        .context("response")?;
    let home_body = response_body_string(home_response).await?;
    anyhow::ensure!(home_body.contains("board-card-new-thread-badge"));
    anyhow::ensure!(!home_body.contains("board-card-new-reply-badge"));

    let catalog_response = router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/tech/catalog")
                .header(
                    header::COOKIE,
                    cookie_header(&cookies).context("baseline cookies")?,
                )
                .extension(crate::test_support::connect_info())
                .body(Body::empty())
                .context("request")?,
        )
        .await
        .context("response")?;
    let catalog_body = response_body_string(catalog_response).await?;
    anyhow::ensure!(catalog_body.contains("catalog-activity-badge"));
    Ok(())
}

#[tokio::test]
async fn thread_toggle_off_does_not_suppress_homepage_reply_badges() -> anyhow::Result<()> {
    let state = crate::test_support::app_state();
    set_new_activity_settings(&state, true, true, false)?;
    let (board_id, thread_id) = seed_board_with_thread(&state, "tech", "op")?;
    let router = activity_router(state.clone());
    let mut cookies = HashMap::new();

    let baseline = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/tech")
                .extension(crate::test_support::connect_info())
                .body(Body::empty())
                .context("request")?,
        )
        .await
        .context("response")?;
    update_cookie_store(&mut cookies, baseline.headers());

    create_reply_on_thread(&state, board_id, thread_id, "reply")?;

    let home_response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/")
                .header(
                    header::COOKIE,
                    cookie_header(&cookies).context("baseline cookies")?,
                )
                .body(Body::empty())
                .context("request")?,
        )
        .await
        .context("response")?;
    let home_body = response_body_string(home_response).await?;
    anyhow::ensure!(home_body.contains("board-card-new-reply-badge"));
    anyhow::ensure!(!home_body.contains("board-card-new-thread-badge"));

    let catalog_response = router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/tech/catalog")
                .header(
                    header::COOKIE,
                    cookie_header(&cookies).context("baseline cookies")?,
                )
                .extension(crate::test_support::connect_info())
                .body(Body::empty())
                .context("request")?,
        )
        .await
        .context("response")?;
    let catalog_body = response_body_string(catalog_response).await?;
    anyhow::ensure!(!catalog_body.contains("catalog-activity-badge"));
    anyhow::ensure!(!catalog_body.contains("thread-summary-activity-badge"));
    Ok(())
}

#[tokio::test]
async fn homepage_thread_toggle_off_does_not_suppress_homepage_reply_badges() -> anyhow::Result<()>
{
    let state = crate::test_support::app_state();
    set_new_activity_settings(&state, false, true, true)?;
    let (board_id, thread_id) = seed_board_with_thread(&state, "tech", "op")?;
    let router = activity_router(state.clone());
    let mut cookies = HashMap::new();

    let baseline = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/tech")
                .extension(crate::test_support::connect_info())
                .body(Body::empty())
                .context("request")?,
        )
        .await
        .context("response")?;
    update_cookie_store(&mut cookies, baseline.headers());

    create_thread_on_board(&state, board_id, "new thread")?;
    create_reply_on_thread(&state, board_id, thread_id, "reply")?;

    let response = router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/")
                .header(
                    header::COOKIE,
                    cookie_header(&cookies).context("baseline cookies")?,
                )
                .body(Body::empty())
                .context("request")?,
        )
        .await
        .context("response")?;
    let body = response_body_string(response).await?;
    anyhow::ensure!(body.contains("board-card-new-reply-badge"));
    anyhow::ensure!(!body.contains("board-card-new-thread-badge"));
    Ok(())
}

#[tokio::test]
async fn thread_badge_markup_sits_between_catalog_info_and_counters() -> anyhow::Result<()> {
    let state = crate::test_support::app_state();
    set_new_activity_settings(&state, true, true, true)?;
    let (board_id, thread_id) = seed_board_with_thread(&state, "tech", "op")?;
    let router = activity_router(state.clone());
    let mut cookies = HashMap::new();

    let baseline = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/tech/catalog")
                .extension(crate::test_support::connect_info())
                .body(Body::empty())
                .context("request")?,
        )
        .await
        .context("response")?;
    update_cookie_store(&mut cookies, baseline.headers());
    create_reply_on_thread(&state, board_id, thread_id, "reply")?;

    let response = router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/tech/catalog")
                .header(header::COOKIE, cookie_header(&cookies).context("cookies")?)
                .extension(crate::test_support::connect_info())
                .body(Body::empty())
                .context("request")?,
        )
        .await
        .context("response")?;
    let body = response_body_string(response).await?;
    let meta_idx = body
        .find("catalog-meta-row")
        .context("catalog meta row present")?;
    let info_idx = body.find("catalog-info").context("catalog info present")?;
    let badge_row_idx = body
        .find("catalog-activity-row")
        .context("catalog badge row present")?;
    let badge_idx = body
        .find("catalog-activity-badge")
        .context("catalog badge present")?;

    anyhow::ensure!(meta_idx < info_idx);
    anyhow::ensure!(info_idx < badge_row_idx);
    anyhow::ensure!(badge_idx > info_idx);
    Ok(())
}

#[tokio::test]
async fn first_board_visit_establishes_quiet_activity_baseline() -> anyhow::Result<()> {
    let state = crate::test_support::app_state();
    set_new_activity_settings(&state, true, true, true)?;
    let (_board_id, _thread_id) = seed_board_with_thread(&state, "tech", "op")?;
    let router = activity_router(state);
    let mut cookies = HashMap::new();

    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/tech/catalog")
                .extension(crate::test_support::connect_info())
                .body(Body::empty())
                .context("request")?,
        )
        .await
        .context("response")?;
    update_cookie_store(&mut cookies, response.headers());
    let body = response_body_string(response).await?;
    anyhow::ensure!(!body.contains("catalog-activity-badge"));

    let home_response = router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/")
                .header(
                    header::COOKIE,
                    cookie_header(&cookies).context("baseline cookies")?,
                )
                .body(Body::empty())
                .context("request")?,
        )
        .await
        .context("response")?;
    let home_body = response_body_string(home_response).await?;
    anyhow::ensure!(!home_body.contains("board-card-activity-badge"));
    Ok(())
}

#[tokio::test]
async fn new_thread_after_board_baseline_shows_homepage_badge() -> anyhow::Result<()> {
    let state = crate::test_support::app_state();
    set_new_activity_settings(&state, true, true, true)?;
    let (board_id, _thread_id) = seed_board_with_thread(&state, "tech", "op")?;
    let router = activity_router(state.clone());
    let mut cookies = HashMap::new();

    let baseline = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/tech")
                .extension(crate::test_support::connect_info())
                .body(Body::empty())
                .context("request")?,
        )
        .await
        .context("response")?;
    update_cookie_store(&mut cookies, baseline.headers());

    create_thread_on_board(&state, board_id, "new thread")?;

    let response = router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/")
                .header(
                    header::COOKIE,
                    cookie_header(&cookies).context("baseline cookies")?,
                )
                .body(Body::empty())
                .context("request")?,
        )
        .await
        .context("response")?;
    let body = response_body_string(response).await?;
    anyhow::ensure!(body.contains("board-card-new-thread-badge"));
    anyhow::ensure!(body.contains(">1 New Threads</span>"));
    Ok(())
}

#[tokio::test]
async fn replies_alone_create_homepage_reply_badge() -> anyhow::Result<()> {
    let state = crate::test_support::app_state();
    set_new_activity_settings(&state, true, true, true)?;
    let (board_id, thread_id) = seed_board_with_thread(&state, "tech", "op")?;
    let router = activity_router(state.clone());
    let mut cookies = HashMap::new();

    let baseline = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/tech")
                .extension(crate::test_support::connect_info())
                .body(Body::empty())
                .context("request")?,
        )
        .await
        .context("response")?;
    update_cookie_store(&mut cookies, baseline.headers());

    create_reply_on_thread(&state, board_id, thread_id, "reply")?;

    let response = router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/")
                .header(
                    header::COOKIE,
                    cookie_header(&cookies).context("baseline cookies")?,
                )
                .body(Body::empty())
                .context("request")?,
        )
        .await
        .context("response")?;
    let body = response_body_string(response).await?;
    anyhow::ensure!(body.contains("board-card-new-reply-badge"));
    anyhow::ensure!(body.contains(">1 New Replies</span>"));
    anyhow::ensure!(!body.contains("board-card-new-thread-badge"));
    Ok(())
}

#[tokio::test]
async fn homepage_thread_and_reply_badges_can_render_together() -> anyhow::Result<()> {
    let state = crate::test_support::app_state();
    set_new_activity_settings(&state, true, true, true)?;
    let (board_id, thread_id) = seed_board_with_thread(&state, "tech", "op")?;
    let router = activity_router(state.clone());
    let mut cookies = HashMap::new();

    let baseline = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/tech")
                .extension(crate::test_support::connect_info())
                .body(Body::empty())
                .context("request")?,
        )
        .await
        .context("response")?;
    update_cookie_store(&mut cookies, baseline.headers());

    create_thread_on_board(&state, board_id, "new thread")?;
    create_reply_on_thread(&state, board_id, thread_id, "reply")?;

    let response = router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/")
                .header(
                    header::COOKIE,
                    cookie_header(&cookies).context("baseline cookies")?,
                )
                .body(Body::empty())
                .context("request")?,
        )
        .await
        .context("response")?;
    let body = response_body_string(response).await?;
    anyhow::ensure!(body.contains("board-card-new-thread-badge"));
    anyhow::ensure!(body.contains("board-card-new-reply-badge"));
    Ok(())
}

#[tokio::test]
async fn board_index_visit_clears_homepage_new_thread_badge() -> anyhow::Result<()> {
    let state = crate::test_support::app_state();
    set_new_activity_settings(&state, true, true, true)?;
    let (board_id, _thread_id) = seed_board_with_thread(&state, "tech", "op")?;
    let router = activity_router(state.clone());
    let mut cookies = HashMap::new();

    let baseline = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/tech")
                .extension(crate::test_support::connect_info())
                .body(Body::empty())
                .context("request")?,
        )
        .await
        .context("response")?;
    update_cookie_store(&mut cookies, baseline.headers());
    create_thread_on_board(&state, board_id, "new thread")?;

    let clear_response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/tech")
                .header(header::COOKIE, cookie_header(&cookies).context("cookies")?)
                .extension(crate::test_support::connect_info())
                .body(Body::empty())
                .context("request")?,
        )
        .await
        .context("response")?;
    update_cookie_store(&mut cookies, clear_response.headers());

    let home_response = router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/")
                .header(header::COOKIE, cookie_header(&cookies).context("cookies")?)
                .body(Body::empty())
                .context("request")?,
        )
        .await
        .context("response")?;
    let body = response_body_string(home_response).await?;
    anyhow::ensure!(!body.contains("board-card-activity-badge"));
    Ok(())
}

#[tokio::test]
async fn board_catalog_visit_clears_homepage_new_thread_badge() -> anyhow::Result<()> {
    let state = crate::test_support::app_state();
    set_new_activity_settings(&state, true, true, true)?;
    let (board_id, _thread_id) = seed_board_with_thread(&state, "tech", "op")?;
    let router = activity_router(state.clone());
    let mut cookies = HashMap::new();

    let baseline = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/tech")
                .extension(crate::test_support::connect_info())
                .body(Body::empty())
                .context("request")?,
        )
        .await
        .context("response")?;
    update_cookie_store(&mut cookies, baseline.headers());
    create_thread_on_board(&state, board_id, "new thread")?;

    let clear_response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/tech/catalog")
                .header(header::COOKIE, cookie_header(&cookies).context("cookies")?)
                .extension(crate::test_support::connect_info())
                .body(Body::empty())
                .context("request")?,
        )
        .await
        .context("response")?;
    update_cookie_store(&mut cookies, clear_response.headers());

    let home_response = router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/")
                .header(header::COOKIE, cookie_header(&cookies).context("cookies")?)
                .body(Body::empty())
                .context("request")?,
        )
        .await
        .context("response")?;
    let body = response_body_string(home_response).await?;
    anyhow::ensure!(!body.contains("board-card-activity-badge"));
    Ok(())
}

#[tokio::test]
async fn thread_visit_clears_homepage_new_thread_badge() -> anyhow::Result<()> {
    let state = crate::test_support::app_state();
    set_new_activity_settings(&state, true, true, true)?;
    let (board_id, thread_id) = seed_board_with_thread(&state, "tech", "op")?;
    let router = activity_router(state.clone());
    let mut cookies = HashMap::new();

    let baseline = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/tech")
                .extension(crate::test_support::connect_info())
                .body(Body::empty())
                .context("request")?,
        )
        .await
        .context("response")?;
    update_cookie_store(&mut cookies, baseline.headers());
    create_thread_on_board(&state, board_id, "new thread")?;

    let clear_response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/tech/thread/{thread_id}"))
                .header(header::COOKIE, cookie_header(&cookies).context("cookies")?)
                .extension(crate::test_support::connect_info())
                .body(Body::empty())
                .context("request")?,
        )
        .await
        .context("response")?;
    update_cookie_store(&mut cookies, clear_response.headers());

    let home_response = router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/")
                .header(header::COOKIE, cookie_header(&cookies).context("cookies")?)
                .body(Body::empty())
                .context("request")?,
        )
        .await
        .context("response")?;
    let body = response_body_string(home_response).await?;
    anyhow::ensure!(!body.contains("board-card-activity-badge"));
    Ok(())
}

#[tokio::test]
async fn conditional_thread_visit_that_marks_activity_read_returns_full_response(
) -> anyhow::Result<()> {
    let state = crate::test_support::app_state();
    set_new_activity_settings(&state, true, true, true)?;
    let (board_id, thread_id) = seed_board_with_thread(&state, "tech", "op")?;
    let router = activity_router(state.clone());
    let mut cookies = HashMap::new();

    let baseline = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/tech/thread/{thread_id}"))
                .extension(crate::test_support::connect_info())
                .body(Body::empty())
                .context("request")?,
        )
        .await
        .context("response")?;
    let baseline_etag = baseline
        .headers()
        .get(header::ETAG)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
        .context("thread etag")?;
    update_cookie_store(&mut cookies, baseline.headers());

    create_thread_on_board(&state, board_id, "new thread")?;

    let clear_response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/tech/thread/{thread_id}"))
                .header(header::COOKIE, cookie_header(&cookies).context("cookies")?)
                .header(header::IF_NONE_MATCH, baseline_etag)
                .extension(crate::test_support::connect_info())
                .body(Body::empty())
                .context("request")?,
        )
        .await
        .context("response")?;
    ensure_eq!(clear_response.status(), StatusCode::OK);
    update_cookie_store(&mut cookies, clear_response.headers());

    let home_response = router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/")
                .header(header::COOKIE, cookie_header(&cookies).context("cookies")?)
                .body(Body::empty())
                .context("request")?,
        )
        .await
        .context("response")?;
    let body = response_body_string(home_response).await?;
    anyhow::ensure!(!body.contains("board-card-activity-badge"));
    Ok(())
}

#[tokio::test]
async fn conditional_board_activity_pages_return_full_response_when_tracking_enabled(
) -> anyhow::Result<()> {
    let state = crate::test_support::app_state();
    set_new_activity_settings(&state, true, true, true)?;
    seed_board_with_thread(&state, "tech", "op")?;
    let router = activity_router(state);

    for uri in ["/tech", "/tech/catalog"] {
        let baseline = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(uri)
                    .extension(crate::test_support::connect_info())
                    .body(Body::empty())
                    .context("request")?,
            )
            .await
            .context("response")?;
        let etag = baseline
            .headers()
            .get(header::ETAG)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned)
            .context("etag")?;

        let conditional = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(uri)
                    .header(header::IF_NONE_MATCH, etag)
                    .extension(crate::test_support::connect_info())
                    .body(Body::empty())
                    .context("request")?,
            )
            .await
            .context("conditional response")?;
        ensure_eq!(conditional.status(), StatusCode::OK);
    }
    Ok(())
}

#[tokio::test]
async fn new_reply_after_thread_baseline_shows_thread_badge_until_visible_board_visit(
) -> anyhow::Result<()> {
    let state = crate::test_support::app_state();
    set_new_activity_settings(&state, true, true, true)?;
    let (board_id, thread_id) = seed_board_with_thread(&state, "tech", "op")?;
    let router = activity_router(state.clone());
    let mut cookies = HashMap::new();

    let baseline = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/tech/catalog")
                .extension(crate::test_support::connect_info())
                .body(Body::empty())
                .context("request")?,
        )
        .await
        .context("response")?;
    update_cookie_store(&mut cookies, baseline.headers());

    create_reply_on_thread(&state, board_id, thread_id, "reply")?;

    let badge_response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/tech/catalog")
                .header(header::COOKIE, cookie_header(&cookies).context("cookies")?)
                .extension(crate::test_support::connect_info())
                .body(Body::empty())
                .context("request")?,
        )
        .await
        .context("response")?;
    update_cookie_store(&mut cookies, badge_response.headers());
    let badge_body = response_body_string(badge_response).await?;
    anyhow::ensure!(badge_body.contains("catalog-activity-badge"));
    anyhow::ensure!(badge_body.contains(">1 New</span>"));

    let cleared_catalog = router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/tech/catalog")
                .header(header::COOKIE, cookie_header(&cookies).context("cookies")?)
                .extension(crate::test_support::connect_info())
                .body(Body::empty())
                .context("request")?,
        )
        .await
        .context("response")?;
    let cleared_body = response_body_string(cleared_catalog).await?;
    anyhow::ensure!(!cleared_body.contains("catalog-activity-badge"));
    Ok(())
}

#[tokio::test]
async fn thread_visit_clears_thread_badge_after_unread_board_render() -> anyhow::Result<()> {
    let state = crate::test_support::app_state();
    set_new_activity_settings(&state, true, true, true)?;
    let (board_id, thread_id) = seed_board_with_thread(&state, "tech", "op")?;
    let router = activity_router(state.clone());
    let mut cookies = HashMap::new();

    let baseline = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/tech/catalog")
                .extension(crate::test_support::connect_info())
                .body(Body::empty())
                .context("request")?,
        )
        .await
        .context("response")?;
    update_cookie_store(&mut cookies, baseline.headers());

    create_reply_on_thread(&state, board_id, thread_id, "reply")?;

    let badge_response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/tech/catalog")
                .header(header::COOKIE, cookie_header(&cookies).context("cookies")?)
                .extension(crate::test_support::connect_info())
                .body(Body::empty())
                .context("request")?,
        )
        .await
        .context("response")?;
    let badge_body = response_body_string(badge_response).await?;
    anyhow::ensure!(badge_body.contains("catalog-activity-badge"));

    let clear_response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/tech/thread/{thread_id}"))
                .header(header::COOKIE, cookie_header(&cookies).context("cookies")?)
                .extension(crate::test_support::connect_info())
                .body(Body::empty())
                .context("request")?,
        )
        .await
        .context("response")?;
    update_cookie_store(&mut cookies, clear_response.headers());

    let cleared_catalog = router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/tech/catalog")
                .header(header::COOKIE, cookie_header(&cookies).context("cookies")?)
                .extension(crate::test_support::connect_info())
                .body(Body::empty())
                .context("request")?,
        )
        .await
        .context("response")?;
    let cleared_body = response_body_string(cleared_catalog).await?;
    anyhow::ensure!(!cleared_body.contains("catalog-activity-badge"));
    Ok(())
}

#[tokio::test]
async fn thread_visit_clears_homepage_and_board_reply_badges() -> anyhow::Result<()> {
    let state = crate::test_support::app_state();
    set_new_activity_settings(&state, true, true, true)?;
    let (board_id, thread_id) = seed_board_with_thread(&state, "tech", "op")?;
    let router = activity_router(state.clone());
    let mut cookies = HashMap::new();

    let baseline = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/tech")
                .extension(crate::test_support::connect_info())
                .body(Body::empty())
                .context("request")?,
        )
        .await
        .context("response")?;
    update_cookie_store(&mut cookies, baseline.headers());

    create_reply_on_thread(&state, board_id, thread_id, "reply")?;

    let badge_home = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/")
                .header(header::COOKIE, cookie_header(&cookies).context("cookies")?)
                .body(Body::empty())
                .context("request")?,
        )
        .await
        .context("response")?;
    let badge_home_body = response_body_string(badge_home).await?;
    anyhow::ensure!(badge_home_body.contains("board-card-new-reply-badge"));

    let badge_board = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/tech")
                .header(header::COOKIE, cookie_header(&cookies).context("cookies")?)
                .extension(crate::test_support::connect_info())
                .body(Body::empty())
                .context("request")?,
        )
        .await
        .context("response")?;
    let badge_board_body = response_body_string(badge_board).await?;
    anyhow::ensure!(badge_board_body.contains("thread-summary-activity-badge"));

    let thread_response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/tech/thread/{thread_id}"))
                .header(header::COOKIE, cookie_header(&cookies).context("cookies")?)
                .extension(crate::test_support::connect_info())
                .body(Body::empty())
                .context("request")?,
        )
        .await
        .context("response")?;
    update_cookie_store(&mut cookies, thread_response.headers());

    let cleared_home = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/")
                .header(header::COOKIE, cookie_header(&cookies).context("cookies")?)
                .body(Body::empty())
                .context("request")?,
        )
        .await
        .context("response")?;
    let cleared_home_body = response_body_string(cleared_home).await?;
    anyhow::ensure!(!cleared_home_body.contains("board-card-new-reply-badge"));

    let cleared_board = router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/tech")
                .header(header::COOKIE, cookie_header(&cookies).context("cookies")?)
                .extension(crate::test_support::connect_info())
                .body(Body::empty())
                .context("request")?,
        )
        .await
        .context("response")?;
    let cleared_board_body = response_body_string(cleared_board).await?;
    anyhow::ensure!(!cleared_board_body.contains("thread-summary-activity-badge"));
    Ok(())
}

#[tokio::test]
async fn board_visit_does_not_clear_unrelated_board_reply_activity() -> anyhow::Result<()> {
    let state = crate::test_support::app_state();
    set_new_activity_settings(&state, true, true, true)?;
    let (tech_board_id, tech_thread_id) = seed_board_with_thread(&state, "tech", "op")?;
    let (chat_board_id, chat_thread_id) = seed_board_with_thread(&state, "chat", "op")?;
    let router = activity_router(state.clone());
    let mut cookies = HashMap::new();

    for uri in ["/tech/catalog", "/chat/catalog"] {
        let baseline = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(uri)
                    .extension(crate::test_support::connect_info())
                    .body(Body::empty())
                    .context("request")?,
            )
            .await
            .context("response")?;
        update_cookie_store(&mut cookies, baseline.headers());
    }

    create_reply_on_thread(&state, tech_board_id, tech_thread_id, "tech reply")?;
    create_reply_on_thread(&state, chat_board_id, chat_thread_id, "chat reply")?;

    let tech_visit = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/tech/catalog")
                .header(header::COOKIE, cookie_header(&cookies).context("cookies")?)
                .extension(crate::test_support::connect_info())
                .body(Body::empty())
                .context("request")?,
        )
        .await
        .context("response")?;
    update_cookie_store(&mut cookies, tech_visit.headers());

    let chat_response = router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/chat/catalog")
                .header(header::COOKIE, cookie_header(&cookies).context("cookies")?)
                .extension(crate::test_support::connect_info())
                .body(Body::empty())
                .context("request")?,
        )
        .await
        .context("response")?;
    let chat_body = response_body_string(chat_response).await?;
    anyhow::ensure!(chat_body.contains("catalog-activity-badge"));
    Ok(())
}

#[tokio::test]
async fn thread_visit_invalidates_stale_catalog_activity_etag() -> anyhow::Result<()> {
    let state = crate::test_support::app_state();
    set_new_activity_settings(&state, true, true, true)?;
    let (board_id, thread_id) = seed_board_with_thread(&state, "tech", "op")?;
    let router = activity_router(state.clone());
    let mut cookies = HashMap::new();

    let baseline = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/tech/catalog")
                .extension(crate::test_support::connect_info())
                .body(Body::empty())
                .context("request")?,
        )
        .await
        .context("response")?;
    update_cookie_store(&mut cookies, baseline.headers());

    create_reply_on_thread(&state, board_id, thread_id, "reply")?;

    let badge_response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/tech/catalog")
                .header(header::COOKIE, cookie_header(&cookies).context("cookies")?)
                .extension(crate::test_support::connect_info())
                .body(Body::empty())
                .context("request")?,
        )
        .await
        .context("response")?;
    let stale_badge_etag = badge_response
        .headers()
        .get(header::ETAG)
        .and_then(|value| value.to_str().ok())
        .context("badge catalog etag")?
        .to_owned();
    let badge_body = response_body_string(badge_response).await?;
    anyhow::ensure!(badge_body.contains("catalog-activity-badge"));

    let thread_response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/tech/thread/{thread_id}"))
                .header(header::COOKIE, cookie_header(&cookies).context("cookies")?)
                .extension(crate::test_support::connect_info())
                .body(Body::empty())
                .context("request")?,
        )
        .await
        .context("response")?;
    update_cookie_store(&mut cookies, thread_response.headers());

    let restored_catalog_response = router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/tech/catalog")
                .header(header::COOKIE, cookie_header(&cookies).context("cookies")?)
                .header(header::IF_NONE_MATCH, stale_badge_etag)
                .extension(crate::test_support::connect_info())
                .body(Body::empty())
                .context("request")?,
        )
        .await
        .context("response")?;

    ensure_eq!(restored_catalog_response.status(), StatusCode::OK);
    let restored_body = response_body_string(restored_catalog_response).await?;
    anyhow::ensure!(!restored_body.contains("catalog-activity-badge"));
    Ok(())
}

#[tokio::test]
async fn board_visit_invalidates_stale_board_activity_etag() -> anyhow::Result<()> {
    let state = crate::test_support::app_state();
    set_new_activity_settings(&state, true, true, true)?;
    let (board_id, thread_id) = seed_board_with_thread(&state, "tech", "op")?;
    let router = activity_router(state.clone());
    let mut cookies = HashMap::new();

    let baseline = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/tech")
                .extension(crate::test_support::connect_info())
                .body(Body::empty())
                .context("request")?,
        )
        .await
        .context("response")?;
    update_cookie_store(&mut cookies, baseline.headers());

    create_reply_on_thread(&state, board_id, thread_id, "reply")?;

    let badge_response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/tech")
                .header(header::COOKIE, cookie_header(&cookies).context("cookies")?)
                .extension(crate::test_support::connect_info())
                .body(Body::empty())
                .context("request")?,
        )
        .await
        .context("response")?;
    let stale_badge_etag = badge_response
        .headers()
        .get(header::ETAG)
        .and_then(|value| value.to_str().ok())
        .context("badge board etag")?
        .to_owned();
    update_cookie_store(&mut cookies, badge_response.headers());
    let badge_body = response_body_string(badge_response).await?;
    anyhow::ensure!(badge_body.contains("thread-summary-activity-badge"));

    let restored_board_response = router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/tech")
                .header(header::COOKIE, cookie_header(&cookies).context("cookies")?)
                .header(header::IF_NONE_MATCH, stale_badge_etag)
                .extension(crate::test_support::connect_info())
                .body(Body::empty())
                .context("request")?,
        )
        .await
        .context("response")?;

    ensure_eq!(restored_board_response.status(), StatusCode::OK);
    let restored_body = response_body_string(restored_board_response).await?;
    anyhow::ensure!(!restored_body.contains("thread-summary-activity-badge"));
    Ok(())
}

#[tokio::test]
async fn thread_updates_clear_thread_activity_badge_cookie() -> anyhow::Result<()> {
    let state = crate::test_support::app_state();
    set_new_activity_settings(&state, true, true, true)?;
    let (board_id, thread_id) = seed_board_with_thread(&state, "tech", "op")?;
    let router = activity_router(state.clone());
    let mut cookies = HashMap::new();

    let baseline = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/tech/catalog")
                .extension(crate::test_support::connect_info())
                .body(Body::empty())
                .context("request")?,
        )
        .await
        .context("response")?;
    update_cookie_store(&mut cookies, baseline.headers());

    create_reply_on_thread(&state, board_id, thread_id, "reply")?;

    let updates = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/tech/thread/{thread_id}/updates?since=0"))
                .header(header::COOKIE, cookie_header(&cookies).context("cookies")?)
                .body(Body::empty())
                .context("request")?,
        )
        .await
        .context("response")?;
    ensure_eq!(updates.status(), StatusCode::OK);
    update_cookie_store(&mut cookies, updates.headers());

    let catalog = router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/tech/catalog")
                .header(header::COOKIE, cookie_header(&cookies).context("cookies")?)
                .extension(crate::test_support::connect_info())
                .body(Body::empty())
                .context("request")?,
        )
        .await
        .context("response")?;
    let body = response_body_string(catalog).await?;
    anyhow::ensure!(!body.contains("catalog-activity-badge"));
    Ok(())
}

#[tokio::test]
async fn thread_updates_clear_homepage_new_thread_badge_cookie() -> anyhow::Result<()> {
    let state = crate::test_support::app_state();
    set_new_activity_settings(&state, true, true, true)?;
    let (board_id, thread_id) = seed_board_with_thread(&state, "tech", "op")?;
    let router = activity_router(state.clone());
    let mut cookies = HashMap::new();

    let baseline = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/tech/thread/{thread_id}"))
                .extension(crate::test_support::connect_info())
                .body(Body::empty())
                .context("request")?,
        )
        .await
        .context("response")?;
    update_cookie_store(&mut cookies, baseline.headers());

    create_thread_on_board(&state, board_id, "new thread")?;

    let badge_home = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/")
                .header(header::COOKIE, cookie_header(&cookies).context("cookies")?)
                .body(Body::empty())
                .context("request")?,
        )
        .await
        .context("response")?;
    let badge_body = response_body_string(badge_home).await?;
    anyhow::ensure!(badge_body.contains("board-card-new-thread-badge"));

    let updates = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/tech/thread/{thread_id}/updates?since=0"))
                .header(header::COOKIE, cookie_header(&cookies).context("cookies")?)
                .body(Body::empty())
                .context("request")?,
        )
        .await
        .context("response")?;
    ensure_eq!(updates.status(), StatusCode::OK);
    update_cookie_store(&mut cookies, updates.headers());

    let cleared_home = router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/")
                .header(header::COOKIE, cookie_header(&cookies).context("cookies")?)
                .body(Body::empty())
                .context("request")?,
        )
        .await
        .context("response")?;
    let cleared_body = response_body_string(cleared_home).await?;
    anyhow::ensure!(!cleared_body.contains("board-card-new-thread-badge"));
    Ok(())
}

#[tokio::test]
async fn password_protected_board_does_not_leak_homepage_new_activity_badge() -> anyhow::Result<()>
{
    let state = crate::test_support::app_state();
    set_new_activity_settings(&state, true, true, true)?;
    let (board_id, _thread_id) = seed_board_with_thread(&state, "secret", "op")?;
    {
        let conn = state.db.get().context("db connection")?;
        let password_hash =
            crate::utils::crypto::hash_password("swordfish").context("hash password")?;
        conn.execute(
            "UPDATE boards SET access_mode = ?1, access_password_hash = ?2 WHERE id = ?3",
            rusqlite::params!["view_password", password_hash, board_id],
        )
        .context("update board access")?;
    }
    let router = activity_router(state);
    let cookie = format!(
        "rustchan_board_activity=v1|{board_id}.0.0.{}",
        chrono::Utc::now().timestamp()
    );

    let response = router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/")
                .header(header::COOKIE, cookie)
                .body(Body::empty())
                .context("request")?,
        )
        .await
        .context("response")?;
    let body = response_body_string(response).await?;
    anyhow::ensure!(!body.contains("board-card-activity-badge"));
    Ok(())
}

#[tokio::test]
async fn thread_updates_rejects_thread_id_from_other_board() -> anyhow::Result<()> {
    let state = crate::test_support::app_state();
    let (_public_board_id, _public_thread_id) = seed_board_with_thread(&state, "pub", "public op")?;
    let (secret_board_id, secret_thread_id) =
        seed_board_with_thread(&state, "secret", "protected op")?;
    {
        let conn = state.db.get().context("db connection")?;
        let password_hash =
            crate::utils::crypto::hash_password("swordfish").context("hash password")?;
        conn.execute(
            "UPDATE boards SET access_mode = ?1, access_password_hash = ?2 WHERE id = ?3",
            rusqlite::params!["view_password", password_hash, secret_board_id],
        )
        .context("protect secret board")?;
    }
    create_reply_on_thread(
        &state,
        secret_board_id,
        secret_thread_id,
        "protected reply should not leak",
    )?;
    let router = activity_router(state);

    let response = router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/pub/thread/{secret_thread_id}/updates?since=0"))
                .body(Body::empty())
                .context("request")?,
        )
        .await
        .context("response")?;

    ensure_eq!(response.status(), StatusCode::NOT_FOUND);
    let body = response_body_string(response).await?;
    anyhow::ensure!(!body.contains("protected op"));
    anyhow::ensure!(!body.contains("protected reply should not leak"));
    Ok(())
}

#[tokio::test]
async fn new_activity_pages_keep_private_no_store_cache_headers() -> anyhow::Result<()> {
    let state = crate::test_support::app_state();
    set_new_activity_settings(&state, true, true, true)?;
    let (_board_id, thread_id) = seed_board_with_thread(&state, "tech", "op")?;
    let router = activity_router(state);

    let home_response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/")
                .body(Body::empty())
                .context("request")?,
        )
        .await
        .context("response")?;
    ensure_eq!(
        home_response
            .headers()
            .get(header::CACHE_CONTROL)
            .and_then(|value| value.to_str().ok()),
        Some(crate::cache::CACHE_CONTROL_PRIVATE_NO_STORE)
    );

    let catalog_response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/tech/catalog")
                .extension(crate::test_support::connect_info())
                .body(Body::empty())
                .context("request")?,
        )
        .await
        .context("response")?;
    ensure_eq!(
        catalog_response
            .headers()
            .get(header::CACHE_CONTROL)
            .and_then(|value| value.to_str().ok()),
        Some(crate::cache::CACHE_CONTROL_PRIVATE_NO_STORE)
    );

    let board_response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/tech")
                .extension(crate::test_support::connect_info())
                .body(Body::empty())
                .context("request")?,
        )
        .await
        .context("response")?;
    ensure_eq!(
        board_response
            .headers()
            .get(header::CACHE_CONTROL)
            .and_then(|value| value.to_str().ok()),
        Some(crate::cache::CACHE_CONTROL_PRIVATE_NO_STORE)
    );

    let thread_response = router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/tech/thread/{thread_id}"))
                .extension(crate::test_support::connect_info())
                .body(Body::empty())
                .context("request")?,
        )
        .await
        .context("response")?;
    ensure_eq!(
        thread_response
            .headers()
            .get(header::CACHE_CONTROL)
            .and_then(|value| value.to_str().ok()),
        Some(crate::cache::CACHE_CONTROL_PRIVATE_NO_STORE)
    );
    Ok(())
}

#[tokio::test]
async fn activity_pages_keep_existing_cache_policy_when_tracking_disabled() -> anyhow::Result<()> {
    let state = crate::test_support::app_state();
    set_new_activity_settings(&state, false, false, false)?;
    let (_board_id, thread_id) = seed_board_with_thread(&state, "tech", "op")?;
    let router = activity_router(state);
    let thread_uri = format!("/tech/thread/{thread_id}");

    for uri in ["/", "/tech", "/tech/catalog", thread_uri.as_str()] {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(uri)
                    .extension(crate::test_support::connect_info())
                    .body(Body::empty())
                    .context("request")?,
            )
            .await
            .context("response")?;
        ensure_eq!(
            response
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some(super::HTML_CACHE_CONTROL),
            "{uri} should keep no-cache when activity tracking is disabled"
        );
    }
    Ok(())
}

#[tokio::test]
async fn catalog_baseline_tracks_only_highest_priority_threads_within_cookie_limit(
) -> anyhow::Result<()> {
    let state = crate::test_support::app_state();
    set_new_activity_settings(&state, true, true, true)?;
    let (board_id, first_thread_id) = seed_board_with_thread(&state, "tech", "op")?;
    let mut created_thread_ids = vec![first_thread_id];
    for index in 0..120 {
        created_thread_ids.push(create_thread_on_board(
            &state,
            board_id,
            &format!("thread {index}"),
        )?);
    }
    let router = activity_router(state);

    let response = router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/tech/catalog")
                .extension(crate::test_support::connect_info())
                .body(Body::empty())
                .context("request")?,
        )
        .await
        .context("response")?;

    let cookie_value = response
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find_map(|value| {
            let (name, cookie_value) = value
                .split(';')
                .next()
                .and_then(|pair| pair.split_once('='))?;
            (name == "rustchan_thread_activity").then(|| cookie_value.to_owned())
        })
        .context("thread activity cookie")?;
    let mut cookie_headers = HeaderMap::new();
    cookie_headers.insert(
        header::COOKIE,
        format!("rustchan_thread_activity={cookie_value}")
            .parse()
            .context("cookie header")?,
    );
    let jar = CookieJar::from_headers(&cookie_headers);
    let markers = super::thread_activity_markers_from_jar(&jar);

    ensure_eq!(markers.len(), super::THREAD_ACTIVITY_MARKER_LIMIT);

    let expected_tracked = created_thread_ids
        .iter()
        .rev()
        .take(super::THREAD_ACTIVITY_MARKER_LIMIT)
        .copied()
        .collect::<Vec<_>>();
    for thread_id in expected_tracked {
        anyhow::ensure!(
            markers.contains_key(&thread_id),
            "expected tracked thread marker for {thread_id}"
        );
    }
    anyhow::ensure!(
        !markers.contains_key(&first_thread_id),
        "oldest catalog thread should not displace newer visible threads"
    );
    Ok(())
}

#[tokio::test]
async fn create_thread_xhr_banned_user_redirects_to_banned_page() -> anyhow::Result<()> {
    let state = crate::test_support::app_state();
    {
        let conn = state.db.get().context("db connection")?;
        crate::db::create_board(&conn, "test", "Test", "", false).context("create board")?;
        crate::db::add_ban(
            &conn,
            &crate::utils::crypto::hash_ip("127.0.0.1", &crate::config::CONFIG.cookie_secret),
            "testing ban",
            None,
        )
        .context("add ban")?;
    }

    let router = Router::new()
        .route("/{board}", post(super::create_thread))
        .with_state(state);
    let (boundary, body) = crate::test_support::multipart_body(
        &[("_csrf", "csrf123"), ("body", "hello banned")],
        None,
    );

    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/test")
                .header(
                    header::CONTENT_TYPE,
                    format!("multipart/form-data; boundary={boundary}"),
                )
                .header(header::COOKIE, "csrf_token=csrf123")
                .header("X-Requested-With", "XMLHttpRequest")
                .extension(crate::test_support::connect_info())
                .body(Body::from(body))
                .context("request")?,
        )
        .await
        .context("response")?;

    ensure_eq!(response.status(), StatusCode::NO_CONTENT);
    ensure_eq!(
        response
            .headers()
            .get("x-rustchan-redirect")
            .and_then(|value| value.to_str().ok()),
        Some(super::banned_page_redirect_url("testing ban").as_str())
    );
    Ok(())
}

#[tokio::test]
async fn create_thread_xhr_captcha_failure_returns_inline_json_error() -> anyhow::Result<()> {
    let state = crate::test_support::app_state();
    {
        let conn = state.db.get().context("db connection")?;
        crate::db::create_board(&conn, "test", "Test", "", false).context("create board")?;
        conn.execute(
            "UPDATE boards SET allow_captcha = 1 WHERE short_name = 'test'",
            [],
        )
        .context("enable captcha")?;
    }

    let router = Router::new()
        .route("/{board}", post(super::create_thread))
        .with_state(state);
    let (boundary, body) = crate::test_support::multipart_body(
        &[("_csrf", "csrf123"), ("body", "captcha please")],
        None,
    );

    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/test")
                .header(
                    header::CONTENT_TYPE,
                    format!("multipart/form-data; boundary={boundary}"),
                )
                .header(header::COOKIE, "csrf_token=csrf123")
                .header("X-Requested-With", "XMLHttpRequest")
                .extension(crate::test_support::connect_info())
                .body(Body::from(body))
                .context("request")?,
        )
        .await
        .context("response")?;

    ensure_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    ensure_eq!(
        response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("application/json; charset=utf-8")
    );
    ensure_eq!(
        response
            .headers()
            .get("x-rustchan-error-status")
            .and_then(|value| value.to_str().ok()),
        Some(StatusCode::UNPROCESSABLE_ENTITY.as_str())
    );

    let body = String::from_utf8(
        to_bytes(response.into_body(), usize::MAX)
            .await
            .context("response body")?
            .to_vec(),
    )
    .context("utf8 body")?;
    anyhow::ensure!(body.contains("CAPTCHA verification failed"));
    Ok(())
}

#[tokio::test]
async fn duplicate_report_redirects_back_without_500() -> anyhow::Result<()> {
    let state = crate::test_support::app_state();
    let (thread_id, post_id) = {
        let conn = state.db.get().context("db connection")?;
        let board_id =
            crate::db::create_board(&conn, "test", "Test", "", false).context("create board")?;
        let post = crate::db::NewPost {
            thread_id: 0,
            board_id,
            name: "anon".to_owned(),
            tripcode: None,
            subject: Some("subject".to_owned()),
            body: "report me".to_owned(),
            body_html: "report me".to_owned(),
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
            deletion_token: "token".to_owned(),
            is_op: true,
        };
        let (thread_id, post_id, _) = crate::db::create_thread_with_optional_poll(
            &conn, board_id, None, &post, "", None, None,
        )
        .context("create thread")?;
        (thread_id, post_id)
    };

    let router = Router::new()
        .route("/report", post(super::file_report))
        .with_state(state.clone());

    for _ in 0..2 {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/report")
                    .header(
                        header::CONTENT_TYPE,
                        "application/x-www-form-urlencoded",
                    )
                    .header(header::COOKIE, "csrf_token=csrf123")
                    .extension(crate::test_support::connect_info())
                    .body(Body::from(format!(
                        "post_id={post_id}&thread_id={thread_id}&board=test&reason=spam&_csrf=csrf123"
                    )))
                    .context("request")?,
            )
            .await
            .context("response")?;

        ensure_eq!(response.status(), StatusCode::SEE_OTHER);
        let location = response
            .headers()
            .get(header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .context("location header")?;
        ensure_eq!(
            location,
            format!("/test/thread/{thread_id}?reported=1#p{post_id}")
        );
    }

    let open_reports = {
        let conn = state.db.get().context("db connection")?;
        conn.query_row(
            "SELECT COUNT(*) FROM reports WHERE post_id = ?1 AND status = 'open'",
            rusqlite::params![post_id],
            |row| row.get::<_, i64>(0),
        )
        .context("open report count")?
    };
    ensure_eq!(open_reports, 1);
    Ok(())
}

#[tokio::test]
async fn create_thread_rejects_uploads_on_upload_disabled_board() -> anyhow::Result<()> {
    let state = crate::test_support::app_state();
    {
        let mut conn = state.db.get().context("db connection")?;
        crate::db::create_board(&conn, "test", "Test", "", false).context("create board")?;
        crate::db::update_board_settings(
            &mut conn,
            1,
            "Test",
            "",
            false,
            500,
            100,
            150,
            false,
            false,
            false,
            i64::try_from(crate::config::CONFIG.max_image_size)
                .context("image size fits in i64")?,
            i64::try_from(crate::config::CONFIG.max_video_size)
                .context("video size fits in i64")?,
            i64::try_from(crate::config::CONFIG.max_audio_size)
                .context("audio size fits in i64")?,
            i64::try_from(crate::config::CONFIG.max_image_size).context("pdf size fits in i64")?,
            false,
            false,
            true,
            0,
            false,
            false,
            true,
            false,
            false,
            false,
            false,
            0,
            "",
            crate::models::BoardBannerMode::Inherit,
            crate::models::BoardAccessMode::Public,
            "",
        )
        .context("update board settings")?;
    }

    let router = Router::new()
        .route("/{board}", post(super::create_thread))
        .with_state(state);
    let (boundary, body) = crate::test_support::multipart_body(
        &[("_csrf", "csrf123"), ("body", "file attempt")],
        Some(("file", "image.png", b"\x89PNG\r\n\x1a\n", "image/png")),
    );

    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/test")
                .header(
                    header::CONTENT_TYPE,
                    format!("multipart/form-data; boundary={boundary}"),
                )
                .header(header::COOKIE, "csrf_token=csrf123")
                .extension(crate::test_support::connect_info())
                .body(Body::from(body))
                .context("request")?,
        )
        .await
        .context("response")?;

    ensure_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    Ok(())
}

#[tokio::test]
async fn view_locked_catalog_renders_unlock_page() -> anyhow::Result<()> {
    let state = crate::test_support::app_state();
    {
        let conn = state.db.get().context("db connection")?;
        crate::db::create_board(&conn, "secret", "Secret", "", false).context("create board")?;
        let password_hash =
            crate::utils::crypto::hash_password("swordfish").context("hash password")?;
        conn.execute(
            "UPDATE boards SET access_mode = ?1, access_password_hash = ?2 WHERE short_name = 'secret'",
            rusqlite::params!["view_password", password_hash],
        )
        .context("update board access")?;
    }

    let router = Router::new()
        .route("/{board}/catalog", get(super::catalog))
        .with_state(state);

    let response = router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/secret/catalog")
                .extension(crate::test_support::connect_info())
                .body(Body::empty())
                .context("request")?,
        )
        .await
        .context("response")?;

    ensure_eq!(response.status(), StatusCode::FORBIDDEN);
    let body = String::from_utf8(
        to_bytes(response.into_body(), usize::MAX)
            .await
            .context("response body")?
            .to_vec(),
    )
    .context("utf8 body")?;
    anyhow::ensure!(body.contains("password protected board"));
    anyhow::ensure!(body.contains("action=\"/secret/unlock\""));
    Ok(())
}

#[tokio::test]
async fn unlock_board_access_sets_cookie_and_redirects() -> anyhow::Result<()> {
    let state = crate::test_support::app_state();
    {
        let conn = state.db.get().context("db connection")?;
        crate::db::create_board(&conn, "secret", "Secret", "", false).context("create board")?;
        let password_hash =
            crate::utils::crypto::hash_password("swordfish").context("hash password")?;
        conn.execute(
            "UPDATE boards SET access_mode = ?1, access_password_hash = ?2 WHERE short_name = 'secret'",
            rusqlite::params!["view_password", password_hash],
        )
        .context("update board access")?;
    }

    let router = Router::new()
        .route("/{board}/unlock", post(super::unlock_board_access))
        .with_state(state);

    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/secret/unlock")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .header(header::COOKIE, "csrf_token=csrf123")
                .extension(crate::test_support::connect_info())
                .body(Body::from(
                    "password=swordfish&return_to=%2Fsecret%2Fcatalog&_csrf=csrf123",
                ))
                .context("request")?,
        )
        .await
        .context("response")?;

    ensure_eq!(response.status(), StatusCode::SEE_OTHER);
    ensure_eq!(
        response
            .headers()
            .get(header::LOCATION)
            .and_then(|value| value.to_str().ok()),
        Some("/secret/catalog")
    );
    let set_cookie = response
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find(|value| value.contains(&super::board_access_cookie_name("secret")))
        .context("board access cookie")?;
    anyhow::ensure!(set_cookie.contains("HttpOnly"));
    Ok(())
}

#[tokio::test]
async fn unlock_board_access_rejects_malformed_return_to_and_uses_board_default(
) -> anyhow::Result<()> {
    let state = crate::test_support::app_state();
    {
        let conn = state.db.get().context("db connection")?;
        crate::db::create_board(&conn, "secret", "Secret", "", false).context("create board")?;
        let password_hash =
            crate::utils::crypto::hash_password("swordfish").context("hash password")?;
        conn.execute(
            "UPDATE boards SET access_mode = ?1, access_password_hash = ?2 WHERE short_name = 'secret'",
            rusqlite::params!["view_password", password_hash],
        )
        .context("update board access")?;
    }

    let router = Router::new()
        .route("/{board}/unlock", post(super::unlock_board_access))
        .with_state(state);

    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/secret/unlock")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .header(header::COOKIE, "csrf_token=csrf123")
                .extension(crate::test_support::connect_info())
                .body(Body::from(
                    "password=swordfish&return_to=%2F%2Fevil.example%2Fcatalog&_csrf=csrf123",
                ))
                .context("request")?,
        )
        .await
        .context("response")?;

    ensure_eq!(response.status(), StatusCode::SEE_OTHER);
    ensure_eq!(
        response
            .headers()
            .get(header::LOCATION)
            .and_then(|value| value.to_str().ok()),
        Some("/secret/catalog")
    );
    Ok(())
}

#[tokio::test]
async fn changing_board_password_invalidates_existing_unlock_cookie() -> anyhow::Result<()> {
    let state = crate::test_support::app_state();
    {
        let conn = state.db.get().context("db connection")?;
        crate::db::create_board(&conn, "secret", "Secret", "", false).context("create board")?;
        let password_hash =
            crate::utils::crypto::hash_password("swordfish").context("hash password")?;
        conn.execute(
            "UPDATE boards SET access_mode = ?1, access_password_hash = ?2 WHERE short_name = 'secret'",
            rusqlite::params!["view_password", password_hash],
        )
        .context("update board access")?;
    }

    let router = Router::new()
        .route("/{board}/unlock", post(super::unlock_board_access))
        .route("/{board}/catalog", get(super::catalog))
        .with_state(state.clone());

    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/secret/unlock")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .header(header::COOKIE, "csrf_token=csrf123")
                .extension(crate::test_support::connect_info())
                .body(Body::from(
                    "password=swordfish&return_to=%2Fsecret%2Fcatalog&_csrf=csrf123",
                ))
                .context("request")?,
        )
        .await
        .context("unlock response")?;
    let access_cookie = response
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find(|value| value.contains(&super::board_access_cookie_name("secret")))
        .and_then(|value| value.split(';').next())
        .context("board access cookie")?
        .to_owned();

    {
        let conn = state.db.get().context("db connection")?;
        let password_hash =
            crate::utils::crypto::hash_password("newpass").context("hash password")?;
        conn.execute(
            "UPDATE boards SET access_password_hash = ?1 WHERE short_name = 'secret'",
            rusqlite::params![password_hash],
        )
        .context("change board password")?;
    }

    let response = router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/secret/catalog")
                .header(header::COOKIE, access_cookie)
                .extension(crate::test_support::connect_info())
                .body(Body::empty())
                .context("request")?,
        )
        .await
        .context("catalog response")?;

    ensure_eq!(response.status(), StatusCode::FORBIDDEN);
    Ok(())
}

#[tokio::test]
async fn theme_redirect_ignores_external_referer_fallback() -> anyhow::Result<()> {
    let router = Router::new()
        .route("/theme/{theme}", get(crate::handlers::board::set_theme))
        .with_state(crate::test_support::app_state());

    let response = router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/theme/forest")
                .header(header::COOKIE, "csrf_token=csrf123")
                .header(header::REFERER, "https://evil.example/secret/catalog")
                .body(Body::empty())
                .context("request")?,
        )
        .await
        .context("response")?;

    ensure_eq!(response.status(), StatusCode::SEE_OTHER);
    ensure_eq!(
        response
            .headers()
            .get(header::LOCATION)
            .and_then(|value| value.to_str().ok()),
        Some("/")
    );
    Ok(())
}

#[tokio::test]
async fn theme_redirect_persists_no_js_theme_without_csrf() -> anyhow::Result<()> {
    let router = Router::new()
        .route("/theme/{theme}", get(crate::handlers::board::set_theme))
        .with_state(crate::test_support::app_state());

    let accepted = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/theme/blue-sky?return_to=%2Fsecret%2Fcatalog")
                .body(Body::empty())
                .context("request")?,
        )
        .await
        .context("accepted response")?;

    ensure_eq!(accepted.status(), StatusCode::SEE_OTHER);
    ensure_eq!(
        accepted
            .headers()
            .get(header::LOCATION)
            .and_then(|value| value.to_str().ok()),
        Some("/secret/catalog")
    );
    anyhow::ensure!(accepted
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .any(|value| value.starts_with("rustchan_theme=blue-sky;")));

    let rejected = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/theme/forest?return_to=%2Fsecret%2Fcatalog&_csrf=wrong")
                .header(header::COOKIE, "csrf_token=csrf123")
                .body(Body::empty())
                .context("request")?,
        )
        .await
        .context("rejected response")?;

    ensure_eq!(rejected.status(), StatusCode::FORBIDDEN);
    anyhow::ensure!(rejected.headers().get(header::SET_COOKIE).is_none());

    let accepted = router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/theme/forest?return_to=%2Fsecret%2Fcatalog&_csrf=csrf123")
                .header(header::COOKIE, "csrf_token=csrf123")
                .body(Body::empty())
                .context("request")?,
        )
        .await
        .context("accepted response")?;

    ensure_eq!(accepted.status(), StatusCode::SEE_OTHER);
    ensure_eq!(
        accepted
            .headers()
            .get(header::LOCATION)
            .and_then(|value| value.to_str().ok()),
        Some("/secret/catalog")
    );
    anyhow::ensure!(accepted
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .any(|value| value.starts_with("rustchan_theme=forest;")));
    Ok(())
}

#[test]
fn user_preferences_from_jar_defaults_and_ignores_invalid_values() -> anyhow::Result<()> {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::COOKIE,
        "rustchan_hide_nsfw=maybe; rustchan_video_audio=loud; rustchan_preferred_view=grid; rustchan_activity_badges=maybe"
            .parse()
            .context("cookie header")?,
    );
    let jar = CookieJar::from_headers(&headers);

    let preferences = super::user_preferences_from_jar(&jar);

    anyhow::ensure!(!preferences.hide_nsfw_boards);
    anyhow::ensure!(!preferences.video_audio_muted);
    anyhow::ensure!(preferences.preferred_board_view.is_catalog());
    anyhow::ensure!(preferences.show_activity_badges);
    Ok(())
}

fn set_cookie_pairs(response: &axum::response::Response) -> String {
    response
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .filter_map(|value| value.split(';').next())
        .collect::<Vec<_>>()
        .join("; ")
}

#[tokio::test]
async fn set_user_preferences_requires_csrf_and_sets_bounded_cookies() -> anyhow::Result<()> {
    install_preference_test_themes();
    let router = Router::new().route("/preferences", post(super::set_user_preferences));

    let rejected = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/preferences")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .header(header::COOKIE, "csrf_token=csrf123")
                .body(Body::from("theme=forest"))
                .context("request")?,
        )
        .await
        .context("rejected response")?;
    ensure_eq!(rejected.status(), StatusCode::FORBIDDEN);

    let accepted = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/preferences")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .header(header::COOKIE, "csrf_token=csrf123")
                .body(Body::from(
                    "_csrf=csrf123&return_to=%2Ftech%2Fcatalog&theme=forest&hide_nsfw_boards=1&video_audio=mute&preferred_board_view=index",
                ))
                .context("request")?,
        )
        .await
        .context("accepted response")?;

    ensure_eq!(accepted.status(), StatusCode::SEE_OTHER);
    ensure_eq!(
        accepted
            .headers()
            .get(header::LOCATION)
            .and_then(|value| value.to_str().ok()),
        Some("/tech/catalog")
    );
    let set_cookies = accepted
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .collect::<Vec<_>>()
        .join("\n");
    anyhow::ensure!(set_cookies.contains("rustchan_theme=forest"));
    anyhow::ensure!(set_cookies.contains("rustchan_hide_nsfw=1"));
    anyhow::ensure!(set_cookies.contains("rustchan_video_audio=mute"));
    anyhow::ensure!(set_cookies.contains("rustchan_preferred_view=index"));
    anyhow::ensure!(set_cookies.contains("rustchan_activity_badges=0"));
    anyhow::ensure!(set_cookies.contains("SameSite=Lax"));
    anyhow::ensure!(set_cookies.contains("Path=/"));
    Ok(())
}

#[tokio::test]
async fn set_user_preferences_supports_background_cookie_updates() -> anyhow::Result<()> {
    install_preference_test_themes();
    let router = Router::new().route("/preferences", post(super::set_user_preferences));

    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/preferences")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .header(header::COOKIE, "csrf_token=csrf123")
                .header("x-rustchan-background", "1")
                .body(Body::from(
                    "_csrf=csrf123&return_to=%2Ftech&preferences_form=1&theme=blue-sky&hide_nsfw_boards=1&video_audio=mute&preferred_board_view=index&show_activity_badges=1",
                ))
                .context("request")?,
        )
        .await
        .context("response")?;

    ensure_eq!(response.status(), StatusCode::NO_CONTENT);
    let set_cookies = set_cookie_pairs(&response);
    anyhow::ensure!(set_cookies.contains("rustchan_theme=blue-sky"));
    anyhow::ensure!(set_cookies.contains("rustchan_hide_nsfw=1"));
    anyhow::ensure!(set_cookies.contains("rustchan_video_audio=mute"));
    anyhow::ensure!(set_cookies.contains("rustchan_preferred_view=index"));
    anyhow::ensure!(set_cookies.contains("rustchan_activity_badges=1"));
    Ok(())
}

#[tokio::test]
async fn set_user_preferences_accepts_admin_scoped_csrf_from_admin_panel() -> anyhow::Result<()> {
    install_preference_test_themes();
    let router = Router::new().route("/preferences", post(super::set_user_preferences));
    let csrf = crate::utils::crypto::make_scoped_csrf_form_token(
        "csrf123",
        &crate::config::CONFIG.cookie_secret,
        "session123",
    );

    let accepted = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/preferences")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .header(
                    header::COOKIE,
                    "csrf_token=csrf123; chan_admin_session=session123",
                )
                .body(Body::from(format!(
                    "_csrf={csrf}&return_to=%2Fadmin%2Fpanel&preferences_form=1&theme=forest&video_audio=mute&preferred_board_view=index"
                )))
                .context("request")?,
        )
        .await
        .context("accepted response")?;

    ensure_eq!(accepted.status(), StatusCode::SEE_OTHER);
    ensure_eq!(
        accepted
            .headers()
            .get(header::LOCATION)
            .and_then(|value| value.to_str().ok()),
        Some("/admin/panel")
    );
    anyhow::ensure!(set_cookie_pairs(&accepted).contains("rustchan_theme=forest"));

    let rejected = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/preferences")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .header(
                    header::COOKIE,
                    "csrf_token=csrf123; chan_admin_session=session123",
                )
                .body(Body::from(
                    "_csrf=csrf123.invalid&return_to=%2Fadmin%2Fpanel&preferences_form=1&theme=forest",
                ))
                .context("request")?,
        )
        .await
        .context("rejected response")?;

    ensure_eq!(rejected.status(), StatusCode::FORBIDDEN);
    Ok(())
}

#[tokio::test]
async fn preferences_theme_cookie_drives_rendered_theme_after_reload() -> anyhow::Result<()> {
    let state = crate::test_support::app_state();
    install_preference_test_themes();
    seed_board_with_thread(&state, "tech", "op")?;
    let router = Router::new()
        .route("/preferences", post(super::set_user_preferences))
        .route("/{board}", get(super::board_index))
        .with_state(state);

    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/preferences")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .header(header::COOKIE, "csrf_token=csrf123")
                .body(Body::from(
                    "_csrf=csrf123&return_to=%2Ftech&preferences_form=1&theme=blue-sky&video_audio=on&preferred_board_view=catalog&show_activity_badges=1",
                ))
                .context("request")?,
        )
        .await
        .context("preference response")?;

    ensure_eq!(response.status(), StatusCode::SEE_OTHER);
    let cookie_header = set_cookie_pairs(&response);
    anyhow::ensure!(cookie_header.contains("rustchan_theme=blue-sky"));

    let rendered = router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/tech")
                .header(header::COOKIE, cookie_header)
                .extension(crate::test_support::connect_info())
                .body(Body::empty())
                .context("request")?,
        )
        .await
        .context("rendered response")?;
    ensure_eq!(rendered.status(), StatusCode::OK);
    let body = String::from_utf8(
        to_bytes(rendered.into_body(), usize::MAX)
            .await
            .context("body bytes")?
            .to_vec(),
    )
    .context("utf8 body")?;
    anyhow::ensure!(body.contains(r#"data-active-theme="blue-sky""#));
    anyhow::ensure!(body.contains(r#"data-theme="blue-sky""#));
    anyhow::ensure!(body.contains(r#"<option value="blue-sky" selected>Blue Sky</option>"#));
    Ok(())
}

#[tokio::test]
async fn invalid_preferences_theme_falls_back_without_panic() -> anyhow::Result<()> {
    install_preference_test_themes();
    let router = Router::new().route("/preferences", post(super::set_user_preferences));

    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/preferences")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .header(
                    header::COOKIE,
                    "csrf_token=csrf123; rustchan_theme=blue-sky; rustchan_hide_nsfw=1",
                )
                .body(Body::from(
                    "_csrf=csrf123&return_to=%2F&theme=does-not-exist",
                ))
                .context("request")?,
        )
        .await
        .context("response")?;

    ensure_eq!(response.status(), StatusCode::SEE_OTHER);
    let set_cookies = response
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .collect::<Vec<_>>()
        .join("\n");
    anyhow::ensure!(set_cookies.contains("rustchan_theme=blue-sky"));
    anyhow::ensure!(set_cookies.contains("rustchan_hide_nsfw=1"));
    Ok(())
}

#[tokio::test]
async fn partial_preference_updates_preserve_unrelated_cookies() -> anyhow::Result<()> {
    install_preference_test_themes();
    let router = Router::new().route("/preferences", post(super::set_user_preferences));

    let theme_only = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/preferences")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .header(
                    header::COOKIE,
                    "csrf_token=csrf123; rustchan_hide_nsfw=1; rustchan_video_audio=mute; rustchan_preferred_view=index; rustchan_activity_badges=0",
                )
                .body(Body::from("_csrf=csrf123&return_to=%2F&theme=blue-sky"))
                .context("request")?,
        )
        .await
        .context("theme-only response")?;
    ensure_eq!(theme_only.status(), StatusCode::SEE_OTHER);
    let theme_only_cookies = set_cookie_pairs(&theme_only);
    anyhow::ensure!(theme_only_cookies.contains("rustchan_theme=blue-sky"));
    anyhow::ensure!(theme_only_cookies.contains("rustchan_hide_nsfw=1"));
    anyhow::ensure!(theme_only_cookies.contains("rustchan_video_audio=mute"));
    anyhow::ensure!(theme_only_cookies.contains("rustchan_preferred_view=index"));
    anyhow::ensure!(theme_only_cookies.contains("rustchan_activity_badges=0"));

    let unrelated_only = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/preferences")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .header(header::COOKIE, "csrf_token=csrf123; rustchan_theme=blue-sky")
                .body(Body::from(
                    "_csrf=csrf123&return_to=%2F&preferences_form=1&video_audio=mute&preferred_board_view=index",
                ))
                .context("request")?,
        )
        .await
        .context("unrelated-only response")?;
    ensure_eq!(unrelated_only.status(), StatusCode::SEE_OTHER);
    let unrelated_cookies = set_cookie_pairs(&unrelated_only);
    anyhow::ensure!(unrelated_cookies.contains("rustchan_theme=blue-sky"));
    anyhow::ensure!(unrelated_cookies.contains("rustchan_video_audio=mute"));
    anyhow::ensure!(unrelated_cookies.contains("rustchan_preferred_view=index"));
    Ok(())
}

#[tokio::test]
async fn user_theme_overrides_configured_default_and_changes_etag() -> anyhow::Result<()> {
    let state = crate::test_support::app_state();
    install_preference_test_themes();
    seed_board_with_thread(&state, "tech", "op")?;
    crate::templates::set_live_default_theme("forest");
    let router = activity_router(state);

    let default_response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/tech")
                .extension(crate::test_support::connect_info())
                .body(Body::empty())
                .context("request")?,
        )
        .await
        .context("default response")?;
    ensure_eq!(default_response.status(), StatusCode::OK);
    let default_etag = default_response
        .headers()
        .get(header::ETAG)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
        .context("default etag")?;

    let themed_response = router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/tech")
                .header(header::COOKIE, "rustchan_theme=blue-sky")
                .header(header::IF_NONE_MATCH, default_etag.as_str())
                .extension(crate::test_support::connect_info())
                .body(Body::empty())
                .context("request")?,
        )
        .await
        .context("themed response")?;
    ensure_eq!(themed_response.status(), StatusCode::OK);
    let themed_etag = themed_response
        .headers()
        .get(header::ETAG)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
        .context("themed etag")?;
    ensure_ne!(default_etag, themed_etag);
    let body = String::from_utf8(
        to_bytes(themed_response.into_body(), usize::MAX)
            .await
            .context("body bytes")?
            .to_vec(),
    )
    .context("utf8 body")?;
    anyhow::ensure!(body.contains(r#"data-default-theme="forest""#));
    anyhow::ensure!(body.contains(r#"data-active-theme="blue-sky""#));
    anyhow::ensure!(body.contains(r#"data-theme="blue-sky""#));
    Ok(())
}

#[test]
fn theme_init_uses_server_active_theme_before_local_storage() -> anyhow::Result<()> {
    let theme_init = include_str!("../../../static/theme-init.js");

    anyhow::ensure!(theme_init.contains("data-active-theme"));
    anyhow::ensure!(!theme_init.contains("localStorage.getItem('rustchan_theme')"));
    Ok(())
}

#[tokio::test]
async fn set_user_preferences_rejects_open_redirect_return_to() -> anyhow::Result<()> {
    install_preference_test_themes();
    let router = Router::new().route("/preferences", post(super::set_user_preferences));

    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/preferences")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .header(header::COOKIE, "csrf_token=csrf123")
                .body(Body::from(
                    "_csrf=csrf123&return_to=%2F%2Fevil.example%2F&theme=forest",
                ))
                .context("request")?,
        )
        .await
        .context("response")?;

    ensure_eq!(
        response
            .headers()
            .get(header::LOCATION)
            .and_then(|value| value.to_str().ok()),
        Some("/")
    );
    Ok(())
}

#[tokio::test]
async fn preference_specific_html_responses_vary_on_cookie() -> anyhow::Result<()> {
    let state = crate::test_support::app_state();
    let (_board_id, thread_id) = seed_board_with_thread(&state, "tech", "op")?;
    let router = activity_router(state);

    for uri in [
        "/".to_owned(),
        "/tech".to_owned(),
        "/tech/catalog".to_owned(),
        format!("/tech/thread/{thread_id}"),
    ] {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(uri)
                    .header(header::COOKIE, "rustchan_preferred_view=index")
                    .extension(crate::test_support::connect_info())
                    .body(Body::empty())
                    .context("request")?,
            )
            .await
            .context("response")?;

        ensure_eq!(response.status(), StatusCode::OK);
        anyhow::ensure!(
            response
                .headers()
                .get(header::VARY)
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| value
                    .split(',')
                    .any(|part| part.trim().eq_ignore_ascii_case("cookie"))),
            "missing Vary: Cookie for preference-specific response"
        );
    }
    Ok(())
}

#[tokio::test]
async fn thread_updates_nav_uses_cookie_preferences() -> anyhow::Result<()> {
    let state = crate::test_support::app_state();
    let conn = state.db.get().context("db connection")?;
    crate::db::create_board(&conn, "tech", "Tech", "", false).context("create sfw board")?;
    crate::db::create_board(&conn, "x", "Adult", "", true).context("create nsfw board")?;
    drop(conn);
    let (board_id, thread_id) = seed_board_with_thread(&state, "chat", "op")?;
    create_reply_on_thread(&state, board_id, thread_id, "reply")?;
    {
        let conn = state.db.get().context("db connection")?;
        crate::templates::set_live_boards(crate::db::get_all_boards(&conn).context("load boards")?);
    }
    let router = activity_router(state);

    let response = router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/chat/thread/{thread_id}/updates?since=0"))
                .header(
                    header::COOKIE,
                    "rustchan_hide_nsfw=1; rustchan_preferred_view=index",
                )
                .body(Body::empty())
                .context("request")?,
        )
        .await
        .context("response")?;

    ensure_eq!(response.status(), StatusCode::OK);
    anyhow::ensure!(response
        .headers()
        .get(header::VARY)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value
            .split(',')
            .any(|part| part.trim().eq_ignore_ascii_case("cookie"))));
    let body = response_body_string(response).await?;
    anyhow::ensure!(!body.contains("/catalog"));
    anyhow::ensure!(!body.contains(r">x</a>"));
    Ok(())
}

#[tokio::test]
async fn malformed_board_password_hash_renders_misconfiguration_message() -> anyhow::Result<()> {
    let state = crate::test_support::app_state();
    {
        let conn = state.db.get().context("db connection")?;
        crate::db::create_board(&conn, "broken", "Broken", "", false).context("create board")?;
        conn.execute(
            "UPDATE boards SET access_mode = ?1, access_password_hash = ?2 WHERE short_name = 'broken'",
            rusqlite::params!["view_password", "not-a-phc-string"],
        )
        .context("update board access")?;
    }

    let router = Router::new()
        .route("/{board}/unlock", post(super::unlock_board_access))
        .with_state(state);

    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/broken/unlock")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .header(header::COOKIE, "csrf_token=csrf123")
                .extension(crate::test_support::connect_info())
                .body(Body::from(
                    "password=anything&return_to=%2Fbroken%2Fcatalog&_csrf=csrf123",
                ))
                .context("request")?,
        )
        .await
        .context("unlock response")?;

    ensure_eq!(response.status(), StatusCode::FORBIDDEN);
    let body = String::from_utf8(
        to_bytes(response.into_body(), usize::MAX)
            .await
            .context("response body")?
            .to_vec(),
    )
    .context("utf8 body")?;
    anyhow::ensure!(
        body.contains("This board password is misconfigured. Please contact an administrator.")
    );
    Ok(())
}

#[tokio::test]
async fn unlock_board_access_rate_limits_repeated_failures() -> anyhow::Result<()> {
    let state = crate::test_support::app_state();
    {
        let conn = state.db.get().context("db connection")?;
        crate::db::create_board(&conn, "srate", "Secret", "", false).context("create board")?;
        let password_hash =
            crate::utils::crypto::hash_password("swordfish").context("hash password")?;
        conn.execute(
            "UPDATE boards SET access_mode = ?1, access_password_hash = ?2 WHERE short_name = 'srate'",
            rusqlite::params!["view_password", password_hash],
        )
        .context("update board access")?;
    }

    let router = Router::new()
        .route("/{board}/unlock", post(super::unlock_board_access))
        .with_state(state);

    for _ in 0..(super::BOARD_UNLOCK_FAIL_LIMIT - 1) {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/srate/unlock")
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .header(header::COOKIE, "csrf_token=csrf123")
                    .extension(crate::test_support::connect_info())
                    .body(Body::from(
                        "password=wrong&return_to=%2Fsrate%2Fcatalog&_csrf=csrf123",
                    ))
                    .context("request")?,
            )
            .await
            .context("response")?;
        ensure_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/srate/unlock")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .header(header::COOKIE, "csrf_token=csrf123")
                .extension(crate::test_support::connect_info())
                .body(Body::from(
                    "password=wrong&return_to=%2Fsrate%2Fcatalog&_csrf=csrf123",
                ))
                .context("request")?,
        )
        .await
        .context("response")?;

    ensure_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    anyhow::ensure!(
        response.headers().contains_key(header::RETRY_AFTER),
        "rate-limited unlock should advertise retry timing"
    );
    let body = String::from_utf8(
        to_bytes(response.into_body(), usize::MAX)
            .await
            .context("response body")?
            .to_vec(),
    )
    .context("utf8 body")?;
    anyhow::ensure!(body.contains("Too many incorrect board password attempts."));
    Ok(())
}

#[tokio::test]
async fn locked_board_media_requires_unlock() -> anyhow::Result<()> {
    let state = crate::test_support::app_state();
    {
        let conn = state.db.get().context("db connection")?;
        crate::db::create_board(&conn, "secret", "Secret", "", false).context("create board")?;
        let password_hash =
            crate::utils::crypto::hash_password("swordfish").context("hash password")?;
        conn.execute(
            "UPDATE boards SET access_mode = ?1, access_password_hash = ?2 WHERE short_name = 'secret'",
            rusqlite::params!["view_password", password_hash],
        )
        .context("update board access")?;
    }

    let router = Router::new()
        .route("/boards/{*media_path}", get(super::serve_board_media))
        .with_state(state);

    let response = router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/boards/secret/thumbs/example.webp")
                .body(Body::empty())
                .context("request")?,
        )
        .await
        .context("response")?;

    ensure_eq!(response.status(), StatusCode::FORBIDDEN);
    Ok(())
}

#[tokio::test]
async fn submit_appeal_is_rate_limited_to_one_open_window() -> anyhow::Result<()> {
    let state = crate::test_support::app_state();
    {
        let conn = state.db.get().context("db connection")?;
        crate::db::add_ban(
            &conn,
            &crate::utils::crypto::hash_ip("127.0.0.1", &crate::config::CONFIG.cookie_secret),
            "test ban",
            None,
        )
        .context("add ban")?;
    }

    let router = Router::new()
        .route("/appeal", post(super::submit_appeal))
        .with_state(state);
    let request = || -> anyhow::Result<Request<Body>> {
        Request::builder()
            .method("POST")
            .uri("/appeal")
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .header(header::COOKIE, "csrf_token=csrf123")
            .extension(crate::test_support::connect_info())
            .body(Body::from("reason=please+unban&_csrf=csrf123"))
            .context("request")
    };

    let first = router
        .clone()
        .oneshot(request()?)
        .await
        .context("first appeal")?;
    let first_body = String::from_utf8(
        to_bytes(first.into_body(), usize::MAX)
            .await
            .context("first body")?
            .to_vec(),
    )
    .context("first body utf8")?;
    anyhow::ensure!(first_body.contains("appeal has been submitted"));

    let second = router.oneshot(request()?).await.context("second appeal")?;
    let second_body = String::from_utf8(
        to_bytes(second.into_body(), usize::MAX)
            .await
            .context("second body")?
            .to_vec(),
    )
    .context("second body utf8")?;
    anyhow::ensure!(second_body.contains("already filed an appeal"));
    Ok(())
}
