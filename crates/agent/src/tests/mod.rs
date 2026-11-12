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
    std::fs::create_dir(&initial).expect("initial netdev");
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
                std::fs::create_dir(&delayed).expect("delayed netdev");
                std::fs::write(delayed.join("phys_port_name"), "c1pf0")
                    .expect("delayed port identity");
                std::fs::write(delayed.join("address"), "02:00:00:00:00:20").expect("delayed MAC");
                Err(eyre::Report::new(tonic::Status::failed_precondition(
                    "selected c1pf0 is not visible",
                )))
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

/// Retries a missing BF4 PF address before sending discovery, preventing stale cached PF facts from escaping a failed acquisition.
#[tokio::test(start_paused = true)]
async fn bf4_registration_retries_failed_pf_acquisition() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::task::Poll;
    use std::time::Duration;

    // Recognize the selected PF without its live address while the snapshot still contains a stale MAC.
    let root = tempfile::tempdir().expect("temporary sysfs");
    let selected = root.path().join("selected");
    std::fs::create_dir(&selected).expect("selected netdev");
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
        carbide_host_support::registration::DiscoveryRetry { secs: 1, max: 1 },
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
    assert!(futures_util::poll!(&mut registration).is_pending());
    assert_eq!(attempts.load(Ordering::SeqCst), 0);

    // Restore the address and let the paused clock drive the retry delay before checking fresh acquisition.
    std::fs::write(selected.join("address"), "02:00:00:00:00:20").expect("selected MAC");
    tokio::time::sleep(Duration::from_secs(1)).await;

    // Poll directly so failure to recover cannot hang the test or hide behind cached discovery.
    let Poll::Ready(result) = futures_util::poll!(&mut registration) else {
        panic!("registration did not recover after the PF address became readable");
    };
    result.expect("registration recovers after acquisition failure");
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
}

/// Returns the final registration error after the configured BF4 retry count, preventing unbounded or nested registration attempts.
#[tokio::test(start_paused = true)]
async fn bf4_registration_stops_after_retry_budget_is_exhausted() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::task::Poll;
    use std::time::Duration;

    // Keep PF acquisition valid so only repeated registration rejection consumes the retry budget.
    let root = tempfile::tempdir().expect("temporary sysfs");
    let selected = root.path().join("selected");
    std::fs::create_dir(&selected).expect("selected netdev");
    std::fs::write(selected.join("phys_port_name"), "c1pf0").expect("selected port identity");
    std::fs::write(selected.join("address"), "02:00:00:00:00:20").expect("selected MAC");
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
        snapshot,
        root.path(),
        carbide_host_support::registration::DiscoveryRetry { secs: 1, max: 2 },
        |_, retry| {
            assert_eq!((retry.secs, retry.max), (0, 0));
            let attempt = attempts.fetch_add(1, Ordering::SeqCst) + 1;
            std::future::ready(Err::<(), _>(eyre::eyre!(
                "registration rejected attempt {attempt}"
            )))
        },
    );
    tokio::pin!(registration);

    // Two retry delays permit the initial attempt and two retries without relying on wall-clock time.
    for expected_attempts in 1..=2 {
        assert!(futures_util::poll!(&mut registration).is_pending());
        assert_eq!(attempts.load(Ordering::SeqCst), expected_attempts);
        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    // A further pending retry is a failure; the caller must receive the third attempt's error now.
    let Poll::Ready(result) = futures_util::poll!(&mut registration) else {
        panic!("registration exceeded its two-retry budget");
    };
    assert_eq!(attempts.load(Ordering::SeqCst), 3);
    assert_eq!(
        result
            .expect_err("registration exhausts its retry budget")
            .to_string(),
        "registration rejected attempt 3"
    );
}
