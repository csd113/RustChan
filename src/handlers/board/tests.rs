use axum::{
    body::{to_bytes, Body},
    http::{header, HeaderMap, Request, StatusCode},
    routing::{get, post},
    Router,
};
use axum_extra::extract::cookie::CookieJar;
use std::collections::HashMap;
use tower::ServiceExt as _;

fn seed_post_password_board(state: &crate::middleware::AppState) -> (i64, i64, i64) {
    let conn = state.db.get().expect("db connection");
    let board_id =
        crate::db::create_board(&conn, "secret", "Secret", "", false).expect("create board");
    let password_hash = crate::utils::crypto::hash_password("swordfish").expect("hash password");
    conn.execute(
        "UPDATE boards SET access_mode = ?1, access_password_hash = ?2, allow_editing = 1, allow_self_delete = 1 WHERE id = ?3",
        rusqlite::params!["post_password", password_hash, board_id],
    )
    .expect("update board access");
    let post = crate::db::NewPost {
        thread_id: 0,
        board_id,
        name: "anon".to_string(),
        tripcode: None,
        subject: Some("subject".to_string()),
        body: "protected posting body".to_string(),
        body_html: "protected posting body".to_string(),
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
        deletion_token: "edit-token".to_string(),
        is_op: true,
    };
    let poll = crate::db::threads::PollInsert {
        question: "pick one",
        options: &["yes".to_string(), "no".to_string()],
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
    .expect("create thread");
    let option_id: i64 = conn
        .query_row(
            "SELECT id FROM poll_options WHERE poll_id = ?1 ORDER BY id LIMIT 1",
            rusqlite::params![poll_id.expect("poll id")],
            |row| row.get(0),
        )
        .expect("poll option id");
    (thread_id, post_id, option_id)
}

fn set_new_activity_settings(
    state: &crate::middleware::AppState,
    homepage_enabled: bool,
    thread_enabled: bool,
) {
    let conn = state.db.get().expect("db connection");
    crate::db::set_site_setting(
        &conn,
        "homepage_new_thread_badges_enabled",
        if homepage_enabled { "1" } else { "0" },
    )
    .expect("set homepage activity setting");
    crate::db::set_site_setting(
        &conn,
        "thread_new_reply_badges_enabled",
        if thread_enabled { "1" } else { "0" },
    )
    .expect("set thread activity setting");
}

fn seed_board_with_thread(
    state: &crate::middleware::AppState,
    short_name: &str,
    body: &str,
) -> (i64, i64) {
    let conn = state.db.get().expect("db connection");
    let board_id =
        crate::db::create_board(&conn, short_name, "Board", "", false).expect("create board");
    crate::templates::set_live_boards(crate::db::get_all_boards(&conn).expect("load boards"));
    let post = crate::db::NewPost {
        thread_id: 0,
        board_id,
        name: "anon".to_string(),
        tripcode: None,
        subject: Some("subject".to_string()),
        body: body.to_string(),
        body_html: body.to_string(),
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
        deletion_token: "token".to_string(),
        is_op: true,
    };
    let (thread_id, _post_id, _) =
        crate::db::create_thread_with_optional_poll(&conn, board_id, None, &post, "", None, None)
            .expect("create thread");
    (board_id, thread_id)
}

fn create_thread_on_board(state: &crate::middleware::AppState, board_id: i64, body: &str) -> i64 {
    let conn = state.db.get().expect("db connection");
    let post = crate::db::NewPost {
        thread_id: 0,
        board_id,
        name: "anon".to_string(),
        tripcode: None,
        subject: Some("subject".to_string()),
        body: body.to_string(),
        body_html: body.to_string(),
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
        deletion_token: "token".to_string(),
        is_op: true,
    };
    let (thread_id, _post_id, _) =
        crate::db::create_thread_with_optional_poll(&conn, board_id, None, &post, "", None, None)
            .expect("create thread");
    thread_id
}

fn create_reply_on_thread(
    state: &crate::middleware::AppState,
    board_id: i64,
    thread_id: i64,
    body: &str,
) {
    let conn = state.db.get().expect("db connection");
    let reply = crate::db::NewPost {
        thread_id,
        board_id,
        name: "anon".to_string(),
        tripcode: None,
        subject: None,
        body: body.to_string(),
        body_html: body.to_string(),
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
        deletion_token: "token".to_string(),
        is_op: false,
    };
    crate::db::create_reply_with_thread_update(&conn, &reply, "", true, None)
        .expect("create reply");
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
            store.insert(name.to_string(), cookie_value.to_string());
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

async fn response_body_string(response: axum::response::Response) -> String {
    String::from_utf8(
        to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("response body")
            .to_vec(),
    )
    .expect("utf8 body")
}

#[test]
fn protected_board_without_password_hash_fails_closed() {
    let board = crate::models::Board {
        access_mode: crate::models::BoardAccessMode::ViewPassword,
        access_password_hash: String::new(),
        ..crate::test_fixtures::sample_board()
    };
    assert!(!super::can_view_board(&board, false, None));
    assert!(!super::can_post_to_board(&board, false, None));
}

#[tokio::test]
async fn post_password_board_remains_viewable_without_unlock() {
    let state = crate::test_support::app_state();
    let (thread_id, _, _) = seed_post_password_board(&state);

    let router = Router::new()
        .route("/{board}", get(super::board_index))
        .route("/{board}/catalog", get(super::catalog))
        .route(
            "/{board}/thread/{id}",
            get(crate::handlers::thread::view_thread),
        )
        .with_state(state);

    for uri in [
        "/secret".to_string(),
        "/secret/catalog".to_string(),
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
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);
    }
}

#[tokio::test]
async fn post_password_board_write_actions_require_unlock() {
    let state = crate::test_support::app_state();
    let (thread_id, post_id, option_id) = seed_post_password_board(&state);
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
                .expect("request"),
        )
        .await
        .expect("create response");
    assert_eq!(create_response.status(), StatusCode::SEE_OTHER);
    assert_eq!(
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
                .expect("request"),
        )
        .await
        .expect("reply response");
    assert_eq!(reply_response.status(), StatusCode::SEE_OTHER);
    assert_eq!(
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
                .expect("request"),
        )
        .await
        .expect("edit get response");
    assert_eq!(edit_get_response.status(), StatusCode::FORBIDDEN);

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
                            axum_extra::extract::cookie::CookieJar::new(),
                            "secret",
                            thread_id,
                            post_id,
                            "edit-token",
                            chrono::Utc::now().timestamp()
                                + crate::handlers::board::SELF_DELETE_WINDOW_SECS,
                        )
                        .get("rustchan_owned_posts")
                        .expect("owned posts cookie")
                        .value()
                    ),
                )
                .extension(crate::test_support::connect_info())
                .body(Body::from("body=changed&_csrf=csrf123"))
                .expect("request"),
        )
        .await
        .expect("edit post response");
    assert_eq!(edit_post_response.status(), StatusCode::SEE_OTHER);
    assert_eq!(
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
                .expect("request"),
        )
        .await
        .expect("vote response");
    assert_eq!(vote_response.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        vote_response
            .headers()
            .get(header::LOCATION)
            .and_then(|value| value.to_str().ok()),
        Some(format!("/secret/unlock?return_to=%2Fsecret%2Fthread%2F{thread_id}%23poll").as_str())
    );
}

