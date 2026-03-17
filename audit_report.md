# RustChan — Comprehensive Security & Code Audit Report

**Audited:** `src/` (Rust imageboard server)  
**Date:** 2026-03-17  
**Auditor:** Static analysis + manual review  

---

## Summary

| Severity | Count |
|----------|-------|
| **Critical** | 3 |
| **High** | 8 |
| **Medium** | 12 |
| **Low** | 11 |
| **Total** | **34** |

**Overall assessment:** The codebase demonstrates strong security awareness — parameterized queries throughout, Argon2id password hashing, constant-time CSRF comparison, EXIF stripping, path-traversal guards, and extensive inline documentation of past fixes. However, several real production risks remain, including an unverified federation layer, a process-management bug that makes the ffmpeg timeout deceptive, and stored XSS via SVG uploads.

---

## Complete Issue Index

| # | Severity | Title | File |
|---|----------|-------|------|
| 1 | **Critical** | SVG served inline — stored XSS | `handlers/board.rs:753` |
| 2 | **Critical** | ChanNet Ed25519 signatures not verified | `chan_net/import.rs:~90` |
| 3 | **Critical** | ffmpeg process not killed on timeout | `workers/mod.rs:460` |
| 4 | **High** | Swapped format args in mod log entry | `handlers/admin/moderation.rs:183` |
| 5 | **High** | Unbounded body on admin restore endpoints | `server/server.rs:598,606` |
| 6 | **High** | `edit_post` misuses `execute_batch` for transactions | `db/posts.rs:~190` |
| 7 | **High** | `insert_board_if_absent` uses `last_insert_rowid()` | `db/chan_net.rs:~57` |
| 8 | **High** | Catalog has no ETag caching | `handlers/board.rs:500` |
| 9 | **High** | ChanNet `/chan/refresh` and `/chan/poll` unauthenticated | `chan_net/mod.rs:146` |
| 10 | **High** | Worker `JoinHandle`s discarded; shutdown is blind sleep | `server/server.rs:239` |
| 11 | **High** | `get_per_board_stats` still uses correlated subqueries | `db/boards.rs:416` |
| 12 | **Medium** | PoW nonce prune not rate-gated under concurrency | `utils/crypto.rs:~165` |
| 13 | **Medium** | `constant_time_eq` in posts.rs — use `subtle` crate | `db/posts.rs:~270` |
| 14 | **Medium** | `update_settings_file_site_names` matches comment lines | `config.rs:~320` |
| 15 | **Medium** | `admin_ban_and_delete` ban+delete not transactional | `handlers/admin/moderation.rs:~160` |
| 16 | **Medium** | Poll expiry not re-checked inside `cast_vote` | `db/posts.rs:cast_vote` |
| 17 | **Medium** | `has_recent_appeal` TOCTOU — no schema constraint | `db/admin.rs` |
| 18 | **Medium** | Gateway `insert_reply_into_thread` stores unescaped HTML | `db/chan_net.rs:~130` |
| 19 | **Medium** | Admin restore doesn't validate SQLite magic bytes | `handlers/admin/backup.rs` |
| 20 | **Medium** | Rate-limit IP hash uses hardcoded salt `"G"` not `cookie_secret` | `middleware/mod.rs:~155` |
| 21 | **Medium** | `thread_updates` builds JSON by string interpolation | `handlers/thread.rs:~360` |
| 22 | **Medium** | `Content-Disposition` header injection via `board` field | `chan_net/command.rs:140` |
| 23 | **Medium** | `pool_size` hardcoded despite comment claiming config | `db/mod.rs:143` |
| 24 | **Medium** | `classify_upload_error` fragile string-prefix matching | `handlers/mod.rs` |
| 25 | **Low** | `edit_post` `edit_window_secs=0` semantic mismatch | `db/posts.rs:~200` |
| 26 | **Low** | `delete_file` silently ignores filesystem errors | `utils/files.rs` |
| 27 | **Low** | `sanitize_filename` truncates by char count, not bytes | `utils/sanitize.rs` |
| 28 | **Low** | Tripcode uses unsalted SHA-256 | `utils/tripcode.rs` |
| 29 | **Low** | `ffmpeg` encoder detection makes 3 separate subprocess calls | `media/ffmpeg.rs` |
| 30 | **Low** | `encode_q` duplicated twice in backup.rs | `handlers/admin/backup.rs` |
| 31 | **Low** | `collect_thread_file_paths` unbounded SQLite variable count | `db/threads.rs:~75` |
| 32 | **Low** | `prune_login_fails` uses `Ordering::Relaxed` store | `handlers/admin/auth.rs:~83` |
| 33 | **Low** | Log file never rotated — will grow unbounded | `logging.rs:~35` |
| 34 | **Low** | `ACTIVE_IPS` prune clears entire map on first run after start | `server/server.rs:~305` |

