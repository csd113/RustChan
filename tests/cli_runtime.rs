#![expect(
    unused_crate_dependencies,
    reason = "Cargo exposes all package dependencies to this black-box integration target, which intentionally interacts only with the compiled CLI"
)]

//! End-to-end tests for command-line startup and runtime isolation.

#[cfg(all(feature = "tls-self-signed", unix))]
use anyhow::bail;
use anyhow::{Context, Result};
#[cfg(all(feature = "tls-self-signed", unix))]
use std::net::TcpListener;
use std::path::{Path, PathBuf};
#[cfg(all(feature = "tls-self-signed", unix))]
use std::process::{Child, ExitStatus};
use std::process::{Command, Output};

/// Builds a CLI command rooted in the supplied working directory.
fn cli_command(binary: &Path, args: &[&str], current_dir: &Path) -> Command {
    let mut command = Command::new(binary);
    command.args(args).current_dir(current_dir);
    command
}

/// Runs one non-server CLI command with Tor support disabled.
fn run_cli(binary: &Path, args: &[&str], current_dir: &Path) -> Result<Output> {
    cli_command(binary, args, current_dir)
        .env("CHAN_TOR_SUPPORT", "0")
        .output()
        .context("run rustchan-cli")
}

/// Copies the compiled CLI into an otherwise empty `bin` directory.
fn copy_cli_to_empty_bin(root: &Path) -> Result<PathBuf> {
    let source = Path::new(env!("CARGO_BIN_EXE_rustchan-cli"));
    let bin_dir = root.join("bin");
    std::fs::create_dir(&bin_dir).context("create empty bin directory")?;
    let file_name = source
        .file_name()
        .context("compiled CLI path has no filename")?;
    let destination = bin_dir.join(file_name);
    std::fs::copy(source, &destination).context("copy rustchan-cli")?;
    Ok(destination)
}

#[cfg(all(feature = "tls-self-signed", unix))]
/// Owns the TLS server subprocess and guarantees that it is reaped.
#[derive(Debug)]
struct TlsProcessGuard {
    /// Live child process, cleared after a successful wait.
    child: Option<Child>,
}

#[cfg(all(feature = "tls-self-signed", unix))]
impl TlsProcessGuard {
    /// Wraps a newly spawned TLS server process.
    const fn new(child: Child) -> Self {
        Self { child: Some(child) }
    }

    /// Returns the live child or an error after it has already been reaped.
    fn child_mut(&mut self) -> Result<&mut Child> {
        self.child
            .as_mut()
            .context("TLS child has already been reaped")
    }

    /// Requests normal Unix termination and waits for the server to exit.
    async fn terminate_gracefully(&mut self, log_path: &Path) -> Result<ExitStatus> {
        let pid = self.child_mut()?.id().to_string();
        let signal_status = Command::new("kill")
            .args(["-TERM", &pid])
            .status()
            .context("run the Unix kill utility")?;
        if !signal_status.success() {
            bail!("failed to send SIGTERM to TLS child {pid}: {signal_status}");
        }

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            if let Some(status) = self.child_mut()?.try_wait().context("wait for TLS child")? {
                self.child.take();
                return Ok(status);
            }
            if std::time::Instant::now() >= deadline {
                bail!(
                    "TLS child did not shut down gracefully:\n{}",
                    read_log(log_path)
                );
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }
}

#[cfg(all(feature = "tls-self-signed", unix))]
impl Drop for TlsProcessGuard {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            if child.try_wait().ok().flatten().is_none() {
                drop(child.kill());
            }
            drop(child.wait());
        }
    }
}

#[cfg(all(feature = "tls-self-signed", unix))]
/// Reads the captured server log, retaining a useful fallback on failure.
fn read_log(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|error| format!("could not read log: {error}"))
}

