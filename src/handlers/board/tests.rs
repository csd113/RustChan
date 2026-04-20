use axum::{
    body::{to_bytes, Body},
    http::{header, Request, StatusCode},
    routing::{get, post},
    Router,
};
use tower::ServiceExt as _;

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