#[tokio::test]
async fn self_delete_requires_owned_post_cookie() {
    let state = crate::test_support::app_state();
    let conn = state.db.get().expect("db connection");
    let board_id = crate::db::create_board(&conn, "test", "Test", "", false).expect("create board");
    conn.execute(
        "UPDATE boards SET allow_self_delete = 1 WHERE id = ?1",
        rusqlite::params![board_id],
    )
    .expect("enable self delete");
    let op = crate::db::NewPost {
        thread_id: 0,
        board_id,
        name: "anon".to_string(),
        tripcode: None,
        subject: Some("subject".to_string()),
        body: "body".to_string(),
        body_html: "body".to_string(),
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
        deletion_token: "op-token".to_string(),
        is_op: true,
    };
    let (thread_id, _op_id, _) =
        crate::db::create_thread_with_optional_poll(&conn, board_id, None, &op, "", None, None)
            .expect("create thread");
    let reply = crate::db::NewPost {
        thread_id,
        board_id,
        name: "anon".to_string(),
        tripcode: None,
        subject: None,
        body: "reply".to_string(),
        body_html: "reply".to_string(),
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
        deletion_token: "reply-token".to_string(),
        is_op: false,
    };
    let reply_id = crate::db::create_reply_with_thread_update(&conn, &reply, "", false, None)
        .expect("create reply");
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
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);

    let owned_cookie_jar = crate::handlers::board::remember_owned_post_until(
        axum_extra::extract::cookie::CookieJar::new(),
        "test",
        thread_id,
        reply_id,
        "reply-token",
        chrono::Utc::now().timestamp() + crate::handlers::board::SELF_DELETE_WINDOW_SECS,
    );
    let owned_cookie = owned_cookie_jar
        .get("rustchan_owned_posts")
        .expect("owned posts cookie");
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
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(allowed.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        allowed
            .headers()
            .get(header::LOCATION)
            .and_then(|value| value.to_str().ok()),
        Some(format!("/test/thread/{thread_id}").as_str())
    );
}

