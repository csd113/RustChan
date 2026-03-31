use crate::{
    db,
    error::{AppError, Result},
    utils::{
        crypto::new_deletion_token,
        sanitize::{
            apply_word_filters, escape_html, render_post_body, validate_body,
            validate_body_with_file, validate_name,
        },
        tripcode::parse_name_tripcode,
    },
};

pub fn is_admin_session(
    conn: &rusqlite::Connection,
    admin_session_id: Option<&str>,
) -> bool {
    admin_session_id
        .is_some_and(|sid| db::get_session(conn, sid).ok().flatten().is_some())
}

pub fn load_word_filters(conn: &rusqlite::Connection) -> Result<Vec<(String, String)>> {
    Ok(db::get_word_filters(conn)?
        .into_iter()
        .map(|f| (f.pattern, f.replacement))
        .collect())
}

pub fn resolve_post_identity(raw_name: &str, allow_tripcodes: bool) -> (String, Option<String>) {
    let (name, tripcode) = parse_name_tripcode(&validate_name(raw_name));
    let tripcode = if allow_tripcodes { tripcode } else { None };
    (name, tripcode)
}

pub fn build_post_body(
    raw_body: &str,
    has_file: bool,
    board_allows_media: bool,
    filters: &[(String, String)],
) -> Result<(String, String)> {
    let body_text = if board_allows_media {
        validate_body_with_file(raw_body, has_file).map_err(AppError::BadRequest)?
    } else {
        validate_body(raw_body)
            .map_err(AppError::BadRequest)?
            .to_string()
    };
    let filtered_body = apply_word_filters(&body_text, filters);
    let escaped_body = escape_html(&filtered_body);
    let body_html = render_post_body(&escaped_body);
    Ok((body_text, body_html))
}

pub fn resolve_deletion_token(raw_token: &str) -> String {
    if raw_token.trim().is_empty() {
        new_deletion_token()
    } else {
        raw_token.trim().chars().take(64).collect()
    }
}