#[cfg(all(feature = "tls-self-signed", unix))]
/// Waits until the HTTPS admin endpoint becomes ready.
async fn wait_for_https_admin(
    client: &reqwest::Client,
    url: &str,
    process: &mut TlsProcessGuard,
    log_path: &Path,
) -> Result<reqwest::Response> {
    for _ in 0..100 {
        if let Some(status) = process
            .child_mut()?
            .try_wait()
            .context("query TLS child status")?
        {
            bail!(
                "TLS child exited before readiness ({status}):\n{}",
                read_log(log_path)
            );
        }
        if let Ok(response) = client.get(url).send().await {
            if response.status() == reqwest::StatusCode::OK {
                return Ok(response);
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    bail!("TLS server did not become ready:\n{}", read_log(log_path));
}

#[cfg(all(feature = "tls-self-signed", unix))]
/// Extracts the first cookie header whose value starts with `name`.
fn set_cookie_header(headers: &reqwest::header::HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(reqwest::header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find(|value| value.starts_with(name))
        .map(str::to_owned)
}

#[cfg(all(feature = "tls-self-signed", unix))]
/// Extracts the hidden CSRF form field without unchecked string slicing.
fn hidden_csrf_value(html: &str) -> Option<&str> {
    let field = html.get(html.find(r#"name="_csrf""#)?..)?;
    let value = field.get(field.find(r#"value=""#)? + r#"value=""#.len()..)?;
    value.get(..value.find('"')?)
}

#[cfg(all(feature = "tls-self-signed", unix))]
/// Percent-encodes one `application/x-www-form-urlencoded` component.
fn form_component(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push('%');
            encoded.push(
                char::from_digit(u32::from(byte >> 4), 16)
                    .unwrap_or('0')
                    .to_ascii_uppercase(),
            );
            encoded.push(
                char::from_digit(u32::from(byte & 0x0f), 16)
                    .unwrap_or('0')
                    .to_ascii_uppercase(),
            );
        }
    }
    encoded
}

#[cfg(all(feature = "tls-self-signed", unix))]
/// Reserves an ephemeral loopback port until the returned listener is dropped.
fn reserve_loopback_port() -> Result<(TcpListener, u16)> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).context("reserve loopback port")?;
    let port = listener
        .local_addr()
        .context("read reserved loopback address")?
        .port();
    Ok((listener, port))
}

#[cfg(all(feature = "tls-self-signed", unix))]
/// Writes the direct-HTTPS topology used by the subprocess test.
fn write_tls_settings(
    data_dir: &Path,
    main_port: u16,
    https_port: u16,
    redirect_port: u16,
) -> Result<()> {
    let settings = format!(
        r#"cookie_secret = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
port = {main_port}
enable_tor_support = false
public_hosts = ["localhost", "127.0.0.1", "::1"]

[tls]
enabled = true
require_https = true
port = {https_port}
redirect_http = true
http_port = {redirect_port}
"#
    );
    std::fs::write(data_dir.join("settings.toml"), settings).context("write TLS settings")
}

#[cfg(all(feature = "tls-self-signed", unix))]
/// Starts the CLI server with deterministic external integrations disabled.
fn spawn_tls_process(
    binary: &Path,
    data_dir_text: &str,
    root: &Path,
    main_port: u16,
    log_path: &Path,
) -> Result<TlsProcessGuard> {
    let stdout = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .context("open TLS server log")?;
    let stderr = stdout.try_clone().context("clone TLS server log")?;
    let child = cli_command(binary, &["--data-dir", data_dir_text, "serve"], root)
        .env("RUSTCHAN_SPAWNED", "1")
        .env("CHAN_HOST", "127.0.0.1")
        .env("CHAN_PORT", main_port.to_string())
        .env("CHAN_BIND", format!("127.0.0.1:{main_port}"))
        .env("CHAN_TOR_SUPPORT", "0")
        .env("CHAN_TOR_ONLY", "0")
        .env("CHAN_REQUIRE_FFMPEG", "0")
        .env("CHAN_FFMPEG_PATH", "__rustchan_tls_test_no_ffmpeg__")
        .env("CHAN_FFPROBE_PATH", "__rustchan_tls_test_no_ffprobe__")
        // Direct HTTPS must mark cookies Secure even when the proxy/tunnel
        // compatibility toggle is disabled.
        .env("CHAN_HTTPS_COOKIES", "0")
        .env("CHAN_PUBLIC_HOSTS", "localhost,127.0.0.1,::1")
        .env("CHAN_AUTO_FULL_BACKUP_HOURS", "0")
        .env("CHAN_AUTO_VACUUM_HOURS", "0")
        .env("CHAN_WAL_CHECKPOINT_SECS", "0")
        .env("CHAN_POLL_CLEANUP_HOURS", "0")
        .env("CHAN_WAVEFORM_CACHE_MAX_MB", "0")
        .stdout(std::process::Stdio::from(stdout))
        .stderr(std::process::Stdio::from(stderr))
        .spawn()
        .context("start TLS server")?;
    Ok(TlsProcessGuard::new(child))
}

#[test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "assertions intentionally report violated CLI filesystem invariants"
)]
fn help_and_version_do_not_create_runtime_state() -> Result<()> {
    let root = tempfile::tempdir().context("create temporary root")?;
    let copied_cli = copy_cli_to_empty_bin(root.path())?;
    let adjacent_data_dir = copied_cli
        .parent()
        .context("copied CLI path has no parent")?
        .join("rustchan-data");

    let help = cli_command(&copied_cli, &["--help"], root.path())
        .output()
        .context("run rustchan-cli --help")?;
    assert!(help.status.success(), "help failed: {help:?}");
    assert!(
        String::from_utf8_lossy(&help.stdout).contains("Usage:"),
        "help output should contain usage"
    );
    assert!(
        !adjacent_data_dir.exists(),
        "--help must not create adjacent runtime state"
    );

    let version = cli_command(&copied_cli, &["--version"], root.path())
        .output()
        .context("run rustchan-cli --version")?;
    assert!(version.status.success(), "version failed: {version:?}");
    assert!(
        String::from_utf8_lossy(&version.stdout).contains(env!("CARGO_PKG_VERSION")),
        "version output should contain the package version"
    );
    assert!(
        !adjacent_data_dir.exists(),
        "--version must not create adjacent runtime state"
    );
    Ok(())
}

#[test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "assertions intentionally report violated CLI filesystem invariants"
)]
fn explicit_data_dir_owns_all_initialized_runtime_state() -> Result<()> {
    let root = tempfile::tempdir().context("create temporary root")?;
    let copied_cli = copy_cli_to_empty_bin(root.path())?;
    let default_data_dir = copied_cli
        .parent()
        .context("copied CLI path has no parent")?
        .join("rustchan-data");
    let intended_data_dir = root.path().join("service-data");
    let data_dir = intended_data_dir.to_string_lossy();

    let output = run_cli(
        &copied_cli,
        &["--data-dir", &data_dir, "admin", "list-boards"],
        root.path(),
    )?;
    assert!(
        output.status.success(),
        "admin command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        intended_data_dir.join("settings.toml").is_file(),
        "explicit data directory must contain settings.toml"
    );
    assert!(
        intended_data_dir.join("chan.db").is_file(),
        "explicit data directory must contain chan.db"
    );
    assert!(
        intended_data_dir.join("logs").is_dir(),
        "explicit data directory must contain logs"
    );
    assert!(
        intended_data_dir.join("backups").is_dir(),
        "explicit data directory must contain backups"
    );
    assert!(
        intended_data_dir.join("runtime").is_dir(),
        "explicit data directory must contain runtime state"
    );
    assert!(
        !default_data_dir.exists(),
        "explicit data directory must prevent default data creation"
    );
    Ok(())
}