#[tokio::test]
async fn search_returns_results_without_500() {
    let state = crate::test_support::app_state();
    {
        let conn = state.db.get().expect("db connection");
        let board_id =
            crate::db::create_board(&conn, "test", "Test", "", false).expect("create board");
        let post = crate::db::NewPost {
            thread_id: 0,
            board_id,
            name: "anon".to_string(),
            tripcode: None,
            subject: Some("subject".to_string()),
            body: "rust search body".to_string(),
            body_html: "rust search body".to_string(),
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
            deletion_token: "token".to_string(),
            is_op: true,
        };
        crate::db::create_thread_with_optional_poll(&conn, board_id, None, &post, "", None, None)
            .expect("create thread");
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
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::OK);
    let body = String::from_utf8(
        to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("response body")
            .to_vec(),
    )
    .expect("utf8 body");
    assert!(body.contains("rust search body"));
}

#[tokio::test]
async fn search_without_q_param_returns_empty_results_page() {
    let state = crate::test_support::app_state();
    {
        let conn = state.db.get().expect("db connection");
        crate::db::create_board(&conn, "test", "Test", "", false).expect("create board");
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
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::OK);
    let body = String::from_utf8(
        to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("response body")
            .to_vec(),
    )
    .expect("utf8 body");
    assert!(body.contains("no results found."));
}

#[tokio::test]
async fn locked_board_search_returns_forbidden_unlock_page() {
    let state = crate::test_support::app_state();
    {
        let conn = state.db.get().expect("db connection");
        crate::db::create_board(&conn, "slock", "Secret", "", false).expect("create board");
        let password_hash =
            crate::utils::crypto::hash_password("swordfish").expect("hash password");
        conn.execute(
            "UPDATE boards SET access_mode = ?1, access_password_hash = ?2 WHERE short_name = 'slock'",
            rusqlite::params!["view_password", password_hash],
        )
        .expect("update board access");
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
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        response
            .headers()
            .get(header::CACHE_CONTROL)
            .and_then(|value| value.to_str().ok()),
        Some(super::HTML_CACHE_CONTROL)
    );
    let body = String::from_utf8(
        to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("response body")
            .to_vec(),
    )
    .expect("utf8 body");
    assert!(body.contains("action=\"/slock/unlock\""));
}

