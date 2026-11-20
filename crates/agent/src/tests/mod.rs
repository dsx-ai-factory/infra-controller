/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 * http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */
mod bootstrap_ca;
pub(crate) mod common;
pub(crate) use common::{respond, respond_then_stall_before_trailers};
mod full;
mod lldp_collector;
mod metrics;
mod polling;
mod test_network_monitor;
mod upgrade;

/// Retries rejected partial BF4 discovery with newly visible live PF facts and no nested registration retries, preventing stale payload reuse after delayed sysfs acquisition.
#[tokio::test]
async fn bf4_registration_recollects_delayed_pf_with_one_retry_budget() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    // A loaded snapshot may contain PFs, but live sysfs initially exposes only a different, incomplete observation.
    let root = tempfile::tempdir().expect("temporary sysfs");
    let initial = root.path().join("first");
    std::fs::create_dir_all(initial.join("device")).expect("initial netdev PCI directory");
    std::fs::write(initial.join("device/vendor"), "0x15b3\n").expect("initial PCI vendor");
    std::fs::write(initial.join("device/device"), "0xa2df\n").expect("initial BF4 PCI device");
    std::fs::write(initial.join("phys_port_name"), "c0pf0").expect("initial port identity");
    std::fs::write(initial.join("address"), "02:00:00:00:00:10").expect("live initial MAC");
    let snapshot = ::rpc::DiscoveryInfo {
        dpu_info: Some(::rpc::machine_discovery::DpuData {
            part_number: "900-9D4B4-CWAA-TSA".to_string(), // Select required BF4 PF refresh.
            factory_mac_address: "02:00:00:00:00:01".to_string(),
            host_pfs: vec![::rpc::machine_discovery::DpuHostPf {
                pci_controller_id: 0,
                pf_id: 0,
                mac_address: "02:00:00:00:00:99".to_string(), // Stale PF MAC in the initial discovery snapshot.
            }],
            ..Default::default()
        }),
        ..Default::default()
    };
    let attempts = AtomicUsize::new(0);

    // Exercise the production retry loop; reject its first request before the selected PF becomes visible.
    crate::register_bf4_with_retry(
        snapshot,
        root.path(),
        carbide_host_support::registration::DiscoveryRetry { secs: 0, max: 2 },
        |request, retry| {
            // Each RPC attempt must receive fresh facts and disable its own retry budget.
            assert_eq!((retry.secs, retry.max), (0, 0));
            let inventory = request.dpu_info.expect("DPU request").host_pfs;
            assert_eq!(inventory[0].mac_address, "02:00:00:00:00:10");
            let attempt = attempts.fetch_add(1, Ordering::SeqCst);
            let result = if attempt == 0 {
                assert_eq!(inventory.len(), 1);
                // Introduce the selected PF only after the first partial request actually reaches registration.
                let delayed = root.path().join("delayed");
                std::fs::create_dir_all(delayed.join("device"))
                    .expect("delayed netdev PCI directory");
                std::fs::write(delayed.join("device/vendor"), "0x15b3\n")
                    .expect("delayed PCI vendor");
                std::fs::write(delayed.join("device/device"), "0xa2df\n")
                    .expect("delayed BF4 PCI device");
                std::fs::write(delayed.join("phys_port_name"), "c1pf0")
                    .expect("delayed port identity");
                std::fs::write(delayed.join("address"), "02:00:00:00:00:20").expect("delayed MAC");
                Err(eyre::Report::new(
                    carbide_host_support::registration::RegistrationError::TonicStatusError(
                        tonic::Status::failed_precondition("selected c1pf0 is not visible"),
                    ),
                ))
            } else {
                assert_eq!(
                    attempt, 1,
                    "registration should recover on the next attempt"
                );
                assert_eq!(inventory.len(), 2);
                assert_eq!((inventory[1].pci_controller_id, inventory[1].pf_id), (1, 0));
                assert_eq!(inventory[1].mac_address, "02:00:00:00:00:20");
                Ok(())
            };
            std::future::ready(result)
        },
    )
    .await
    .expect("registration recovers after fresh acquisition");

    // Successful completion requires the rejected partial request and a later request with the newly visible PF.
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
}

