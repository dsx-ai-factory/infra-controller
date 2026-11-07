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

use std::path::Path;
use std::time::Duration;

use carbide_host_support::agent_config::IterationTime;
use carbide_host_support::hardware_enumeration::HardwareEnumerationError;
use rpc::DiscoveryInfo;
use rpc::machine_discovery::DpuData;
use tokio::time::Instant;

use crate::enumerate_registration_hardware;

/// Retries a sysfs read failure and an empty BF4 snapshot before returning complete inventory,
/// preserving startup recovery within the configured budget.
#[tokio::test(start_paused = true)]
async fn retries_transient_failure_and_incomplete_bf4_inventory() {
    // BF4 host-PF facts drive completeness; the remaining snapshot fields provide DPU VPD.
    let incomplete = DiscoveryInfo {
        dpu_info: Some(DpuData {
            part_number: "900-9D4B4-CWAA-TSA".to_string(), // BF4 requires nonempty host-PF inventory.
            ..Default::default()
        }),
        ..Default::default()
    };
    // Add one valid observation without imposing the API's selected controller.
    let mut complete = incomplete.clone();
    complete.dpu_info.as_mut().expect("DPU VPD").host_pfs =
        vec![::rpc::machine_discovery::DpuHostPf {
            pci_controller_id: 2,
            pf_id: 0,
            mac_address: "96:ab:d0:3e:5f:a6".to_string(),
            physical_port_name: "c2pf0".to_string(),
            ..Default::default()
        }];
    let period = IterationTime {
        discovery_retry_secs: 2, // Use a nondefault delay to prove the registration setting is applied.
        discovery_retries_max: 2, // Allow recovery on the final permitted retry.
        ..Default::default()
    };
    let mut attempts = Vec::new();

    // Fail the source once, then reject readable but incomplete inventory before recovery.
    let recovered = enumerate_registration_hardware(&period, || {
        attempts.push(Instant::now());
        match attempts.len() {
            // A netdev attribute can be unavailable before inventory validation runs.
            1 => Err(HardwareEnumerationError::HostPfSysfs {
                path: Path::new("/sys/class/net/host-pf/address").to_path_buf(),
                source: std::io::ErrorKind::NotFound.into(),
            }),
            // A successful read still requires BF4 host-PF facts.
            2 => Ok(incomplete.clone()),
            // Complete facts must end retries rather than consume another attempt.
            _ => Ok(complete.clone()),
        }
    })
    .await
    .expect("complete BF4 inventory on the final retry");

    // Return only the complete snapshot and apply the configured wait to both failure kinds.
    assert_eq!(recovered, complete);
    assert_eq!(attempts.len(), 3);
    assert!(
        attempts
            .windows(2)
            .all(|pair| { pair[1] - pair[0] == Duration::from_secs(period.discovery_retry_secs) })
    );
}

/// Returns the BF4 completeness error after the configured retries are exhausted so registration
/// cannot wait indefinitely on absent host-PF facts.
#[tokio::test(start_paused = true)]
async fn incomplete_bf4_inventory_exhausts_registration_retries() {
    // Keep BF4 host-PF inventory absent throughout a smaller retry budget.
    let incomplete = DiscoveryInfo {
        dpu_info: Some(DpuData {
            part_number: "900-9D4B4-CWAA-TSA".to_string(), // BF4 requires host-PF facts before registration.
            ..Default::default()
        }),
        ..Default::default()
    };
    let period = IterationTime {
        discovery_retry_secs: 3,  // A second delay catches hard-coded retry timing.
        discovery_retries_max: 1, // One retry permits two observations in total.
        ..Default::default()
    };
    let mut attempts = Vec::new();

    // Repeated successful reads must still fail when BF4 inventory remains empty.
    let error = enumerate_registration_hardware(&period, || {
        attempts.push(Instant::now());
        Ok(incomplete.clone())
    })
    .await
    .expect_err("incomplete BF4 inventory must exhaust its retry budget");

    // Preserve the typed completeness failure and stop at the configured retry limit.
    assert!(matches!(
        error.downcast_ref::<HardwareEnumerationError>(),
        Some(HardwareEnumerationError::DpuHostPf(_))
    ));
    assert_eq!(attempts.len(), 2);
    assert_eq!(
        attempts[1] - attempts[0],
        Duration::from_secs(period.discovery_retry_secs)
    );
}