#[tokio::test]
async fn create_thread_accepts_valid_multipart_submission() {
    let state = crate::test_support::app_state();
    {
        let conn = state.db.get().expect("db connection");
        crate::db::create_board(&conn, "test", "Test", "", false).expect("create board");
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
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let location = response
        .headers()
        .get(header::LOCATION)
        .and_then(|value| value.to_str().ok())
        .expect("location header");
    assert!(location.starts_with("/test/thread/"));
}

#[tokio::test]
async fn create_thread_xhr_returns_explicit_redirect_header() {
    let state = crate::test_support::app_state();
    {
        let conn = state.db.get().expect("db connection");
        crate::db::create_board(&conn, "test", "Test", "", false).expect("create board");
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
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let redirect = response
        .headers()
        .get("x-rustchan-redirect")
        .and_then(|value| value.to_str().ok())
        .expect("xhr redirect header");
    assert!(redirect.starts_with("/test/thread/"));
}

#[tokio::test]
async fn create_thread_xhr_validation_failure_returns_json_error() {
    let state = crate::test_support::app_state();
    {
        let conn = state.db.get().expect("db connection");
        crate::db::create_board(&conn, "test", "Test", "", false).expect("create board");
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
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("application/json; charset=utf-8")
    );
    assert_eq!(
        response
            .headers()
            .get("x-rustchan-error-status")
            .and_then(|value| value.to_str().ok()),
        Some(StatusCode::UNPROCESSABLE_ENTITY.as_str())
    );

    let body = String::from_utf8(
        to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("response body")
            .to_vec(),
    )
    .expect("utf8 body");
    assert!(body.contains("\"error\""));
}

#[tokio::test]
async fn homepage_and_thread_badges_default_to_enabled() {
    let state = crate::test_support::app_state();
    let conn = state.db.get().expect("db connection");

    assert!(crate::db::get_homepage_new_thread_badges_enabled(&conn));
    assert!(crate::db::get_thread_new_reply_badges_enabled(&conn));
}

#[tokio::test]
async fn homepage_toggle_off_suppresses_only_homepage_badges() {
    let state = crate::test_support::app_state();
    set_new_activity_settings(&state, false, true);
    let (board_id, thread_id) = seed_board_with_thread(&state, "tech", "op");
    create_reply_on_thread(&state, board_id, thread_id, "reply");
    let router = activity_router(state);
    let cookie = format!(
        "rustchan_board_activity=v1|{board_id}.0.0.{}; rustchan_thread_activity=v1|{thread_id}.0.{}",
        chrono::Utc::now().timestamp(),
        chrono::Utc::now().timestamp()
    );

    let home_response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/")
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    let home_body = response_body_string(home_response).await;
    assert!(!home_body.contains("board-card-activity-badge"));

    let catalog_response = router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/tech/catalog")
                .header(header::COOKIE, &cookie)
                .extension(crate::test_support::connect_info())
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    let catalog_body = response_body_string(catalog_response).await;
    assert!(catalog_body.contains("catalog-activity-badge"));
}

#[tokio::test]
async fn thread_toggle_off_suppresses_only_thread_badges() {
    let state = crate::test_support::app_state();
    set_new_activity_settings(&state, true, false);
    let (board_id, thread_id) = seed_board_with_thread(&state, "tech", "op");
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
                .expect("request"),
        )
        .await
        .expect("response");
    update_cookie_store(&mut cookies, baseline.headers());

    create_thread_on_board(&state, board_id, "new thread");
    create_reply_on_thread(&state, board_id, thread_id, "reply");

    let home_response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/")
                .header(
                    header::COOKIE,
                    cookie_header(&cookies).expect("baseline cookies"),
                )
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    let home_body = response_body_string(home_response).await;
    assert!(home_body.contains("board-card-activity-badge"));

    let catalog_response = router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/tech/catalog")
                .header(
                    header::COOKIE,
                    cookie_header(&cookies).expect("baseline cookies"),
                )
                .extension(crate::test_support::connect_info())
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    let catalog_body = response_body_string(catalog_response).await;
    assert!(!catalog_body.contains("catalog-activity-badge"));
    assert!(!catalog_body.contains("thread-summary-activity-badge"));
}

#[tokio::test]
async fn thread_badge_markup_sits_between_catalog_info_and_counters() {
    let state = crate::test_support::app_state();
    set_new_activity_settings(&state, true, true);
    let (board_id, thread_id) = seed_board_with_thread(&state, "tech", "op");
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
                .expect("request"),
        )
        .await
        .expect("response");
    update_cookie_store(&mut cookies, baseline.headers());
    create_reply_on_thread(&state, board_id, thread_id, "reply");

    let response = router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/tech/catalog")
                .header(header::COOKIE, cookie_header(&cookies).expect("cookies"))
                .extension(crate::test_support::connect_info())
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    let body = response_body_string(response).await;
    let meta_idx = body
        .find("catalog-meta-row")
        .expect("catalog meta row present");
    let info_idx = body.find("catalog-info").expect("catalog info present");
    let badge_row_idx = body
        .find("catalog-activity-row")
        .expect("catalog badge row present");
    let badge_idx = body
        .find("catalog-activity-badge")
        .expect("catalog badge present");

    assert!(meta_idx < info_idx);
    assert!(info_idx < badge_row_idx);
    assert!(badge_idx > info_idx);
}

#[tokio::test]
async fn first_board_visit_establishes_quiet_activity_baseline() {
    let state = crate::test_support::app_state();
    set_new_activity_settings(&state, true, true);
    let (_board_id, _thread_id) = seed_board_with_thread(&state, "tech", "op");
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
                .expect("request"),
        )
        .await
        .expect("response");
    update_cookie_store(&mut cookies, response.headers());
    let body = response_body_string(response).await;
    assert!(!body.contains("catalog-activity-badge"));

    let home_response = router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/")
                .header(
                    header::COOKIE,
                    cookie_header(&cookies).expect("baseline cookies"),
                )
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    let home_body = response_body_string(home_response).await;
    assert!(!home_body.contains("board-card-activity-badge"));
}

#[tokio::test]
async fn new_thread_after_board_baseline_shows_homepage_badge() {
    let state = crate::test_support::app_state();
    set_new_activity_settings(&state, true, true);
    let (board_id, _thread_id) = seed_board_with_thread(&state, "tech", "op");
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
                .expect("request"),
        )
        .await
        .expect("response");
    update_cookie_store(&mut cookies, baseline.headers());

    create_thread_on_board(&state, board_id, "new thread");

    let response = router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/")
                .header(
                    header::COOKIE,
                    cookie_header(&cookies).expect("baseline cookies"),
                )
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    let body = response_body_string(response).await;
    assert!(body.contains("board-card-activity-badge"));
    assert!(body.contains(">1 New</span>"));
}

