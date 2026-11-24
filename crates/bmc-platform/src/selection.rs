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

use serde::{Deserialize, Serialize};
use strum_macros::{Display, EnumCount, EnumIter, EnumString, IntoStaticStr};

/// A BMC capability with its own driver trait and driver-map slot.
///
/// Declaration order is the stable wire order and the driver-map slot order.
#[derive(
    Clone,
    Copy,
    Debug,
    Display,
    EnumCount,
    EnumIter,
    EnumString,
    Eq,
    Hash,
    IntoStaticStr,
    Ord,
    PartialEq,
    PartialOrd,
    Serialize,
    Deserialize,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum Capability {
    Power,
    BmcControl,
    Bios,
    BootOrder,
    SecureBoot,
    Lockdown,
    Accounts,
    Firmware,
    Storage,
    Dpu,
    Attestation,
    Console,
    SuperNic,
}

impl Capability {
    /// The capability's wire name.
    pub fn as_str(self) -> &'static str {
        self.into()
    }
}

#[cfg(test)]
mod tests {
    use strum::IntoEnumIterator;

    use super::*;

    #[test]
    fn capabilities_round_trip_through_their_wire_names() {
        for capability in Capability::iter() {
            let encoded = serde_json::to_string(&capability).expect("capability serializes");
            assert_eq!(encoded, format!("\"{capability}\""));
            let decoded: Capability =
                serde_json::from_str(&encoded).expect("capability deserializes");
            assert_eq!(decoded, capability);
            assert_eq!(capability.as_str().parse::<Capability>(), Ok(capability));
        }
        assert_eq!(Capability::BmcControl.as_str(), "bmc_control");
        assert!("nope".parse::<Capability>().is_err());
    }
}