---

## Detailed Findings

---

### [Critical] #1 — SVG Served Inline Without `Content-Disposition: attachment`

**File:** `src/handlers/board.rs:753` and `src/utils/files.rs` (`detect_mime_type`)

**Problem:**
SVG files are accepted as uploads (`image/svg+xml` is explicitly allowed in `detect_mime_type`) and served back with `Content-Type: image/svg+xml` inline via the `media_content_type()` map. An SVG file can contain `<script>` tags, `onload=` event handlers, and JavaScript `href`s. When the browser receives `Content-Type: image/svg+xml` with no `Content-Disposition: attachment`, it renders the SVG as a top-level document and executes any embedded JavaScript — bypassing all of the application's HTML-escaping and CSP.

**Impact:**
Stored XSS via file upload. Any user who can post on a board can upload a crafted SVG, and anyone who views that attachment will execute attacker-controlled JavaScript in the forum's origin. The CSP (`script-src 'self'`) does **not** block inline scripts inside an SVG document served from `'self'`.

**Fix:**

Option A (recommended): Remove SVG from the allowed MIME types entirely. Imageboard SVG support is rarely needed and the attack surface is significant.

Option B: Force download by adding `Content-Disposition: attachment` for all SVG responses in `serve_board_media`:

```rust
if let Some("svg") = target.extension().and_then(|e| e.to_str()) {
    resp.headers_mut().insert(
        axum::http::header::CONTENT_DISPOSITION,
        axum::http::HeaderValue::from_static("attachment"),
    );
}
```

---

### [Critical] #2 — ChanNet Ed25519 Signatures Not Verified

**File:** `src/chan_net/import.rs:~90`

**Problem:**
The `do_import` function explicitly logs a warning and continues processing when a snapshot carries an Ed25519 signature — verification is not implemented. Any remote node can send arbitrary content (boards, posts) and it will be inserted into the `chan_net_posts` mirror table without any authenticity check.

```rust
// From import.rs — current behavior:
if let Some(ref sig) = metadata.signature {
    tracing::warn!("... verification not yet implemented; signature will not be checked ...");
}
// Processing continues regardless
```

**Impact:**
Any attacker who can reach the ChanNet listener (default `127.0.0.1:7070`) can inject arbitrary federation data. If this port is ever exposed (misconfiguration, container networking), the attack surface is zero-auth data injection. The inline comment itself says: *"Do NOT promote this instance to production without completing Ed25519 verification."*

**Fix:**
Reject signed snapshots until verification is implemented:

```rust
if metadata.signature.is_some() {
    return Err(AppError::BadRequest(
        "Ed25519 signature verification is not yet implemented. \
         Signed snapshots are rejected until Phase N is complete.".into()
    ));
}
```

---

### [Critical] #3 — ffmpeg Process Not Actually Killed on Timeout

**File:** `src/workers/mod.rs:460–476`

**Problem:**
The worker wraps `spawn_blocking` in `tokio::time::timeout(ffmpeg_timeout, spawn_blocking(...))`. When the timeout fires, Tokio stops polling the future — but the underlying OS process launched by `std::process::Command::output()` inside the blocking thread is **not killed**. It continues running until it finishes. The blocking thread also remains occupied. The log message `"ffmpeg killed"` is factually incorrect.

```rust
match timeout(
    ffmpeg_timeout,
    tokio::task::spawn_blocking(move || {
        transcode_video_inner(...)  // ← std::process::Command::output() blocks here
    }),
).await {
    Err(_elapsed) => {
        // "ffmpeg killed" log is wrong — the OS process is still running
        warn!("VideoTranscode: job ... timed out ... — ffmpeg killed");
    }
}
```

**Impact:**
On a pathological input file, every blocking thread becomes permanently occupied. After `blocking_threads` (default: CPUs×4) such events, all `spawn_blocking` tasks stall indefinitely, making the entire server unresponsive.

**Fix:**
Switch to `tokio::process::Command` with `kill_on_drop(true)`:

```rust
use tokio::process::Command;

let mut child = Command::new("ffmpeg")
    .args(args)
    .stderr(Stdio::piped())
    .kill_on_drop(true)  // kills the OS process when Child is dropped
    .spawn()?;

match timeout(ffmpeg_timeout, child.wait_with_output()).await {
    Ok(Ok(output)) => { /* check exit status */ }
    Err(_elapsed) => {
        let _ = child.kill().await;
        return Err(anyhow::anyhow!("ffmpeg timed out after {}s", timeout_secs));
    }
}
```

---

### [High] #4 — Format String Arguments Swapped in Mod Log Entry

**File:** `src/handlers/admin/moderation.rs:183`

**Problem:**
The `log_mod_action` detail string for `admin_ban_and_delete` has its format arguments reversed:

