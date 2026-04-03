use serde_json::Value;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command as TokioCommand;
use tokio::sync::{mpsc, oneshot, Mutex, RwLock};
use tokio::task::JoinHandle;
use tokio::time::{timeout, Duration};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum NgrokState {
    #[default]
    Disabled,
    Starting,
    Ready {
        url: String,
    },
    NotInstalled,
    NotConfigured,
    Error {
        message: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToggleOutcome {
    Enabled,
    Disabled,
    NeedsSetupPrompt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StartupSignal {
    Started,
    NeedsSetup,
}

#[derive(Default)]
struct ManagedProcess {
    task: Option<JoinHandle<()>>,
    stop: Option<CancellationToken>,
    generation: u64,
}

#[derive(Clone)]
pub struct NgrokController {
    state: Arc<RwLock<NgrokState>>,
    process: Arc<Mutex<ManagedProcess>>,
    generation: Arc<AtomicU64>,
}

impl Default for NgrokController {
    fn default() -> Self {
        Self::new()
    }
}

impl NgrokController {
    pub fn new() -> Self {
        Self {
            state: Arc::new(RwLock::new(NgrokState::Disabled)),
            process: Arc::new(Mutex::new(ManagedProcess::default())),
            generation: Arc::new(AtomicU64::new(0)),
        }
    }

    pub async fn snapshot(&self) -> NgrokState {
        self.state.read().await.clone()
    }

    pub async fn shutdown(&self) {
        self.stop_active_process(true).await;
    }

    pub async fn toggle(&self, port: u16) -> ToggleOutcome {
        self.reap_finished_process().await;

        if self.has_active_process().await {
            self.stop_active_process(true).await;
            return ToggleOutcome::Disabled;
        }

        if !is_ngrok_installed() {
            self.set_state_unconditionally(NgrokState::NotInstalled)
                .await;
            return ToggleOutcome::NeedsSetupPrompt;
        }

        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        self.set_state_if_current(generation, NgrokState::Starting)
            .await;

        let stop = CancellationToken::new();
        let (startup_tx, startup_rx) = oneshot::channel();
        let task = tokio::spawn(supervise_ngrok(
            port,
            generation,
            self.state.clone(),
            self.process.clone(),
            self.generation.clone(),
            stop.clone(),
            startup_tx,
        ));

        {
            let mut managed = self.process.lock().await;
            managed.task = Some(task);
            managed.stop = Some(stop);
            managed.generation = generation;
        }

        match timeout(Duration::from_secs(3), startup_rx).await {
            Ok(Ok(StartupSignal::NeedsSetup)) => ToggleOutcome::NeedsSetupPrompt,
            Ok(Ok(StartupSignal::Started)) | Ok(Err(_)) | Err(_) => ToggleOutcome::Enabled,
        }
    }

    async fn has_active_process(&self) -> bool {
        let managed = self.process.lock().await;
        managed.task.is_some()
    }

    async fn reap_finished_process(&self) {
        let finished_task = {
            let mut managed = self.process.lock().await;
            if managed.task.as_ref().is_some_and(JoinHandle::is_finished) {
                managed.stop = None;
                managed.generation = 0;
                managed.task.take()
            } else {
                None
            }
        };

        if let Some(task) = finished_task {
            let _ = task.await;
        }
    }

    async fn stop_active_process(&self, set_disabled: bool) {
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let managed = {
            let mut managed = self.process.lock().await;
            ManagedProcess {
                task: managed.task.take(),
                stop: managed.stop.take(),
                generation: managed.generation,
            }
        };

        if let Some(stop) = managed.stop {
            stop.cancel();
        }
        if let Some(task) = managed.task {
            let _ = task.await;
        }

        let mut process = self.process.lock().await;
        if process.generation == managed.generation {
            process.generation = 0;
            process.stop = None;
            process.task = None;
        }
        drop(process);

        if set_disabled {
            self.set_state_if_current(generation, NgrokState::Disabled)
                .await;
        }
    }

    async fn set_state_unconditionally(&self, next: NgrokState) {
        *self.state.write().await = next;
    }

    async fn set_state_if_current(&self, generation: u64, next: NgrokState) {
        if self.generation.load(Ordering::SeqCst) == generation {
            *self.state.write().await = next;
        }
    }
}

fn is_ngrok_installed() -> bool {
    std::process::Command::new("ngrok")
        .arg("version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

async fn supervise_ngrok(
    port: u16,
    generation: u64,
    state: Arc<RwLock<NgrokState>>,
    process: Arc<Mutex<ManagedProcess>>,
    generation_clock: Arc<AtomicU64>,
    stop: CancellationToken,
    startup_tx: oneshot::Sender<StartupSignal>,
) {
    let target = format!("http://127.0.0.1:{port}");
    let mut command = TokioCommand::new("ngrok");
    command
        .arg("http")
        .arg(&target)
        .arg("--log")
        .arg("stdout")
        .arg("--log-format")
        .arg("json")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let mut startup_tx = Some(startup_tx);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            set_state_if_current(
                &state,
                &generation_clock,
                generation,
                NgrokState::NotInstalled,
            )
            .await;
            let _ = startup_tx
                .take()
                .map(|tx| tx.send(StartupSignal::NeedsSetup));
            cleanup_process_slot(&process, generation).await;
            return;
        }
        Err(error) => {
            set_state_if_current(
                &state,
                &generation_clock,
                generation,
                NgrokState::Error {
                    message: format!("failed to start ngrok: {error}"),
                },
            )
            .await;
            let _ = startup_tx.take().map(|tx| tx.send(StartupSignal::Started));
            cleanup_process_slot(&process, generation).await;
            return;
        }
    };

    let (line_tx, mut line_rx) = mpsc::unbounded_channel::<String>();
    if let Some(stdout) = child.stdout.take() {
        spawn_pipe_reader(stdout, line_tx.clone());
    }
    if let Some(stderr) = child.stderr.take() {
        spawn_pipe_reader(stderr, line_tx);
    }

    let mut saw_ready = false;
    let mut setup_issue = false;
    let mut last_error = None::<String>;

    loop {
        tokio::select! {
            _ = stop.cancelled() => {
                let _ = child.start_kill();
                let _ = child.wait().await;
                set_state_if_current(&state, &generation_clock, generation, NgrokState::Disabled).await;
                cleanup_process_slot(&process, generation).await;
                return;
            }
            status = child.wait() => {
                let next = match status {
                    Ok(exit) if exit.success() => NgrokState::Disabled,
                    Ok(_exit) if setup_issue => NgrokState::NotConfigured,
                    Ok(exit) => NgrokState::Error {
                        message: last_error.unwrap_or_else(|| format!("ngrok exited with status {exit}")),
                    },
                    Err(error) => NgrokState::Error {
                        message: format!("failed to wait for ngrok: {error}"),
                    },
                };
                set_state_if_current(&state, &generation_clock, generation, next).await;
                cleanup_process_slot(&process, generation).await;
                let signal = if setup_issue {
                    StartupSignal::NeedsSetup
                } else {
                    StartupSignal::Started
                };
                let _ = startup_tx.take().map(|tx| tx.send(signal));
                return;
            }
            Some(line) = line_rx.recv() => {
                if let Some(url) = extract_public_url(&line) {
                    saw_ready = true;
                    set_state_if_current(
                        &state,
                        &generation_clock,
                        generation,
                        NgrokState::Ready { url },
                    )
                    .await;
                    let _ = startup_tx.take().map(|tx| tx.send(StartupSignal::Started));
                    continue;
                }

                if looks_like_setup_issue(&line) {
                    setup_issue = true;
                    last_error = Some(clean_line(&line));
                    let _ = startup_tx.take().map(|tx| tx.send(StartupSignal::NeedsSetup));
                    let _ = child.start_kill();
                    continue;
                }

                if let Some(message) = extract_error_message(&line) {
                    last_error = Some(message);
                    if !saw_ready {
                        set_state_if_current(
                            &state,
                            &generation_clock,
                            generation,
                            NgrokState::Starting,
                        )
                        .await;
                    }
                }
            }
            else => {
                let next = if setup_issue {
                    NgrokState::NotConfigured
                } else if saw_ready {
                    NgrokState::Disabled
                } else {
                    NgrokState::Error {
                        message: last_error.unwrap_or_else(|| {
                            "ngrok ended before publishing a public URL".to_string()
                        }),
                    }
                };
                set_state_if_current(&state, &generation_clock, generation, next).await;
                cleanup_process_slot(&process, generation).await;
                let signal = if setup_issue {
                    StartupSignal::NeedsSetup
                } else {
                    StartupSignal::Started
                };
                let _ = startup_tx.take().map(|tx| tx.send(signal));
                return;
            }
        }
    }
}

async fn cleanup_process_slot(process: &Arc<Mutex<ManagedProcess>>, generation: u64) {
    let mut managed = process.lock().await;
    if managed.generation == generation {
        managed.task = None;
        managed.stop = None;
        managed.generation = 0;
    }
}

async fn set_state_if_current(
    state: &Arc<RwLock<NgrokState>>,
    generation_clock: &Arc<AtomicU64>,
    generation: u64,
    next: NgrokState,
) {
    if generation_clock.load(Ordering::SeqCst) == generation {
        *state.write().await = next;
    }
}

fn spawn_pipe_reader<T>(pipe: T, tx: mpsc::UnboundedSender<String>)
where
    T: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut lines = BufReader::new(pipe).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
}

fn extract_public_url(line: &str) -> Option<String> {
    if let Ok(json) = serde_json::from_str::<Value>(line) {
        if let Some(url) = find_ngrok_url(&json) {
            return Some(url);
        }
    }

    find_ngrok_url_in_text(line)
}

fn find_ngrok_url(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => find_ngrok_url_in_text(text),
        Value::Array(values) => values.iter().find_map(find_ngrok_url),
        Value::Object(map) => map.values().find_map(find_ngrok_url),
        _ => None,
    }
}

fn find_ngrok_url_in_text(text: &str) -> Option<String> {
    text.split(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | ',' | ';'))
        .find(|token| token.starts_with("https://") && token.contains("ngrok"))
        .map(|token| token.trim_end_matches('/').to_string())
}