#[tokio::test]
async fn replies_alone_do_not_create_homepage_board_badge() {
    let state = crate::test_support::app_state();
    set_new_activity_settings(&state, true, true);
    let (board_id, thread_id) = seed_board_with_thread(&state, "tech", "op");
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
                .expect("request"),
        )
        .await
        .expect("response");
    update_cookie_store(&mut cookies, baseline.headers());

    create_reply_on_thread(&state, board_id, thread_id, "reply");

    let response = router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/")
                .header(
                    header::COOKIE,
                    cookie_header(&cookies).expect("baseline cookies"),
                )
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    let body = response_body_string(response).await;
    assert!(!body.contains("board-card-activity-badge"));
}

#[tokio::test]
async fn board_index_visit_clears_homepage_new_thread_badge() {
    let state = crate::test_support::app_state();
    set_new_activity_settings(&state, true, true);
    let (board_id, _thread_id) = seed_board_with_thread(&state, "tech", "op");
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
                .expect("request"),
        )
        .await
        .expect("response");
    update_cookie_store(&mut cookies, baseline.headers());
    create_thread_on_board(&state, board_id, "new thread");

    let clear_response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/tech")
                .header(header::COOKIE, cookie_header(&cookies).expect("cookies"))
                .extension(crate::test_support::connect_info())
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    update_cookie_store(&mut cookies, clear_response.headers());

    let home_response = router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/")
                .header(header::COOKIE, cookie_header(&cookies).expect("cookies"))
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    let body = response_body_string(home_response).await;
    assert!(!body.contains("board-card-activity-badge"));
}

#[tokio::test]
async fn board_catalog_visit_clears_homepage_new_thread_badge() {
    let state = crate::test_support::app_state();
    set_new_activity_settings(&state, true, true);
    let (board_id, _thread_id) = seed_board_with_thread(&state, "tech", "op");
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
                .expect("request"),
        )
        .await
        .expect("response");
    update_cookie_store(&mut cookies, baseline.headers());
    create_thread_on_board(&state, board_id, "new thread");

    let clear_response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/tech/catalog")
                .header(header::COOKIE, cookie_header(&cookies).expect("cookies"))
                .extension(crate::test_support::connect_info())
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    update_cookie_store(&mut cookies, clear_response.headers());

    let home_response = router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/")
                .header(header::COOKIE, cookie_header(&cookies).expect("cookies"))
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    let body = response_body_string(home_response).await;
    assert!(!body.contains("board-card-activity-badge"));
}

#[tokio::test]
async fn thread_visit_clears_homepage_new_thread_badge() {
    let state = crate::test_support::app_state();
    set_new_activity_settings(&state, true, true);
    let (board_id, thread_id) = seed_board_with_thread(&state, "tech", "op");
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
                .expect("request"),
        )
        .await
        .expect("response");
    update_cookie_store(&mut cookies, baseline.headers());
    create_thread_on_board(&state, board_id, "new thread");

    let clear_response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/tech/thread/{thread_id}"))
                .header(header::COOKIE, cookie_header(&cookies).expect("cookies"))
                .extension(crate::test_support::connect_info())
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    update_cookie_store(&mut cookies, clear_response.headers());

    let home_response = router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/")
                .header(header::COOKIE, cookie_header(&cookies).expect("cookies"))
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    let body = response_body_string(home_response).await;
    assert!(!body.contains("board-card-activity-badge"));
}

#[tokio::test]
async fn new_reply_after_thread_baseline_shows_thread_badge_until_thread_visit() {
    let state = crate::test_support::app_state();
    set_new_activity_settings(&state, true, true);
    let (board_id, thread_id) = seed_board_with_thread(&state, "tech", "op");
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
                .expect("request"),
        )
        .await
        .expect("response");
    update_cookie_store(&mut cookies, baseline.headers());

    create_reply_on_thread(&state, board_id, thread_id, "reply");

    let badge_response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/tech/catalog")
                .header(header::COOKIE, cookie_header(&cookies).expect("cookies"))
                .extension(crate::test_support::connect_info())
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    let badge_body = response_body_string(badge_response).await;
    assert!(badge_body.contains("catalog-activity-badge"));
    assert!(badge_body.contains(">1 New</span>"));

    let clear_response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/tech/thread/{thread_id}"))
                .header(header::COOKIE, cookie_header(&cookies).expect("cookies"))
                .extension(crate::test_support::connect_info())
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    update_cookie_store(&mut cookies, clear_response.headers());

    let cleared_catalog = router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/tech/catalog")
                .header(header::COOKIE, cookie_header(&cookies).expect("cookies"))
                .extension(crate::test_support::connect_info())
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    let cleared_body = response_body_string(cleared_catalog).await;
    assert!(!cleared_body.contains("catalog-activity-badge"));
}