#[test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "assertions intentionally report violated CLI filesystem invariants"
)]
fn relative_data_dir_is_rejected_before_filesystem_mutation() -> Result<()> {
    let root = tempfile::tempdir().context("create temporary root")?;
    let copied_cli = copy_cli_to_empty_bin(root.path())?;
    let default_data_dir = copied_cli
        .parent()
        .context("copied CLI path has no parent")?
        .join("rustchan-data");
    let relative_data_dir = root.path().join("relative-data");

    let output = run_cli(
        &copied_cli,
        &["--data-dir", "relative-data", "admin", "list-boards"],
        root.path(),
    )?;
    assert!(
        !output.status.success(),
        "relative data directory must be rejected"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("must be an absolute path"),
        "unexpected error: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !relative_data_dir.exists(),
        "rejected relative path must not be created"
    );
    assert!(
        !default_data_dir.exists(),
        "rejected relative path must not create default state"
    );
    Ok(())
}

#[cfg(all(feature = "tls-self-signed", unix))]
#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one subprocess lifecycle verifies the complete direct-HTTPS topology and cookie contract"
)]
#[expect(
    clippy::panic_in_result_fn,
    reason = "assertions intentionally report violated TLS topology and cookie invariants"
)]
async fn direct_https_process_exposes_only_https_and_redirect_with_secure_login_cookies(
) -> Result<()> {
    drop(rustls::crypto::ring::default_provider().install_default());
    let root = tempfile::tempdir().context("create temporary root")?;
    let copied_cli = copy_cli_to_empty_bin(root.path())?;
    let data_dir = root.path().join("tls-data");
    std::fs::create_dir(&data_dir).context("create TLS data directory")?;

    // Keep the main plaintext port reserved for the entire child lifetime. If
    // direct-HTTPS mode regresses and tries to bind it, startup fails instead
    // of allowing an unrelated process to create a false test result.
    let (main_reservation, main_port) = reserve_loopback_port()?;
    let (initial_https_reservation, initial_https_port) = reserve_loopback_port()?;
    let (initial_redirect_reservation, initial_redirect_port) = reserve_loopback_port()?;
    write_tls_settings(
        &data_dir,
        main_port,
        initial_https_port,
        initial_redirect_port,
    )?;
    let data_dir_text = data_dir
        .to_str()
        .context("temporary data directory is not UTF-8")?;
    let create_admin = run_cli(
        &copied_cli,
        &[
            "--data-dir",
            data_dir_text,
            "admin",
            "create-admin",
            "admin",
            "AdminPass123!",
        ],
        root.path(),
    )?;
    assert!(
        create_admin.status.success(),
        "create admin failed: {}",
        String::from_utf8_lossy(&create_admin.stderr)
    );

    let client = reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .timeout(std::time::Duration::from_secs(3))
        .build()
        .context("build TLS test client")?;

    drop((initial_https_reservation, initial_redirect_reservation));
    let mut launched = None;
    for attempt in 1..=3 {
        let (https_reservation, https_port) = reserve_loopback_port()?;
        let (redirect_reservation, redirect_port) = reserve_loopback_port()?;
        write_tls_settings(&data_dir, main_port, https_port, redirect_port)?;
        drop((https_reservation, redirect_reservation));

        let log_path = root.path().join(format!("tls-server-{attempt}.log"));
        let mut candidate = spawn_tls_process(
            &copied_cli,
            data_dir_text,
            root.path(),
            main_port,
            &log_path,
        )?;
        let https_base = format!("https://127.0.0.1:{https_port}");
        let readiness = wait_for_https_admin(
            &client,
            &format!("{https_base}/admin"),
            &mut candidate,
            &log_path,
        )
        .await;
        match readiness {
            Ok(login_page) => {
                launched = Some((candidate, redirect_port, https_base, log_path, login_page));
                break;
            }
            Err(error) => {
                let address_in_use = error.to_string().contains("Address already in use");
                if attempt < 3 && address_in_use {
                    // A bind can be stolen only in the short handoff between
                    // releasing the reservations and the child binding them.
                    // Retry with fresh kernel-assigned ports instead of flaking.
                    drop(candidate);
                    continue;
                }
                return Err(error.context(format!("TLS server launch failed on attempt {attempt}")));
            }
        }
    }
    let (mut process, redirect_port, https_base, log_path, login_page) =
        launched.context("TLS server launch loop completed without a result")?;
    let csrf_set_cookie = set_cookie_header(login_page.headers(), "csrf_token=")
        .context("HTTPS login page did not set a CSRF cookie")?;
    assert!(
        csrf_set_cookie.to_ascii_lowercase().contains("; secure"),
        "direct HTTPS CSRF cookie must be Secure: {csrf_set_cookie}"
    );
    let csrf_cookie = csrf_set_cookie
        .split(';')
        .next()
        .context("CSRF cookie header had no cookie pair")?
        .to_owned();
    let login_html = login_page.text().await.context("read login page")?;
    let csrf_form = hidden_csrf_value(&login_html).context("login page has no CSRF field")?;
    let form = format!(
        "username={}&password={}&_csrf={}",
        form_component("admin"),
        form_component("AdminPass123!"),
        form_component(csrf_form)
    );
    let login = client
        .post(format!("{https_base}/admin/login"))
        .header(
            reqwest::header::CONTENT_TYPE,
            "application/x-www-form-urlencoded",
        )
        .header(reqwest::header::COOKIE, csrf_cookie)
        .header(reqwest::header::ORIGIN, &https_base)
        .header(reqwest::header::REFERER, format!("{https_base}/admin"))
        .body(form)
        .send()
        .await
        .context("HTTPS admin login")?;
    assert_eq!(
        login.status(),
        reqwest::StatusCode::SEE_OTHER,
        "valid HTTPS login must redirect"
    );
    let session_cookie = set_cookie_header(login.headers(), "chan_admin_session=")
        .context("successful login did not set a session cookie")?;
    assert!(
        session_cookie.to_ascii_lowercase().contains("; secure"),
        "direct HTTPS session cookie must be Secure: {session_cookie}"
    );

    let redirect = client
        .get(format!("http://127.0.0.1:{redirect_port}/admin"))
        .send()
        .await
        .context("HTTP redirect request")?;
    assert_eq!(
        redirect.status(),
        reqwest::StatusCode::PERMANENT_REDIRECT,
        "plaintext admin request must redirect permanently"
    );
    assert_eq!(
        redirect
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|value| value.to_str().ok()),
        Some(format!("{https_base}/admin").as_str()),
        "plaintext redirect must target the HTTPS admin page"
    );
    assert!(
        !redirect.headers().contains_key(reqwest::header::SET_COOKIE),
        "plaintext redirect listener must not issue authentication cookies"
    );

    let plaintext_login = client
        .post(format!("http://127.0.0.1:{redirect_port}/admin/login"))
        .header(
            reqwest::header::CONTENT_TYPE,
            "application/x-www-form-urlencoded",
        )
        .body("username=admin&password=AdminPass123%21")
        .send()
        .await
        .context("plaintext login request")?;
    assert_eq!(
        plaintext_login.status(),
        reqwest::StatusCode::PERMANENT_REDIRECT,
        "plaintext login must redirect permanently"
    );
    assert!(
        !plaintext_login
            .headers()
            .contains_key(reqwest::header::SET_COOKIE),
        "plaintext redirect listener must not authenticate"
    );

    assert_eq!(
        main_reservation
            .local_addr()
            .context("read retained main-port reservation")?
            .port(),
        main_port,
        "the test must retain exclusive ownership of the disabled plaintext port"
    );
    assert!(
        data_dir.join("runtime/tls/dev/self-signed.crt").is_file(),
        "self-signed TLS startup must persist its certificate"
    );
    assert!(
        data_dir.join("runtime/tls/dev/self-signed.key").is_file(),
        "self-signed TLS startup must persist its private key"
    );

    let status = process.terminate_gracefully(&log_path).await?;
    assert!(
        status.success(),
        "TLS server did not exit cleanly ({status}):\n{}",
        read_log(&log_path)
    );
    Ok(())
}