fn looks_like_setup_issue(line: &str) -> bool {
    let cleaned = clean_line(line).to_ascii_lowercase();

    cleaned.contains("authtoken")
        || cleaned.contains("authentication failed")
        || cleaned.contains("failed to authenticate")
        || cleaned.contains("err_ngrok_")
        || cleaned.contains("config check failed")
        || cleaned.contains("account is required")
        || cleaned.contains("sign up")
}

fn extract_error_message(line: &str) -> Option<String> {
    if let Ok(json) = serde_json::from_str::<Value>(line) {
        for key in ["msg", "message", "err", "error", "details"] {
            if let Some(text) = json.get(key).and_then(Value::as_str) {
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.to_string());
                }
            }
        }
    }

    let trimmed = clean_line(line);
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

fn clean_line(line: &str) -> String {
    if let Ok(json) = serde_json::from_str::<Value>(line) {
        if let Some(message) = extract_error_message_from_json(&json) {
            return message;
        }
    }
    line.trim().to_string()
}

fn extract_error_message_from_json(json: &Value) -> Option<String> {
    for key in ["msg", "message", "err", "error", "details"] {
        if let Some(text) = json.get(key).and_then(Value::as_str) {
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{extract_public_url, looks_like_setup_issue};

    #[test]
    fn extracts_public_url_from_json_log_line() {
        let line = r#"{"lvl":"info","msg":"started tunnel","url":"https://abc123.ngrok-free.app"}"#;
        assert_eq!(
            extract_public_url(line).as_deref(),
            Some("https://abc123.ngrok-free.app")
        );
    }

    #[test]
    fn extracts_public_url_from_plain_text_line() {
        let line = "Forwarding https://abc123.ngrok-free.app -> http://127.0.0.1:8080";
        assert_eq!(
            extract_public_url(line).as_deref(),
            Some("https://abc123.ngrok-free.app")
        );
    }

    #[test]
    fn detects_setup_errors_from_log_message() {
        let line = r#"{"lvl":"error","msg":"authentication failed: authtoken missing"}"#;
        assert!(looks_like_setup_issue(line));
    }
}