/// Retries a missing BF4 PF address before discovery and reports the first failure immediately,
/// limiting repeated diagnostics to once per minute.
/// Fresh acquisition after recovery prevents stale cached PF facts from escaping a failed read.
#[tokio::test(start_paused = true)]
async fn bf4_registration_retries_failed_pf_acquisition() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::task::Poll;
    use std::time::Duration;

    use carbide_instrument::testing::capture_logs_async;

    // Recognize the selected PF without its live address while the snapshot still contains a stale MAC.
    let root = tempfile::tempdir().expect("temporary sysfs");
    let selected = root.path().join("selected");
    std::fs::create_dir_all(selected.join("device")).expect("selected netdev PCI directory");
    std::fs::write(selected.join("device/vendor"), "0x15b3\n").expect("selected PCI vendor");
    std::fs::write(selected.join("device/device"), "0xa2df\n").expect("selected BF4 PCI device");
    std::fs::write(selected.join("phys_port_name"), "c1pf0").expect("selected port identity");
    let snapshot = ::rpc::DiscoveryInfo {
        dpu_info: Some(::rpc::machine_discovery::DpuData {
            part_number: "900-9D4B4-CWAA-TSA".to_string(), // Require live BF4 PF acquisition.
            host_pfs: vec![::rpc::machine_discovery::DpuHostPf {
                pci_controller_id: 1,
                pf_id: 0,
                mac_address: "02:00:00:00:00:99".to_string(), // Cached facts cannot replace a failed live read.
            }],
            ..Default::default()
        }),
        ..Default::default()
    };
    let attempts = AtomicUsize::new(0);

    // Poll the production retry loop while acquisition fails, before making the missing address available.
    let registration = crate::register_bf4_with_retry(
        snapshot,
        root.path(),
        carbide_host_support::registration::DiscoveryRetry { secs: 1, max: 3 },
        |request, retry| {
            attempts.fetch_add(1, Ordering::SeqCst);
            assert_eq!((retry.secs, retry.max), (0, 0));
            let inventory = request.dpu_info.expect("DPU request").host_pfs;
            assert_eq!(inventory.len(), 1);
            assert_eq!((inventory[0].pci_controller_id, inventory[0].pf_id), (1, 0));
            assert_eq!(inventory[0].mac_address, "02:00:00:00:00:20");
            std::future::ready(Ok(()))
        },
    );
    tokio::pin!(registration);
    let expected_directory = root.path().display().to_string();
    let ((), logs) = capture_logs_async(async {
        for delay_seconds in [
            // The first failed acquisition must identify the local cause before any discovery request.
            0,
            // A retry inside the warning interval must not flood logs when discovery retries are frequent.
            1,
            // A persistent failure must remain observable after the warning interval.
            // The preceding retry already advanced the paused clock by one second.
            59,
        ] {
            // Poll the next failed acquisition under the same paused clock used by the retry budget.
            tokio::time::advance(Duration::from_secs(delay_seconds)).await;
            let acquisition = futures_util::poll!(&mut registration);

            // Local failures keep discovery blocked while emitting only the permitted actionable diagnostics.
            assert!(acquisition.is_pending());
            assert_eq!(attempts.load(Ordering::SeqCst), 0);
            tracing::debug!("BF4 acquisition attempt completed");
        }

        // Restore the address and let the paused clock drive the retry delay before checking fresh acquisition.
        std::fs::write(selected.join("address"), "02:00:00:00:00:20").expect("selected MAC");
        tokio::time::sleep(Duration::from_secs(1)).await;

        // Poll directly so failure to recover cannot hang the test or hide behind cached discovery.
        let Poll::Ready(result) = futures_util::poll!(&mut registration) else {
            panic!("registration did not recover after the PF address became readable");
        };
        result.expect("registration recovers after acquisition failure");
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    })
    .await;

    // Separate each failed poll's warnings from recovery without changing subscribers while Tokio retains retry timers.
    let warning_counts: Vec<_> = logs
        .split(|log| log.message == "BF4 acquisition attempt completed")
        .map(|attempt_logs| {
            attempt_logs
                .iter()
                .filter(|log| log.level == tracing::Level::WARN)
                .count()
        })
        .collect();
    assert_eq!(warning_counts, [1, 0, 1, 0]);
    let warnings = logs
        .into_iter()
        .filter(|log| log.level == tracing::Level::WARN);
    for warning in warnings {
        assert_eq!(
            warning.message,
            "Could not collect BF4 host PFs during registration"
        );
        assert_eq!(
            warning.field("net_directory"),
            Some(expected_directory.as_str())
        );
        assert_eq!(warning.field("retry_delay_seconds"), Some("1"));
        assert!(
            warning
                .field("error")
                .expect("acquisition error")
                .contains("selected/address")
        );
    }
}