```rust
// BUG: "reason" and "ip_hash_log" are in each other's positions
&format!("inline ban — ip_hash={reason}… reason={}", &ip_hash_log),
```

The result is that the mod log records `ip_hash=<reason text>… reason=<first 8 chars of hash>`. This makes forensic reconstruction from the mod log impossible for ban+delete actions.

**Fix:**
```rust
&format!("inline ban — ip_hash={}… reason={}", &ip_hash_log, reason),
```

---

### [High] #5 — Unbounded Body on Admin Restore Endpoints

**File:** `src/server/server.rs:598,606`

**Problem:**
Both `admin_restore` and `board_restore` use `DefaultBodyLimit::disable()`, removing all upload size limits. The `copy_limited()` helper in `backup.rs` limits *extraction* size, but it runs after the full body is already buffered into memory by Axum.

```rust
post(crate::handlers::admin::admin_restore).layer(DefaultBodyLimit::disable()),
post(crate::handlers::admin::board_restore).layer(DefaultBodyLimit::disable()),
```

**Impact:**
An authenticated admin (or a session-hijack) can send a request of arbitrary size and exhaust server RAM before the handler runs.

**Fix:**
Set a large but bounded limit:
```rust
.layer(DefaultBodyLimit::max(20 * 1024 * 1024 * 1024)) // 20 GiB
```

---

### [High] #6 — `edit_post` Misuses `execute_batch` for Transactions

**File:** `src/db/posts.rs:~190`

**Problem:**
`edit_post` issues `conn.execute_batch("BEGIN IMMEDIATE")` / `"COMMIT"` / `"ROLLBACK"` manually on a `&rusqlite::Connection`. Rusqlite's internal transaction-tracking state is not updated by raw `execute_batch` calls. If another caller on the same pooled connection subsequently calls `unchecked_transaction()`, it can start a nested transaction on top of an already-committed one, producing `SQLITE_ERROR: cannot start a transaction within a transaction`.

**Fix:**
Use rusqlite's typed transaction API:

```rust
use rusqlite::TransactionBehavior;

let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
let row: Option<(String, i64)> = tx.query_row(
    "SELECT deletion_token, created_at FROM posts WHERE id = ?1",
    params![post_id],
    |r| Ok((r.get(0)?, r.get(1)?)),
).optional()?;
// ... rest of logic on tx ...
tx.commit()?;
```

---

### [High] #7 — `insert_board_if_absent` Uses `last_insert_rowid()` (Racy)

**File:** `src/db/chan_net.rs:~57`

**Problem:**
While `db/admin.rs` and `db/posts.rs` were audited and fixed to use `INSERT … RETURNING id`, `insert_board_if_absent` still uses `conn.last_insert_rowid()` after a plain `conn.execute(...)`. In a multi-connection SQLite pool, `last_insert_rowid()` is connection-local. If any other code on the same connection inserts a row between the board INSERT and `last_insert_rowid()`, the wrong ID is returned.

**Fix:**
```rust
let id: i64 = conn.query_row(
    "INSERT INTO boards (short_name, title, description, nsfw, max_threads, bump_limit)
     VALUES (?1, ?2, '', 0, 100, 300) RETURNING id",
    rusqlite::params![short_name, title],
    |r| r.get(0),
)?;
Ok(id)
```

---

### [High] #8 — Catalog Endpoint Has No ETag Caching

**File:** `src/handlers/board.rs:500`

**Problem:**
The catalog view calls `db::get_threads_for_board(&conn, board.id, 200, 0)` — always fetching up to 200 full thread rows — on every request. Unlike the board index and thread view, the catalog has no ETag computation or `304 Not Modified` path. Every catalog request performs a full DB scan and full HTML render regardless of whether anything changed.

**Impact:**
Under moderate load on an active board, the catalog is a significant repeated I/O and CPU cost with no caching benefit.

**Fix:**
Add ETag computation mirroring the board index handler:
```rust
let max_bump = threads.iter().map(|t| t.bumped_at).max().unwrap_or(0);
let admin_tag = if is_admin { "-a" } else { "" };
let etag = format!("\"{max_bump}-catalog{admin_tag}\"");

// Return 304 if client ETag matches
if req_headers.get("if-none-match").and_then(|v| v.to_str().ok()) == Some(&etag) {
    return Ok((jar, StatusCode::NOT_MODIFIED).into_response());
}
```

---

### [High] #9 — ChanNet `/chan/refresh` and `/chan/poll` Have No Authentication

**File:** `src/chan_net/mod.rs:146–168`

**Problem:**
The ChanNet router exposes `/chan/refresh` and `/chan/poll` with no authentication. Any process reaching the ChanNet bind address can trigger a full database snapshot + push to RustWave (`/chan/refresh`), or instruct the server to pull and import remote data (`/chan/poll`).

