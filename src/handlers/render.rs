use crate::{
    db,
    error::{AppError, Result},
    handlers::posting,
    models::{Board, Pagination, PollData, Thread, ThreadSummary},
    templates,
    utils::crypto::hash_ip,
};

pub struct BoardPageData {
    pub board: Board,
    pub pagination: Pagination,
    pub summaries: Vec<ThreadSummary>,
    pub is_admin: bool,
}

pub struct ThreadPageData {
    pub board: Board,
    pub thread: Thread,
    pub posts: Vec<crate::models::Post>,
    pub poll: Option<PollData>,
    pub is_admin: bool,
}

pub fn load_board_page_data(
    conn: &rusqlite::Connection,
    board_short: &str,
    page: i64,
    threads_per_page: i64,
    preview_replies: i64,
    admin_session_id: Option<&str>,
) -> Result<BoardPageData> {
    let is_admin = posting::is_admin_session(conn, admin_session_id);
    let board = db::get_board_by_short(conn, board_short)?
        .ok_or_else(|| AppError::NotFound(format!("Board /{board_short}/ not found")))?;
    let total = db::count_threads_for_board(conn, board.id)?;
    let pagination = Pagination::new(page, threads_per_page, total);
    let threads = db::get_threads_for_board(conn, board.id, threads_per_page, pagination.offset())?;
    let thread_ids = threads.iter().map(|thread| thread.id).collect::<Vec<_>>();
    let previews = db::get_preview_posts_for_threads(conn, &thread_ids, preview_replies)?;
    let summaries = threads
        .into_iter()
        .map(|thread| {
            let preview_posts = previews.get(&thread.id).cloned().unwrap_or_default();
            let omitted =
                (thread.reply_count - i64::try_from(preview_posts.len()).unwrap_or(0)).max(0);
            ThreadSummary {
                thread,
                preview_posts,
                omitted,
            }
        })
        .collect();
    Ok(BoardPageData {
        board,
        pagination,
        summaries,
        is_admin,
    })
}

pub fn render_board_page(
    data: &BoardPageData,
    csrf_token: &str,
    error: Option<&str>,
    current_theme: Option<&str>,
) -> String {
    let boards = templates::live_boards();
    let collapse_greentext = templates::live_collapse_greentext();
    templates::board_page(
        &data.board,
        &data.summaries,
        &data.pagination,
        csrf_token,
        boards.as_slice(),
        data.is_admin,
        error,
        current_theme,
        collapse_greentext,
    )
}

pub fn load_thread_page_data(
    conn: &rusqlite::Connection,
    board_short: &str,
    thread_id: i64,
    client_ip: &str,
    admin_session_id: Option<&str>,
    cookie_secret: &str,
) -> Result<ThreadPageData> {
    let is_admin = posting::is_admin_session(conn, admin_session_id);
    let board = db::get_board_by_short(conn, board_short)?
        .ok_or_else(|| AppError::NotFound(format!("Board /{board_short}/ not found")))?;
    let thread = db::get_thread(conn, thread_id)?
        .ok_or_else(|| AppError::NotFound(format!("Thread {thread_id} not found")))?;
    if thread.board_id != board.id {
        return Err(AppError::NotFound("Thread not found in this board.".into()));
    }
    let posts = db::get_posts_for_thread(conn, thread_id)?;
    let ip_hash = hash_ip(client_ip, cookie_secret);
    let poll = db::get_poll_for_thread(conn, thread_id, &ip_hash)?;
    Ok(ThreadPageData {
        board,
        thread,
        posts,
        poll,
        is_admin,
    })
}

pub fn render_thread_page(
    data: &ThreadPageData,
    csrf_token: &str,
    error: Option<&str>,
    current_theme: Option<&str>,
) -> String {
    let boards = templates::live_boards();
    let collapse_greentext = templates::live_collapse_greentext();
    templates::thread_page(
        &data.board,
        &data.thread,
        &data.posts,
        csrf_token,
        boards.as_slice(),
        data.is_admin,
        data.poll.as_ref(),
        error,
        current_theme,
        collapse_greentext,
    )
}
