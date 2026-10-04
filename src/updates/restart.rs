//! Configuration-only transactions inside the existing updater engine.

use super::{
    transaction::native::{Engine, Service},
    Operation, Phase, Status,
};
use crate::restart::{configuration_digest, Store};
use anyhow::Context as _;
use std::fs::File;
use uuid::Uuid;

impl Engine {
    /// The updater exclusively owns rollback payloads; the web identity cannot forge them.
    pub(super) fn restart_store(&self) -> Store {
        Store {
            directory: self.config.state_dir.join("settings-restart"),
            settings: self.config.settings_path.clone(),
        }
    }
    /// Record actual startup without replacing rollback bytes during an active transaction.
    pub(super) fn started(&self, instance: Uuid, configuration: &str) -> anyhow::Result<Status> {
        anyhow::ensure!(
            configuration.len() == 64 && configuration.bytes().all(|c| c.is_ascii_hexdigit()),
            "invalid configuration binding"
        );
        let status = self.status()?;
        let store = self.restart_store();
        if status.phase.active() {
            anyhow::ensure!(
                status.restart_instance != Some(instance),
                "old process cannot verify replacement initialization"
            );
            store.observe(instance, configuration, false)?;
        } else {
            let _update = self.lock()?;
            let _settings = crate::config::admin::settings_lease(&self.config.settings_path)?;
            store.observe(instance, configuration, true)?;
        }
        Ok(status)
    }
    /// Accept a single fixed restart only after config/service preflight and durable journal intent.
    /// Both leases transfer to the worker and remain held through health or rollback.
    pub(super) fn approve_restart(
        &self,
        instance: Uuid,
        administrator: i64,
        service: &impl Service,
    ) -> anyhow::Result<(Status, File, File)> {
        let update = self.lock()?;
        let settings = crate::config::admin::settings_lease(&self.config.settings_path)?;
        let mut status = self.status()?;
        anyhow::ensure!(
            !status.phase.active() && status.phase != Phase::FailedManualIntervention,
            "another transaction or recovery is in progress"
        );
        anyhow::ensure!(
            status.restart_instance != Some(instance),
            "restart for this process was already consumed"
        );
        let store = self.restart_store();
        let running = store.running()?;
        anyhow::ensure!(
            running.instance == instance && administrator > 0,
            "stale process or invalid administrator"
        );
        let candidate = store.candidate()?;
        anyhow::ensure!(
            configuration_digest(&candidate) != running.digest,
            "configuration has not changed"
        );
        let good = Store::read(&store.directory.join("known-good.toml"), 4 * 1024 * 1024)?;
        anyhow::ensure!(
            crate::config::admin::restart_files_differ(
                std::str::from_utf8(&good)?,
                std::str::from_utf8(&candidate)?
            )?,
            "no startup settings changed"
        );
        crate::config::validate_update_settings(
            std::str::from_utf8(&good)?,
            &self.config.data_dir,
        )?;
        service.preflight()?;
        store.write(
            "candidate.sha256",
            configuration_digest(&candidate).as_bytes(),
        )?;
        status.operation = Operation::SettingsRestart;
        status.restart_instance = Some(instance);
        status.approval = None;
        status.job = Some(Uuid::new_v4().to_string());
        status.administrator = Some(administrator);
        status.installed = self.current_version()?;
        status.previous_version = Some(status.installed.clone());
        status.target_version.clone_from(&status.previous_version);
        self.save(
            &mut status,
            Phase::Stopping,
            "Settings restart accepted. RustChan is draining requests.",
        )?;
        Ok((status, update, settings))
    }
    /// Stop through systemd SIGTERM, start the fixed service, then verify exact loaded config.
    pub(super) fn restart_settings(
        &self,
        status: &mut Status,
        service: &impl Service,
    ) -> anyhow::Result<()> {
        if let Err(error) = service.stop() {
            // An unconfirmed stop must never permit configuration restoration under a live process.
            tracing::error!(%error, "settings shutdown did not confirm service stop");
            self.save(status, Phase::FailedManualIntervention, "RustChan did not stop cleanly. Configuration was not replaced; operator recovery is required.")?;
            return Ok(());
        }
        let mut committed = false;
        let trial = (|| {
            self.save(
                status,
                Phase::Restarting,
                "Starting RustChan with saved settings.",
            )?;
            service.start()?;
            self.save(
                status,
                Phase::HealthChecking,
                "Verifying replacement process, database and listener readiness.",
            )?;
            let previous = status
                .restart_instance
                .context("missing previous process identity")?;
            service.health_instance(&status.installed, previous)?;
            let store = self.restart_store();
            let running = store.running()?;
            let expected = Store::read(&store.directory.join("candidate.sha256"), 64)?;
            anyhow::ensure!(
                running.instance != previous && running.digest.as_bytes() == expected,
                "replacement did not initialize the requested settings"
            );
            service.prepare_commit(&status.installed)?;
            self.save(
                status,
                Phase::Succeeded,
                "RustChan restarted; saved settings passed readiness verification.",
            )?;
            committed = true;
            store.observe(running.instance, &running.digest, true)?;
            service.commit()
        })();
        if let Err(error) = trial {
            if committed {
                // Terminal commit selects the healthy replacement. Losing a
                // final acknowledgment cannot authorize restoration beneath it.
                return Err(error);
            }
            tracing::error!(%error, "settings restart failed; recovering known-good configuration");
            self.recover_settings_restart(status, service)?;
        }
        Ok(())
    }
    /// Configuration rollback reuses updater recovery admission without restoring any database.
    pub(super) fn recover_settings_restart(
        &self,
        status: &mut Status,
        service: &impl Service,
    ) -> anyhow::Result<()> {
        let recovery = (|| {
            self.save(
                status,
                Phase::RollingBack,
                "Settings startup failed or was interrupted; restoring previous configuration.",
            )?;
            service.stop()?;
            self.restart_store().restore()?;
            self.save(
                status,
                Phase::RestartingPrevious,
                "Checking RustChan with the last healthy configuration.",
            )?;
            service.start()?;
            service.health_instance(
                &status.installed,
                status
                    .restart_instance
                    .context("missing restart identity")?,
            )?;
            let store = self.restart_store();
            let running = store.running()?;
            let expected = configuration_digest(&Store::read(
                &store.directory.join("known-good.toml"),
                4 * 1024 * 1024,
            )?);
            anyhow::ensure!(
                running.digest == expected,
                "recovery configuration did not initialize"
            );
            service.prepare_commit(&status.installed)?;
            self.save(
                status,
                Phase::RolledBack,
                "Settings startup failed. Previous configuration restored and readiness verified.",
            )
        })();
        if let Err(error) = recovery {
            tracing::error!(%error, "settings recovery failed");
            self.save(status, Phase::FailedManualIntervention, "Settings restart and recovery failed. Last healthy configuration is retained; operator recovery is required.")?;
            return Ok(());
        }
        service.commit()
    }
}