```rust
.route("/chan/refresh", post(refresh::chan_refresh))  // ← no auth
.route("/chan/poll",    post(poll::chan_poll))         // ← no auth
```

**Impact:**
Unauthenticated refresh is a CPU/IO-intensive operation triggerable by anyone on the network. Unauthenticated poll triggers inbound data import (which also lacks Ed25519 verification — see finding #2). An operator who sets `chan_net_bind = "0.0.0.0:7070"` exposes these publicly with zero access control.

**Fix:**
Add a pre-shared API key middleware on the ChanNet router:

```rust
async fn verify_chan_api_key(req: Request, next: Next) -> Response {
    let key = req.headers()
        .get("X-ChanNet-Key")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if key != CONFIG.chan_net_api_key {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    next.run(req).await
}
```

---

### [High] #10 — Worker `JoinHandle`s Discarded; Graceful Shutdown Is a Blind Sleep

**File:** `src/server/server.rs:239`, `src/workers/mod.rs:171`

**Problem:**
`start_worker_pool` explicitly documents that callers must hold and await its `Vec<JoinHandle<()>>` to know when in-flight jobs have finished. The server discards the return value entirely:

```rust
// workers/mod.rs docstring says:
// "Without holding these handles the caller has no way to know when
//  in-progress jobs have actually finished"

// server.rs actual code:
crate::workers::start_worker_pool(&q, ffmpeg_available, ffmpeg_vp9_available);
// ↑ return value silently dropped
```

Shutdown waits a hardcoded 10 seconds with no guarantee any worker has completed. If a video transcode takes 11 seconds, the process exits mid-write, leaving corrupted files and DB rows permanently stuck in `"running"` state.

**Fix:**
```rust
let worker_handles = crate::workers::start_worker_pool(...);

// In shutdown sequence:
worker_cancel.cancel();
for handle in worker_handles {
    let _ = tokio::time::timeout(Duration::from_secs(30), handle).await;
}
```

---

### [High] #11 — `get_per_board_stats` Uses Correlated Subqueries (N+1 in SQL)

**File:** `src/db/boards.rs:416`

**Problem:**
While `get_all_boards_with_stats` was correctly fixed to use a `LEFT JOIN`, `get_per_board_stats` (used by the terminal stats display) still uses two correlated subqueries per board row:

```sql
SELECT b.short_name,
    (SELECT COUNT(*) FROM threads WHERE board_id = b.id) AS tc,
    (SELECT COUNT(*) FROM posts p JOIN threads t ON p.thread_id = t.id
      WHERE t.board_id = b.id) AS pc
FROM boards b
```

For a forum with 20 boards this executes 41 SQL statements. The per-board post-count subquery joins the full posts table once per board.

**Fix:**
```sql
SELECT b.short_name,
       COUNT(DISTINCT t.id) AS tc,
       COUNT(DISTINCT p.id) AS pc
FROM boards b
LEFT JOIN threads t ON t.board_id = b.id
LEFT JOIN posts   p ON p.thread_id = t.id
GROUP BY b.id
ORDER BY b.short_name
```

---

### [Medium] #12 — PoW Nonce Cache Pruning Is Not Rate-Gated Under Concurrency

**File:** `src/utils/crypto.rs:~165`

**Problem:**
`verify_pow` calls `SEEN_NONCES.retain(...)` unconditionally before the per-shard `entry()` lock. Under high concurrency, many goroutines can call `retain` simultaneously, each acquiring and releasing individual DashMap shard locks in sequence. This is not atomic with the map as a whole and causes redundant prune work at high RPS.

**Fix:**
Guard the prune with a compare-exchange on a timestamp atomic:

```rust
static LAST_NONCE_PRUNE: std::sync::atomic::AtomicI64 = AtomicI64::new(0);

let last = LAST_NONCE_PRUNE.load(Ordering::Relaxed);
if now - last > POW_WINDOW_SECS {
    if LAST_NONCE_PRUNE
        .compare_exchange(last, now, Ordering::AcqRel, Ordering::Relaxed)
        .is_ok()
    {
        SEEN_NONCES.retain(|_, ts| now - *ts < POW_WINDOW_SECS);
    }
}
```

---

### [Medium] #13 — `constant_time_eq` in `db/posts.rs` — Use `subtle` Crate

**File:** `src/db/posts.rs:~270`

**Problem:**
The hand-rolled `constant_time_eq` uses a `u8::try_from(a.len() ^ b.len()).unwrap_or(u8::MAX)` to capture length differences. This is subtle and fragile: the `try_from` truncation can in theory produce false negatives for lengths differing by multiples of 256 (though not for the 32-char tokens in use today). A well-audited crate eliminates the risk entirely.

**Fix:**
```toml
# Cargo.toml
subtle = "2"
```

```rust
use subtle::ConstantTimeEq;

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.ct_eq(b).into()
}
```

---

### [Medium] #14 — `update_settings_file_site_names` Matches Comment Lines

**File:** `src/config.rs:~320`

**Problem:**
The line-matching logic uses:
```rust
if line.trim_start().starts_with("forum_name") && line.contains('=') {
```

This will incorrectly match a commented-out line like `# forum_name = "old value"`, silently activating a previously commented setting.

**Fix:**
```rust
if line.trim_start().starts_with("forum_name")
    && !line.trim_start().starts_with('#')
    && line.contains('=') {
```

---

### [Medium] #15 — `admin_ban_and_delete` Ban and Delete Not Transactional

**File:** `src/handlers/admin/moderation.rs:~160`

**Problem:**
The handler bans the IP hash, then deletes the post. These are two separate DB operations with no wrapping transaction. If `delete_post` or `delete_thread` fails after `add_ban` succeeds, the IP is now banned but the offending post still exists — inconsistent moderation state.

**Fix:**
Wrap both operations in a single rusqlite transaction:
```rust
let tx = conn.transaction()?;
db::add_ban_tx(&tx, &form.ip_hash, &reason, expires_at)?;
if is_op {
    db::delete_thread_tx(&tx, thread_id)?;
} else {
    db::delete_post_tx(&tx, post_id)?;
}
tx.commit()?;
```

---

### [Medium] #16 — Poll Expiry Not Re-Checked Inside `cast_vote`

**File:** `src/db/posts.rs:cast_vote`, `src/handlers/thread.rs:vote_handler`

**Problem:**
`vote_handler` checks `expires_at <= now` before calling `cast_vote`. But `cast_vote` does not re-verify expiry atomically. There is a TOCTOU window: the expiry check passes, the poll expires, and `cast_vote` inserts a vote for an expired poll.

**Fix:**
Add expiry validation inside `cast_vote`'s INSERT:

```sql
INSERT OR IGNORE INTO poll_votes (poll_id, option_id, ip_hash)
SELECT ?1, ?2, ?3
WHERE EXISTS (
    SELECT 1 FROM poll_options
    WHERE id = ?2 AND poll_id = ?1
)
AND EXISTS (
    SELECT 1 FROM polls
    WHERE id = ?1 AND expires_at > unixepoch()
)
```

---

### [Medium] #17 — `has_recent_appeal` / `file_ban_appeal` TOCTOU — No Schema Constraint

**File:** `src/db/admin.rs`

**Problem:**
The `has_recent_appeal` + `file_ban_appeal` pair has an acknowledged race condition: two concurrent requests from the same IP can both pass the duplicate check and both insert appeals. A schema-level constraint is the correct fix and has been deferred.

**Fix:**
Add a partial unique index to enforce the one-appeal-per-window rule at the DB level:
```sql
CREATE UNIQUE INDEX IF NOT EXISTS idx_ban_appeals_ip_open
ON ban_appeals(ip_hash) WHERE status = 'open';
```

Then use `INSERT OR IGNORE` in `file_ban_appeal` and check `conn.changes() == 0` to detect the duplicate case.

---

### [Medium] #18 — `insert_reply_into_thread` Stores Unescaped Content in `body_html`

**File:** `src/db/chan_net.rs:~130`

**Problem:**
The RustWave gateway writes plain `content` directly into both `body` and `body_html`:

```rust
// body_html = plain text content — the render pipeline is not invoked
rusqlite::params![thread_id, board_id, author, content,
                  content, // body_html = plain text content
                  deletion_token]
```

This relies on every template that renders `body_html` to HTML-escape it. If any template renders `body_html` as raw HTML (the normal path for locally created posts), gateway-inserted content with HTML special characters causes display corruption or stored XSS.

**Fix:**
Escape and render at the point of insertion:
```rust
use crate::utils::sanitize::{escape_html, render_post_body};
let escaped = escape_html(content);
let body_html = render_post_body(&escaped);
// Use body_html in the INSERT, not raw content
```

---

### [Medium] #19 — Admin Restore Does Not Validate SQLite Magic Bytes

**File:** `src/handlers/admin/backup.rs:~353`

**Problem:**
The restore handler extracts a file from the uploaded ZIP and opens it directly with `rusqlite::Connection::open()` without first verifying it is a valid SQLite file. Passing a non-SQLite binary to rusqlite invokes the underlying SQLite C library's parser on arbitrary bytes.

**Fix:**
```rust
const SQLITE_MAGIC: &[u8] = b"SQLite format 3\0";
let header = std::fs::read(&temp_db)?;
if !header.starts_with(SQLITE_MAGIC) {
    anyhow::bail!("Uploaded file is not a valid SQLite database");
}
```

---

### [Medium] #20 — Rate-Limit IP Hash Uses Hardcoded Salt `"G"` Instead of `cookie_secret`

**File:** `src/middleware/mod.rs:~155`

**Problem:**
The rate-limit table hashes IPs with a single hardcoded byte `"G"` as the salt:

```rust
let ip_key = {
    let mut h = Sha256::new();
    h.update(ip.as_bytes());
    h.update(b"G");  // ← hardcoded salt
    hex::encode(h.finalize())
};
```

This is inconsistent with `hash_ip()` in `utils/crypto.rs` which uses the `cookie_secret` as a proper salt. The hardcoded salt means IP hashes in the rate-limit table are reconstructible from a rainbow table with no knowledge of any application secret.

**Fix:**
Use the existing `hash_ip` function:
```rust
let ip_key = crate::utils::crypto::hash_ip(&ip, &CONFIG.cookie_secret);
```

---

### [Medium] #21 — `thread_updates` Builds JSON by String Interpolation

**File:** `src/handlers/thread.rs:~360`

**Problem:**
The auto-update endpoint response is built via `format!()` with raw value interpolation for integers and booleans. Only string fields go through `serde_json::to_string`. If any field type changes, the JSON silently becomes malformed.

```rust
let json = format!(
    r#"{{"html":{html_json},"last_id":{last_id},"count":{count},...}}"#,
    last_id = last_id,   // raw i64 interpolation
    locked = locked,     // raw bool interpolation
    ...
);
```

**Fix:**
Use a `serde`-derived struct:
```rust
#[derive(Serialize)]
struct UpdatesResponse {
    html: String,
    last_id: i64,
    count: usize,
    reply_count: i64,
    bump_time: i64,
    locked: bool,
    sticky: bool,
    boards_version: u64,
    nav_html: String,
}
let json = serde_json::to_string(&UpdatesResponse { html, last_id, ... })
    .unwrap_or_else(|_| r#"{"error":"serialization failed"}"#.to_string());
```

---

### [Medium] #22 — `Content-Disposition` Header Injection via Unsanitized `board` Field

**File:** `src/chan_net/command.rs:140,150,200`

**Problem:**
The `board` field from the JSON request body is interpolated directly into the `Content-Disposition` filename header without sanitization:

```rust
Ok((zip, format!("rustchan_board_{board}_{now}.zip")))
// ...
let disposition = format!("attachment; filename=\"{filename}\"");
```

A `board` value containing `"` breaks the quoted filename syntax. A value with CRLF sequences could inject additional HTTP headers in some parsers.

**Fix:**
```rust
let safe_board: String = board.chars()
    .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
    .take(32)
    .collect();
Ok((zip, format!("rustchan_board_{safe_board}_{now}.zip")))
```

---

### [Medium] #23 — `pool_size` Hardcoded Despite Comment Claiming Config Control

**File:** `src/db/mod.rs:143–148`

**Problem:**
The comment says the pool size comes from config so it can be tuned without recompiling, but the implementation hardcodes `8u32`:

```rust
// FIX[LOW-15]: Pool size comes from config so it can be tuned without recompiling.
// Falls back to 8 if not set.
let pool_size = 8u32;  // ← always 8, CONFIG not consulted
```

`CONFIG` has no `pool_size` field. On a server with `blocking_threads = 32`, all 32 threads may queue waiting for one of only 8 DB connections.

**Fix:**
Add `pool_size` to `SettingsFile`, `Config`, and `settings.toml`:
```rust
// config.rs
pub pool_size: u32, // default 8

// db/mod.rs
let pool_size = CONFIG.pool_size.clamp(2, 64);
```

---

### [Medium] #24 — `classify_upload_error` Uses Fragile Prefix-String Matching

**File:** `src/handlers/mod.rs`

**Problem:**
HTTP status code mapping depends on error message string prefixes:

```rust
if lower.starts_with("file too large") || lower.starts_with("insufficient disk space") {
    AppError::UploadTooLarge(msg)
} else if lower.starts_with("file type not allowed") || lower.starts_with("not an audio file") {
    AppError::InvalidMediaType(msg)
}
```

Any rewording in `save_upload` or `detect_mime_type` silently changes the HTTP status code returned to clients. There are no tests asserting these prefix strings match.

**Fix:**
Define a typed error enum in `utils/files.rs` and use `downcast_ref`:
```rust
#[derive(thiserror::Error, Debug)]
pub enum UploadError {
    #[error("File too large: {0}")]  TooLarge(String),
    #[error("File type not allowed: {0}")] InvalidMime(String),
    #[error("{0}")] Other(String),
}

match e.downcast_ref::<UploadError>() {
    Some(UploadError::TooLarge(m)) => AppError::UploadTooLarge(m.clone()),
    Some(UploadError::InvalidMime(m)) => AppError::InvalidMediaType(m.clone()),
    _ => AppError::BadRequest(e.to_string()),
}
```

---

### [Low] #25 — `edit_post` `edit_window_secs=0` Semantic Mismatch

**File:** `src/db/posts.rs:~200` vs `src/handlers/thread.rs:edit_post_get`

**Problem:**
The GET handler correctly treats `edit_window_secs = 0` as "no restriction — always editable." But the DB-layer `edit_post` function silently enforces a 5-minute default when `0` is passed:

```rust
// db/posts.rs — WRONG: treats 0 as "use 300s default"
let window = if edit_window_secs <= 0 { 300 } else { edit_window_secs };
```

Users on boards with `edit_window_secs = 0` will see the edit form but have their edits rejected after 5 minutes.

**Fix:**
```rust
// Treat 0 as unlimited
let window = if edit_window_secs <= 0 { i64::MAX } else { edit_window_secs };
```

---

### [Low] #26 — `delete_file` Silently Ignores Filesystem Errors

**File:** `src/utils/files.rs`

**Problem:**
```rust
let _ = std::fs::remove_file(full_path);
```

Permission errors, broken filesystems, or partial deletes are silently discarded. Orphaned files accumulate with no visibility.

**Fix:**
```rust
if let Err(e) = std::fs::remove_file(&full_path) {
    if e.kind() != std::io::ErrorKind::NotFound {
        tracing::warn!("Failed to delete file {:?}: {}", full_path, e);
    }
}
```

---

### [Low] #27 — `sanitize_filename` Truncates by Character Count, Not Byte Count

**File:** `src/utils/sanitize.rs`

**Problem:**
```rust
name.chars().take(100).collect()
```

100 CJK characters is 300 UTF-8 bytes. Some filesystems (ext4 path component limit: 255 bytes) reject names longer than 255 bytes. Since filenames here are display-only, this is cosmetic but worth fixing.

**Fix:**
```rust
let s: String = name.chars().take(100).collect();
if s.len() <= 200 { return s; }
let mut end = 200;
while end > 0 && !s.is_char_boundary(end) { end -= 1; }
s[..end].to_string()
```

---

### [Low] #28 — Tripcodes Use Unsalted SHA-256

**File:** `src/utils/tripcode.rs`

**Problem:**
Tripcodes are computed as `SHA-256(password)[..10 chars base64url]` with no application-specific salt. The file's own security note acknowledges this: rainbow tables for common passwords are portable across all RustChan instances, enabling cross-instance identity correlation.

**Fix:**
Use HMAC-SHA256 with `cookie_secret` as the key:
```rust
use hmac::{Hmac, Mac};
type HmacSha256 = Hmac<sha2::Sha256>;

fn compute_tripcode(password: &str) -> String {
    let mut mac = HmacSha256::new_from_slice(
        crate::config::CONFIG.cookie_secret.as_bytes()
    ).expect("HMAC accepts any key length");
    mac.update(password.as_bytes());
    let result = mac.finalize().into_bytes();
    // encode result as before
}
```

---

### [Low] #29 — `ffmpeg` Encoder Detection Makes 3 Separate Subprocess Calls

**File:** `src/media/ffmpeg.rs`

**Problem:**
`check_webp_encoder()`, `check_vp9_encoder()`, and `check_opus_encoder()` each spawn a separate `ffmpeg -encoders` process. Each call can take 100–300ms, adding up to ~900ms of startup latency for work that could be done in one pass.

**Fix:**
```rust
pub fn detect_encoders() -> (bool, bool, bool) {
    let output = Command::new("ffmpeg").args(["-encoders"])
        .stdout(Stdio::piped()).stderr(Stdio::null()).output();
    match output {
        Ok(o) => {
            let stdout = String::from_utf8_lossy(&o.stdout);
            (
                stdout.lines().any(|l| l.contains("libwebp")),
                stdout.lines().any(|l| l.contains("libvpx-vp9")),
                stdout.lines().any(|l| l.contains("libopus")),
            )
        }
        Err(_) => (false, false, false),
    }
}
```

---

### [Low] #30 — `encode_q` Duplicated Twice in `backup.rs`

**File:** `src/handlers/admin/backup.rs:1522,2260`

**Problem:**
A custom percent-encoder function `encode_q` appears verbatim at two locations in the same file. There are no tests for it and it encodes space as `+` (form-encoding) rather than `%20` (URL path encoding), which is a subtle difference future callers may not expect.

**Fix:**
Extract to a shared utility, or replace with `urlencoding::encode`:
```rust
use urlencoding::encode;
let msg = encode(&app_err.to_string());
```

---

### [Low] #31 — `collect_thread_file_paths` Unbounded SQLite Variable Count

**File:** `src/db/threads.rs:~75`

**Problem:**
The function builds a dynamic `WHERE thread_id IN (?, ?, ...)` clause with no upper bound on the number of parameters. SQLite's default `SQLITE_MAX_VARIABLE_NUMBER` is 999. A prune operation deleting 1000+ threads at once will fail with `SQLITE_RANGE`.

**Fix:**
Process in chunks:
```rust
const MAX_PARAMS: usize = 900;
let mut all_paths = Vec::new();
for chunk in thread_ids.chunks(MAX_PARAMS) {
    all_paths.extend(collect_chunk_file_paths(conn, chunk)?);
}
```

---

### [Low] #32 — `prune_login_fails` Uses `Ordering::Relaxed` Store

**File:** `src/handlers/admin/auth.rs:~83`

**Problem:**
```rust
LOGIN_CLEANUP_SECS.store(now, Ordering::Relaxed);
```

Unlike the rate-limit cleanup in middleware which uses `compare_exchange(AcqRel)`, the login-fail prune uses `Relaxed` for the throttle store. Under concurrent login attempts, multiple threads may all observe the window has expired and all run the prune simultaneously — wasting work and causing lock contention on the DashMap.

**Fix:**
```rust
if LOGIN_CLEANUP_SECS
    .compare_exchange(last, now, Ordering::AcqRel, Ordering::Relaxed)
    .is_ok()
{
    ADMIN_LOGIN_FAILS.retain(|_, (_, window_start)|
        now.saturating_sub(*window_start) <= LOGIN_FAIL_WINDOW);
}
```

---

### [Low] #33 — Log File Never Rotated; Will Grow Without Bound

**File:** `src/logging.rs:~35`

**Problem:**
```rust
let file_appender = tracing_appender::rolling::never(log_dir, "rustchan.log");
```

The log file is opened in append mode and never rotated. On a busy instance it will grow indefinitely. If the filesystem fills, the server will crash or corrupt the SQLite WAL.

**Fix:**
Use daily rotation:
```rust
let file_appender = tracing_appender::rolling::daily(log_dir, "rustchan.log");
```
Or document and provide a `logrotate` configuration for production deployments.

---

### [Low] #34 — `ACTIVE_IPS` Prune Clears Entire Map on First Run After Start

**File:** `src/server/server.rs:~305`

**Problem:**
```rust
let cutoff = Instant::now()
    .checked_sub(Duration::from_secs(300))
    .unwrap_or_else(Instant::now);  // ← fires in first 5 min of uptime
```

Within the first 300 seconds of process lifetime, `checked_sub` returns `None` and the fallback sets `cutoff = Instant::now()`. The retain closure `*last_seen > cutoff` then evaluates to false for all entries (no entry is in the future), silently clearing the entire `ACTIVE_IPS` map. The "users online" counter resets to zero on the first prune.

**Fix:**
```rust
let now = Instant::now();
if let Some(cutoff) = now.checked_sub(Duration::from_secs(300)) {
    ACTIVE_IPS.retain(|_, last_seen| *last_seen > cutoff);
}
// else: skip prune — process is too young to have stale entries
```

---

## Architectural Notes

### Areas Requiring Tests

The following areas have no unit or integration tests and are high-priority candidates:

- `insert_reply_into_thread` — the primary RustWave gateway write path
- `edit_post` with `edit_window_secs = 0` — the semantic mismatch bug (#25)
- `cast_vote` race condition — concurrent votes on an expiring poll (#16)
- `serve_board_media` with SVG content — the inline-XSS path (#1)
- `prune_old_threads` / `archive_old_threads` with sticky threads
- `collect_thread_file_paths` with > 999 thread IDs — the SQLite variable limit (#31)
- `transcode_video_inner` / `transcode_audio_inner` — with a known-good 1-second MP4
- `admin_ban_and_delete` when the thread was concurrently deleted (#15)

### Design Concerns

**SQLite write-lock contention under load:** All write operations share a single write lock. Under a post flood, `spawn_blocking` tasks queuing for DB connections can exhaust the blocking thread pool (CPUs×4). There is no circuit breaker — the server will queue all requests into `spawn_blocking` and appear to hang. Add a connection-wait timeout and expose the pending-job count via metrics.

**ChanNet is effectively unauthenticated in its current state:** Findings #2 (no Ed25519 verification) and #9 (no API key on endpoints) combine to make the entire federation layer a zero-auth data injection surface. The `--chan-net` flag should be treated as experimental and the loopback-only bind default should be enforced at startup, not just in the config default.

**`JoinError` wrapping loses context:** The pattern `AppError::Internal(anyhow::anyhow!(e))` for `tokio::task::JoinError` appears throughout the codebase. Since `JoinError` does not implement `std::error::Error`, the resulting message is `"JoinError(...)"`. Use `anyhow::anyhow!("Worker panicked: {e}")` for more useful diagnostics.

**`encode_q` duplication:** The custom percent-encoder at lines 1522 and 2260 in `backup.rs` is functionally identical. It should be extracted to a shared utility, or replaced with the `urlencoding` crate.
