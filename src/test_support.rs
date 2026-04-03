#[cfg(test)]
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

#[cfg(test)]
pub fn app_state() -> crate::middleware::AppState {
    let pool = crate::db::init_test_pool().expect("test pool");
    let job_queue = std::sync::Arc::new(crate::workers::JobQueue::new(pool.clone()));
    crate::middleware::AppState {
        db: pool,
        ffmpeg_available: false,
        ffmpeg_webp_available: false,
        job_queue,
        backup_progress: std::sync::Arc::new(crate::middleware::BackupProgress::new()),
        chan_ledger: None,
        onion_address: std::sync::Arc::new(tokio::sync::RwLock::new(None)),
        ngrok: crate::server::ngrok::NgrokController::new(),
    }
}

#[cfg(test)]
pub fn connect_info() -> axum::extract::ConnectInfo<SocketAddr> {
    axum::extract::ConnectInfo(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 41000))
}

#[cfg(test)]
pub fn multipart_body(
    fields: &[(&str, &str)],
    file: Option<(&str, &str, &[u8], &str)>,
) -> (String, Vec<u8>) {
    let boundary = "rustchan-test-boundary".to_string();
    let mut body = Vec::new();

    for (name, value) in fields {
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        body.extend_from_slice(
            format!("Content-Disposition: form-data; name=\"{name}\"\r\n\r\n").as_bytes(),
        );
        body.extend_from_slice(value.as_bytes());
        body.extend_from_slice(b"\r\n");
    }

    if let Some((field_name, filename, contents, content_type)) = file {
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        body.extend_from_slice(
            format!(
                "Content-Disposition: form-data; name=\"{field_name}\"; filename=\"{filename}\"\r\n"
            )
            .as_bytes(),
        );
        body.extend_from_slice(format!("Content-Type: {content_type}\r\n\r\n").as_bytes());
        body.extend_from_slice(contents);
        body.extend_from_slice(b"\r\n");
    }

    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    (boundary, body)
}