/// Shares one finite BF4 retry budget across acquisition and RPC failures, preserving the terminal typed error and stopping immediately on certificate failure to avoid repeated successful discovery.
#[tokio::test(start_paused = true)]
async fn bf4_registration_stops_after_retry_budget_is_exhausted() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::task::Poll;
    use std::time::Duration;

    // Start without the PF address so acquisition consumes the first attempt before RPC rejection can occur.
    let root = tempfile::tempdir().expect("temporary sysfs");
    let selected = root.path().join("selected");
    std::fs::create_dir_all(selected.join("device")).expect("selected netdev PCI directory");
    std::fs::write(selected.join("device/vendor"), "0x15b3\n").expect("selected PCI vendor");
    std::fs::write(selected.join("device/device"), "0xa2df\n").expect("selected BF4 PCI device");
    std::fs::write(selected.join("phys_port_name"), "c1pf0").expect("selected port identity");
    let snapshot = ::rpc::DiscoveryInfo {
        dpu_info: Some(::rpc::machine_discovery::DpuData {
            part_number: "900-9D4B4-CWAA-TSA".to_string(), // Exercise the BF4 acquisition and registration budget.
            ..Default::default()
        }),
        ..Default::default()
    };
    let attempts = AtomicUsize::new(0);

    // Reject every RPC under one outer budget and identify each error to distinguish the terminal attempt.
    let registration = crate::register_bf4_with_retry(
        snapshot.clone(),
        root.path(),
        carbide_host_support::registration::DiscoveryRetry { secs: 1, max: 2 },
        |_, retry| {
            assert_eq!((retry.secs, retry.max), (0, 0));
            let attempt = attempts.fetch_add(1, Ordering::SeqCst) + 1;
            std::future::ready(Err::<(), _>(eyre::Report::new(
                carbide_host_support::registration::RegistrationError::TonicStatusError(
                    tonic::Status::failed_precondition(format!(
                        "registration rejected attempt {attempt}"
                    )),
                ),
            )))
        },
    );
    tokio::pin!(registration);

    // Fail acquisition before any RPC, then restore the address without resetting the outer budget.
    assert!(futures_util::poll!(&mut registration).is_pending());
    assert_eq!(attempts.load(Ordering::SeqCst), 0);
    std::fs::write(selected.join("address"), "02:00:00:00:00:20").expect("selected MAC");
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(futures_util::poll!(&mut registration).is_pending());
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
    tokio::time::sleep(Duration::from_secs(1)).await;

    // One acquisition failure and two RPC failures exhaust the same budget; no fourth attempt may be scheduled.
    let Poll::Ready(result) = futures_util::poll!(&mut registration) else {
        panic!("registration exceeded its two-retry budget");
    };
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    let failure = result.expect_err("registration exhausts its retry budget");
    let Some(carbide_host_support::registration::RegistrationError::TonicStatusError(status)) =
        failure.downcast_ref::<carbide_host_support::registration::RegistrationError>()
    else {
        panic!("registration must preserve its typed RPC error");
    };
    assert_eq!(status.message(), "registration rejected attempt 2");

    // Model successful discovery followed by a local certificate-write error under the same retry settings.
    let terminal_attempts = AtomicUsize::new(0);
    let failure = crate::register_bf4_with_retry(
        snapshot,
        root.path(),
        carbide_host_support::registration::DiscoveryRetry { secs: 1, max: 2 },
        |_, _| {
            terminal_attempts.fetch_add(1, Ordering::SeqCst);
            std::future::ready(Err::<(), _>(eyre::Report::new(
                carbide_host_support::registration::RegistrationError::ClientCertificateError(
                    eyre::eyre!("certificate directory is read-only"),
                ),
            )))
        },
    )
    .await
    .expect_err("local certificate failure must return immediately");

    // A terminal local failure must reach the caller without repeating committed discovery.
    assert_eq!(terminal_attempts.load(Ordering::SeqCst), 1);
    assert!(matches!(
        failure.downcast_ref::<carbide_host_support::registration::RegistrationError>(),
        Some(carbide_host_support::registration::RegistrationError::ClientCertificateError(_))
    ));
}

#[test]
fn pretty_cmd_preserves_empty_and_multiple_arguments() {
    carbide_test_support::value_scenarios!(run = |arguments: &[&str]| {
        let mut command = std::process::Command::new("program");
        command.args(arguments);
        crate::pretty_cmd(&command)
    };
        "no arguments retains the trailing space" {
            &[][..] => "program ".to_string(),
        }
        "multiple arguments are separated by spaces" {
            &["first", "two words"][..] => "program first two words".to_string(),
        }
    );
}
