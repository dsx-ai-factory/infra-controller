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

//! Admitted NIC firmware operation fixtures, without physical hardware qualification.

use carbide_libmlx_model::firmware::FirmwareSpec;
use config_version::ConfigVersion;
use mac_address::MacAddress;

use crate::machine::nic_firmware::{
    NicFirmwareActivation, NicFirmwareBaselineContinuation, NicFirmwareExecutionOptions,
    NicFirmwareLockdownPolicy, NicFirmwareOperation, NicFirmwarePhase, NicFirmwarePurpose,
    NicFirmwareTarget,
};
use crate::nic_firmware::{
    NicFirmwareApproach, NicFirmwareArtifact, NicFirmwareCardSelector, NicFirmwareProfileEntry,
    NicFirmwareResolution, NicFirmwareSelectionScope,
};

/// One unlocked card's immutable request at version `1.2.3`, before any
/// evidence; tests change the specific identity or policy they exercise.
pub fn target() -> NicFirmwareTarget {
    NicFirmwareTarget {
        selector: NicFirmwareCardSelector::Mac(MacAddress::new([2, 0, 0, 0, 0, 1])),
        pci_name: "0000:01:00.0".to_string(),
        resolution: NicFirmwareResolution {
            scope: NicFirmwareSelectionScope::Site,
            profile_id: "baseline".parse().expect("fixture profile ID"),
            profile_version: ConfigVersion::initial(),
            entry: NicFirmwareProfileEntry {
                firmware: FirmwareSpec {
                    part_number: "test-card".to_string(),
                    psid: "test-psid".to_string(),
                    version: "1.2.3".to_string(),
                },
                image: NicFirmwareArtifact {
                    url: "https://firmware.example.test/image.bin"
                        .parse()
                        .expect("fixture URL"),
                    sha256: "ab".repeat(32),
                },
                device_config: None,
                approach: NicFirmwareApproach::Scout,
            },
        },
        original_lockdown: NicFirmwareLockdownPolicy::Unlocked,
        supervisor_instance_id: None,
        receipt: None,
        diagnostic: None,
    }
}

/// One tenant-free workflow, activated by a host reboot and requesting no
/// verification, before helper observation or any hardware effect.
pub fn operation() -> NicFirmwareOperation {
    NicFirmwareOperation {
        operation_id: "nic-workflow".to_string(),
        purpose: NicFirmwarePurpose::Baseline {
            continuation: NicFirmwareBaselineContinuation::Ready,
        },
        targets: vec![target()],
        options: NicFirmwareExecutionOptions {
            verify_image: false,
            verify_version: false,
            activation: NicFirmwareActivation::HostReboot,
        },
        phase: NicFirmwarePhase::ObservingSupervisor { target_index: 0 },
        lockdown: Default::default(),
    }
}
