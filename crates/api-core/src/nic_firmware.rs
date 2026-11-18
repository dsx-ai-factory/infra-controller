/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

//! Resolve frozen NIC lockdown policy only when delivering its committed action.
//! Credentials stay out of controller state and reports; any Core replica can
//! render the action from shared persistence without owning a Scout stream.

use std::time::Duration;

use carbide_secrets::credentials::CredentialReader;
use carbide_uuid::machine::HostMachineId;
use config_version::ConfigVersion;
use model::machine::nic_firmware::{
    NicFirmwareLockdownPolicy, NicFirmwareLockdownStatus, NicFirmwareLockdownStep,
    NicFirmwareTarget,
};
use rpc::forge_agent_control_response::mlx_device_lockdown::Request;
use rpc::forge_agent_control_response::{
    MlxDeviceLock, MlxDeviceLockStatus, MlxDeviceLockdown, MlxDeviceUnlock,
};

use crate::dpa::lockdown::build_resolved_lockdown_key;
use crate::{CarbideError, CarbideResult};

/// Build a status request or derive the original key for a committed change.
/// The caller has checked the active phase, result, and permission for the change.
/// Restoring the original policy remains allowed after lifecycle ownership changes.
/// Credential failures and lookup expiry return a retryable, nonsecret diagnostic.
pub(crate) async fn lockdown_request(
    host: HostMachineId,
    version: ConfigVersion,
    target: &NicFirmwareTarget,
    step: NicFirmwareLockdownStep,
    desired: NicFirmwareLockdownStatus,
    credentials: &dyn CredentialReader,
) -> CarbideResult<MlxDeviceLockdown> {
    let request = match step {
        NicFirmwareLockdownStep::Observe => Request::Status(MlxDeviceLockStatus {}),
        NicFirmwareLockdownStep::Change => {
            let NicFirmwareLockdownPolicy::Locked {
                ikm_version,
                mac_address,
            } = target.original_lockdown
            else {
                return Err(CarbideError::FailedPrecondition(
                    "cannot change NIC lockdown without its frozen key identity".into(),
                ));
            };
            let key = tokio::time::timeout(
                Duration::from_secs(30),
                build_resolved_lockdown_key(host, mac_address, credentials, ikm_version),
            )
            .await
            .map_err(|_| {
                tracing::warn!(
                    host_machine_id = %host,
                    ikm_version,
                    reason = "timeout",
                    "NIC lockdown credential lookup failed"
                );
                CarbideError::UnavailableError("NIC lockdown credentials are unavailable".into())
            })?
            .map_err(|_| {
                tracing::warn!(
                    host_machine_id = %host,
                    ikm_version,
                    reason = "lookup",
                    "NIC lockdown credential lookup failed"
                );
                CarbideError::UnavailableError("NIC lockdown credentials are unavailable".into())
            })?;
            match desired {
                NicFirmwareLockdownStatus::Locked => Request::Lock(MlxDeviceLock { key }),
                NicFirmwareLockdownStatus::Unlocked => Request::Unlock(MlxDeviceUnlock { key }),
            }
        }
    };
    Ok(MlxDeviceLockdown {
        controller_state_version: version.to_string(),
        expected_device: Some(target.selector.clone().into()),
        request: Some(request),
    })
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use async_trait::async_trait;
    use carbide_instrument::testing::capture_logs_async;
    use carbide_secrets::credentials::{
        CredentialKey, CredentialWriter, Credentials, NicLockdownIkm,
    };
    use carbide_secrets::{MemoryCredentialStore, SecretsError};
    use tokio::sync::Notify;

    use super::*;

    struct Lookup {
        store: MemoryCredentialStore,
        stalled: AtomicBool,
        failed: bool,
        started: Notify,
    }

    #[async_trait]
    impl CredentialReader for Lookup {
        async fn get_credentials(
            &self,
            key: &CredentialKey,
        ) -> Result<Option<Credentials>, SecretsError> {
            if self.stalled.load(Ordering::SeqCst) {
                self.started.notify_one();
                return std::future::pending().await;
            }
            if self.failed {
                return Err(SecretsError::GenericError(eyre::eyre!(
                    "backend exposed test-secret"
                )));
            }
            self.store.get_credentials(key).await
        }
    }

    // A credential outage has no bearing on whether the saved hardware request
    // is valid. Its error must stay retryable without exposing backend text.
    #[tokio::test]
    async fn credential_backend_failure_is_unavailable_without_secret_details() {
        let reader = Lookup {
            store: MemoryCredentialStore::default(),
            stalled: AtomicBool::new(false),
            failed: true,
            started: Notify::new(),
        };
        let mut target = model::test_support::nic_firmware::target();
        let host = "fm100htes3rn1npvbtm5qd57dkilaag7ljugl1llmm7rfuq1ov50i0rpl30"
            .parse()
            .unwrap();
        target.original_lockdown = NicFirmwareLockdownPolicy::Locked {
            ikm_version: 7,
            mac_address: "02:00:00:00:00:01".parse().unwrap(),
        };
        let (result, logs) = capture_logs_async(lockdown_request(
            host,
            ConfigVersion::initial(),
            &target,
            NicFirmwareLockdownStep::Change,
            NicFirmwareLockdownStatus::Unlocked,
            &reader,
        ))
        .await;
        let error = result.expect_err("credential failure must remain visible");
        assert!(matches!(error, CarbideError::UnavailableError(message)
            if message == "NIC lockdown credentials are unavailable"));
        // The warning identifies the failed lookup without copying the backend's
        // response into a second diagnostic channel.
        let warnings: Vec<_> = logs
            .iter()
            .filter(|log| log.target == module_path!().trim_end_matches("::tests"))
            .collect();
        assert_eq!(warnings.len(), 1);
        let warning = warnings[0];
        assert_eq!(warning.level, tracing::Level::WARN);
        assert_eq!(
            warning.field("host_machine_id"),
            Some(host.to_string().as_str())
        );
        assert_eq!(warning.field("ikm_version"), Some("7"));
        assert_eq!(warning.field("reason"), Some("lookup"));
        assert_eq!(warning.message, "NIC lockdown credential lookup failed");
        assert!(!format!("{logs:?}").contains("test-secret"));
    }

    // Expire the actual credential deadline after lookup starts, then make the
    // same frozen request succeed. No database clock participates in this wait.
    #[tokio::test]
    async fn credential_lookup_deadline_allows_the_same_request_to_retry() {
        let reader = Lookup {
            store: MemoryCredentialStore::default(),
            stalled: AtomicBool::new(true),
            failed: false,
            started: Notify::new(),
        };
        reader
            .store
            .set_credentials(
                &CredentialKey::NicLockdownIkm {
                    credential_type: NicLockdownIkm::SiteWide { version: 7 },
                },
                &Credentials::UsernamePassword {
                    username: String::new(),
                    password: "test-ikm".into(),
                },
            )
            .await
            .unwrap();
        let host = "fm100htes3rn1npvbtm5qd57dkilaag7ljugl1llmm7rfuq1ov50i0rpl30"
            .parse()
            .unwrap();
        let version = ConfigVersion::initial();
        let mut target = model::test_support::nic_firmware::target();
        target.original_lockdown = NicFirmwareLockdownPolicy::Locked {
            ikm_version: 7,
            mac_address: "02:00:00:00:00:01".parse().unwrap(),
        };
        let request = lockdown_request(
            host,
            version,
            &target,
            NicFirmwareLockdownStep::Change,
            NicFirmwareLockdownStatus::Unlocked,
            &reader,
        );
        tokio::pin!(request);
        tokio::time::timeout(Duration::from_secs(5), async {
            tokio::select! {
                _ = &mut request => panic!("a stalled lookup must await its deadline"),
                () = reader.started.notified() => {},
            }
        })
        .await
        .expect("lookup must start");
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(30)).await;
        tokio::time::resume();
        let (result, logs) =
            capture_logs_async(tokio::time::timeout(Duration::from_secs(5), &mut request)).await;
        let error = result
            .expect("credential deadline must finish the attempt")
            .expect_err("stalled lookup must fail visibly");
        assert!(matches!(error, CarbideError::UnavailableError(_)));
        let warnings: Vec<_> = logs
            .iter()
            .filter(|log| log.target == module_path!().trim_end_matches("::tests"))
            .collect();
        assert_eq!(warnings.len(), 1);
        let warning = warnings[0];
        assert_eq!(warning.level, tracing::Level::WARN);
        assert_eq!(
            warning.field("host_machine_id"),
            Some(host.to_string().as_str())
        );
        assert_eq!(warning.field("ikm_version"), Some("7"));
        assert_eq!(warning.field("reason"), Some("timeout"));
        assert_eq!(warning.message, "NIC lockdown credential lookup failed");
        assert!(!format!("{logs:?}").contains("test-ikm"));
        reader.stalled.store(false, Ordering::SeqCst);
        let retry = lockdown_request(
            host,
            version,
            &target,
            NicFirmwareLockdownStep::Change,
            NicFirmwareLockdownStatus::Unlocked,
            &reader,
        )
        .await
        .expect("retry after credential recovery");
        assert_eq!(retry.controller_state_version, version.to_string());
        assert_eq!(retry.expected_device, Some(target.selector.clone().into()));
        assert!(
            matches!(retry.request, Some(Request::Unlock(MlxDeviceUnlock { key })) if key.len() == 16)
        );
    }
}