#[tokio::test]
async fn password_protected_board_does_not_leak_homepage_new_activity_badge() {
    let state = crate::test_support::app_state();
    set_new_activity_settings(&state, true, true);
    let (board_id, _thread_id) = seed_board_with_thread(&state, "secret", "op");
    {
        let conn = state.db.get().expect("db connection");
        let password_hash =
            crate::utils::crypto::hash_password("swordfish").expect("hash password");
        conn.execute(
            "UPDATE boards SET access_mode = ?1, access_password_hash = ?2 WHERE id = ?3",
            rusqlite::params!["view_password", password_hash, board_id],
        )
        .expect("update board access");
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
                .expect("request"),
        )
        .await
        .expect("response");
    let body = response_body_string(response).await;
    assert!(!body.contains("board-card-activity-badge"));
}

#[tokio::test]
async fn new_activity_pages_keep_private_cache_headers() {
    let state = crate::test_support::app_state();
    set_new_activity_settings(&state, true, true);
    let (_board_id, _thread_id) = seed_board_with_thread(&state, "tech", "op");
    let router = activity_router(state);

    let home_response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(
        home_response
            .headers()
            .get(header::CACHE_CONTROL)
            .and_then(|value| value.to_str().ok()),
        Some(super::HTML_CACHE_CONTROL)
    );

    let catalog_response = router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/tech/catalog")
                .extension(crate::test_support::connect_info())
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(
        catalog_response
            .headers()
            .get(header::CACHE_CONTROL)
            .and_then(|value| value.to_str().ok()),
        Some(super::HTML_CACHE_CONTROL)
    );
}

#[tokio::test]
async fn catalog_baseline_tracks_only_highest_priority_threads_within_cookie_limit() {
    let state = crate::test_support::app_state();
    set_new_activity_settings(&state, true, true);
    let (board_id, first_thread_id) = seed_board_with_thread(&state, "tech", "op");
    let mut created_thread_ids = vec![first_thread_id];
    for index in 0..120 {
        created_thread_ids.push(create_thread_on_board(
            &state,
            board_id,
            &format!("thread {index}"),
        ));
    }
    let router = activity_router(state);

    let response = router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/tech/catalog")
                .extension(crate::test_support::connect_info())
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");

    let cookie_value = response
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find_map(|value| {
            value
                .split(';')
                .next()
                .and_then(|pair| pair.split_once('='))
                .and_then(|(name, cookie_value)| {
                    (name == "rustchan_thread_activity").then(|| cookie_value.to_string())
                })
        })
        .expect("thread activity cookie");
    let mut cookie_headers = HeaderMap::new();
    cookie_headers.insert(
        header::COOKIE,
        format!("rustchan_thread_activity={cookie_value}")
            .parse()
            .expect("cookie header"),
    );
    let jar = CookieJar::from_headers(&cookie_headers);
    let markers = super::thread_activity_markers_from_jar(&jar);

    assert_eq!(markers.len(), super::THREAD_ACTIVITY_MARKER_LIMIT);

    let expected_tracked = created_thread_ids
        .iter()
        .rev()
        .take(super::THREAD_ACTIVITY_MARKER_LIMIT)
        .copied()
        .collect::<Vec<_>>();
    for thread_id in expected_tracked {
        assert!(
            markers.contains_key(&thread_id),
            "expected tracked thread marker for {thread_id}"
        );
    }
    assert!(
        !markers.contains_key(&first_thread_id),
        "oldest catalog thread should not displace newer visible threads"
    );
}

#[tokio::test]
async fn create_thread_xhr_banned_user_redirects_to_banned_page() {
    let state = crate::test_support::app_state();
    {
        let conn = state.db.get().expect("db connection");
        crate::db::create_board(&conn, "test", "Test", "", false).expect("create board");
        crate::db::add_ban(
            &conn,
            &crate::utils::crypto::hash_ip("127.0.0.1", &crate::config::CONFIG.cookie_secret),
            "testing ban",
            None,
        )
        .expect("add ban");
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
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        response
            .headers()
            .get("x-rustchan-redirect")
            .and_then(|value| value.to_str().ok()),
        Some(super::banned_page_redirect_url("testing ban").as_str())
    );
}

#[tokio::test]
async fn create_thread_xhr_captcha_failure_returns_inline_json_error() {
    let state = crate::test_support::app_state();
    {
        let conn = state.db.get().expect("db connection");
        crate::db::create_board(&conn, "test", "Test", "", false).expect("create board");
        conn.execute(
            "UPDATE boards SET allow_captcha = 1 WHERE short_name = 'test'",
            [],
        )
        .expect("enable captcha");
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
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("application/json; charset=utf-8")
    );
    assert_eq!(
        response
            .headers()
            .get("x-rustchan-error-status")
            .and_then(|value| value.to_str().ok()),
        Some(StatusCode::UNPROCESSABLE_ENTITY.as_str())
    );

    let body = String::from_utf8(
        to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("response body")
            .to_vec(),
    )
    .expect("utf8 body");
    assert!(body.contains("CAPTCHA verification failed"));
}

