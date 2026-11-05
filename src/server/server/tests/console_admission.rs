//! Regression for the confirmed rejected-operation Progress lock-up.
use super::admit_console_operation;
use crate::middleware::data_access::DataAccess;
use crate::server::console::input::KeyEvent;
use crate::server::console::state::{ConsoleAction, ConsoleState, Dialog, OperationRequest};
use std::sync::Arc;
use tokio::sync::RwLock;

#[tokio::test]
async fn rejected_operation_completes_and_can_be_retried() -> anyhow::Result<()> {
    let access = DataAccess::default();
    let console = Arc::new(RwLock::new(ConsoleState {
        dialog: Some(Dialog::Progress {
            label: "Deleting thread",
        }),
        ..ConsoleState::default()
    }));
    let request = OperationRequest::DeleteThread { thread_id: 1 };
    let exclusive = access
        .exclusive()
        .await
        .ok_or_else(|| anyhow::anyhow!("exclusive admission failed"))?;
    anyhow::ensure!(
        admit_console_operation(&access, &console, &request)
            .await
            .is_none(),
        "exclusive work must reject admission"
    );
    let mut app = console.write().await;
    anyhow::ensure!(
        app.dialog.is_none() && app.notice.is_some(),
        "rejection must end Progress with feedback"
    );
    anyhow::ensure!(
        matches!(
            app.handle_key(&KeyEvent::Character('r'), 0, (80, 24)),
            ConsoleAction::Reload
        ),
        "console must accept refresh after rejection"
    );
    drop(app);
    drop(exclusive);
    anyhow::ensure!(
        admit_console_operation(&access, &console, &request)
            .await
            .is_some(),
        "retry must be admitted after release"
    );
    Ok(())
}
