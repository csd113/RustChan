//! Read-only manual-restart guidance for the administrator panel.

/// Compare saved settings with this process; no restart or updater is invoked.
pub(super) async fn snapshot() -> crate::restart::View {
    let pending =
        crate::middleware::data_access::spawn_blocking(crate::config::admin::restart_pending).await;
    crate::restart::View {
        message: match pending {
            Ok(Ok(true)) => "Settings saved. Stop and start RustChan using your usual launcher or service manager to apply them. Live settings already apply.".to_owned(),
            Ok(Ok(false)) => "Saved startup settings are active. No restart is required.".to_owned(),
            Ok(Err(_)) | Err(_) => "Could not compare saved settings with the running configuration. Check settings.toml before restarting RustChan.".to_owned(),
        },
    }
}