#[tokio::test]
async fn duplicate_report_redirects_back_without_500() {
    let state = crate::test_support::app_state();
    let (thread_id, post_id) = {
        let conn = state.db.get().expect("db connection");
        let board_id =
            crate::db::create_board(&conn, "test", "Test", "", false).expect("create board");
        let post = crate::db::NewPost {
            thread_id: 0,
            board_id,
            name: "anon".to_string(),
            tripcode: None,
            subject: Some("subject".to_string()),
            body: "report me".to_string(),
            body_html: "report me".to_string(),
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
            deletion_token: "token".to_string(),
            is_op: true,
        };
        let (thread_id, post_id, _) = crate::db::create_thread_with_optional_poll(
            &conn, board_id, None, &post, "", None, None,
        )
        .expect("create thread");
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
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        let location = response
            .headers()
            .get(header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .expect("location header");
        assert_eq!(
            location,
            format!("/test/thread/{thread_id}?reported=1#p{post_id}")
        );
    }

    let open_reports = {
        let conn = state.db.get().expect("db connection");
        conn.query_row(
            "SELECT COUNT(*) FROM reports WHERE post_id = ?1 AND status = 'open'",
            rusqlite::params![post_id],
            |row| row.get::<_, i64>(0),
        )
        .expect("open report count")
    };
    assert_eq!(open_reports, 1);
}

#[tokio::test]
async fn create_thread_rejects_uploads_on_upload_disabled_board() {
    let state = crate::test_support::app_state();
    {
        let mut conn = state.db.get().expect("db connection");
        crate::db::create_board(&conn, "test", "Test", "", false).expect("create board");
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
        .expect("update board settings");
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
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn view_locked_catalog_renders_unlock_page() {
    let state = crate::test_support::app_state();
    {
        let conn = state.db.get().expect("db connection");
        crate::db::create_board(&conn, "secret", "Secret", "", false).expect("create board");
        let password_hash =
            crate::utils::crypto::hash_password("swordfish").expect("hash password");
        conn.execute(
            "UPDATE boards SET access_mode = ?1, access_password_hash = ?2 WHERE short_name = 'secret'",
            rusqlite::params!["view_password", password_hash],
        )
        .expect("update board access");
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
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let body = String::from_utf8(
        to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("response body")
            .to_vec(),
    )
    .expect("utf8 body");
    assert!(body.contains("password protected board"));
    assert!(body.contains("action=\"/secret/unlock\""));
}

#[tokio::test]
async fn unlock_board_access_sets_cookie_and_redirects() {
    let state = crate::test_support::app_state();
    {
        let conn = state.db.get().expect("db connection");
        crate::db::create_board(&conn, "secret", "Secret", "", false).expect("create board");
        let password_hash =
            crate::utils::crypto::hash_password("swordfish").expect("hash password");
        conn.execute(
            "UPDATE boards SET access_mode = ?1, access_password_hash = ?2 WHERE short_name = 'secret'",
            rusqlite::params!["view_password", password_hash],
        )
        .expect("update board access");
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
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(
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
        .expect("board access cookie");
    assert!(set_cookie.contains("HttpOnly"));
}

#[tokio::test]
async fn unlock_board_access_rejects_malformed_return_to_and_uses_board_default() {
    let state = crate::test_support::app_state();
    {
        let conn = state.db.get().expect("db connection");
        crate::db::create_board(&conn, "secret", "Secret", "", false).expect("create board");
        let password_hash =
            crate::utils::crypto::hash_password("swordfish").expect("hash password");
        conn.execute(
            "UPDATE boards SET access_mode = ?1, access_password_hash = ?2 WHERE short_name = 'secret'",
            rusqlite::params!["view_password", password_hash],
        )
        .expect("update board access");
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
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        response
            .headers()
            .get(header::LOCATION)
            .and_then(|value| value.to_str().ok()),
        Some("/secret/catalog")
    );
}

#[tokio::test]
async fn changing_board_password_invalidates_existing_unlock_cookie() {
    let state = crate::test_support::app_state();
    {
        let conn = state.db.get().expect("db connection");
        crate::db::create_board(&conn, "secret", "Secret", "", false).expect("create board");
        let password_hash =
            crate::utils::crypto::hash_password("swordfish").expect("hash password");
        conn.execute(
            "UPDATE boards SET access_mode = ?1, access_password_hash = ?2 WHERE short_name = 'secret'",
            rusqlite::params!["view_password", password_hash],
        )
        .expect("update board access");
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
                .expect("request"),
        )
        .await
        .expect("unlock response");
    let access_cookie = response
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find(|value| value.contains(&super::board_access_cookie_name("secret")))
        .and_then(|value| value.split(';').next())
        .expect("board access cookie")
        .to_string();

    {
        let conn = state.db.get().expect("db connection");
        let password_hash = crate::utils::crypto::hash_password("newpass").expect("hash password");
        conn.execute(
            "UPDATE boards SET access_password_hash = ?1 WHERE short_name = 'secret'",
            rusqlite::params![password_hash],
        )
        .expect("change board password");
    }

    let response = router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/secret/catalog")
                .header(header::COOKIE, access_cookie)
                .extension(crate::test_support::connect_info())
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("catalog response");

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn theme_redirect_rejects_external_referer_fallback() {
    let router = Router::new()
        .route("/theme/{theme}", get(crate::handlers::board::set_theme))
        .with_state(crate::test_support::app_state());

    let response = router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/theme/forest")
                .header(header::REFERER, "https://evil.example/secret/catalog")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        response
            .headers()
            .get(header::LOCATION)
            .and_then(|value| value.to_str().ok()),
        Some("/")
    );
}

#[tokio::test]
async fn malformed_board_password_hash_renders_misconfiguration_message() {
    let state = crate::test_support::app_state();
    {
        let conn = state.db.get().expect("db connection");
        crate::db::create_board(&conn, "broken", "Broken", "", false).expect("create board");
        conn.execute(
            "UPDATE boards SET access_mode = ?1, access_password_hash = ?2 WHERE short_name = 'broken'",
            rusqlite::params!["view_password", "not-a-phc-string"],
        )
        .expect("update board access");
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
                .expect("request"),
        )
        .await
        .expect("unlock response");

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let body = String::from_utf8(
        to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("response body")
            .to_vec(),
    )
    .expect("utf8 body");
    assert!(body.contains("This board password is misconfigured. Please contact an administrator."));
}

#[tokio::test]
async fn unlock_board_access_rate_limits_repeated_failures() {
    let state = crate::test_support::app_state();
    {
        let conn = state.db.get().expect("db connection");
        crate::db::create_board(&conn, "srate", "Secret", "", false).expect("create board");
        let password_hash =
            crate::utils::crypto::hash_password("swordfish").expect("hash password");
        conn.execute(
            "UPDATE boards SET access_mode = ?1, access_password_hash = ?2 WHERE short_name = 'srate'",
            rusqlite::params!["view_password", password_hash],
        )
        .expect("update board access");
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
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
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
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(
        response.headers().contains_key(header::RETRY_AFTER),
        "rate-limited unlock should advertise retry timing"
    );
    let body = String::from_utf8(
        to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("response body")
            .to_vec(),
    )
    .expect("utf8 body");
    assert!(body.contains("Too many incorrect board password attempts."));
}

#[tokio::test]
async fn locked_board_media_requires_unlock() {
    let state = crate::test_support::app_state();
    {
        let conn = state.db.get().expect("db connection");
        crate::db::create_board(&conn, "secret", "Secret", "", false).expect("create board");
        let password_hash =
            crate::utils::crypto::hash_password("swordfish").expect("hash password");
        conn.execute(
            "UPDATE boards SET access_mode = ?1, access_password_hash = ?2 WHERE short_name = 'secret'",
            rusqlite::params!["view_password", password_hash],
        )
        .expect("update board access");
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
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn submit_appeal_is_rate_limited_to_one_open_window() {
    let state = crate::test_support::app_state();
    {
        let conn = state.db.get().expect("db connection");
        crate::db::add_ban(
            &conn,
            &crate::utils::crypto::hash_ip("127.0.0.1", &crate::config::CONFIG.cookie_secret),
            "test ban",
            None,
        )
        .expect("add ban");
    }

    let router = Router::new()
        .route("/appeal", post(super::submit_appeal))
        .with_state(state);
    let request = || {
        Request::builder()
            .method("POST")
            .uri("/appeal")
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .header(header::COOKIE, "csrf_token=csrf123")
            .extension(crate::test_support::connect_info())
            .body(Body::from("reason=please+unban&_csrf=csrf123"))
            .expect("request")
    };

    let first = router
        .clone()
        .oneshot(request())
        .await
        .expect("first appeal");
    let first_body = String::from_utf8(
        to_bytes(first.into_body(), usize::MAX)
            .await
            .expect("first body")
            .to_vec(),
    )
    .expect("first body utf8");
    assert!(first_body.contains("appeal has been submitted"));

    let second = router.oneshot(request()).await.expect("second appeal");
    let second_body = String::from_utf8(
        to_bytes(second.into_body(), usize::MAX)
            .await
            .expect("second body")
            .to_vec(),
    )
    .expect("second body utf8");
    assert!(second_body.contains("already filed an appeal"));
}
