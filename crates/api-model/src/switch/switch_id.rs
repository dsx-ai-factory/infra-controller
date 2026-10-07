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

use carbide_uuid::switch::{SwitchId, SwitchIdSource, SwitchType};
use sha2::{Digest, Sha256};

/// Generates a Switch ID from the hardware fingerprint
///
/// Returns `MissingHardwareInfo::Serial` when the BMC hasn't surfaced a
/// usable chassis serial yet. The literal `"NA"` is treated the same as
/// amissing serial: the current switch BMC seems to return it in some
/// type of error situation, and hashing it would result in us having a
/// junk `SwitchId` that drifts to a real one once the BMC reports its
/// actual serial on a later exploration cycle (which would ultimately
/// give us two SwitchIds pointing to the same hardware).
pub fn from_hardware_info_with_type(
    serial: &str,
    vendor: &str,
    model: &str,
    source: SwitchIdSource,
    switch_type: SwitchType,
) -> Result<SwitchId, MissingHardwareInfo> {
    if serial == "NA" {
        return Err(MissingHardwareInfo::Serial);
    }

    let bytes = format!("s{}-b{}-c{}", serial, vendor, model);
    let mut hasher = Sha256::new();
    hasher.update(bytes.as_bytes());

    Ok(SwitchId::new(source, hasher.finalize().into(), switch_type))
}

/// Generates a Switch ID from a hardware fingerprint
pub fn from_hardware_info(
    serial: &str,
    vendor: &str,
    model: &str,
    source: SwitchIdSource,
    switch_type: SwitchType,
) -> Result<SwitchId, MissingHardwareInfo> {
    from_hardware_info_with_type(serial, vendor, model, source, switch_type)
}

#[derive(Debug, Copy, Clone, PartialEq, thiserror::Error)]
pub enum MissingHardwareInfo {
    #[error("the TPM certificate has no bytes")]
    TPMCertEmpty,
    #[error("serial number missing (product, board and chassis)")]
    Serial,
    #[error("TPM and DMI data are both missing")]
    All,
}
