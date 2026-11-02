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

//! Host desired maintenance configuration and independent per-DPU Xplane observations.

use std::collections::BTreeMap;

use config_version::ConfigVersion;
use serde::{Deserialize, Serialize};

use crate::expected_machine::ExpectedInterface;

/// Desired per-interface maintenance requests stored in a host's `spx_multiplane` column.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineSpxMultiplane {
    /// Requests keyed by declared logical interface names, such as `eth_r2_p6`.
    pub interfaces: BTreeMap<String, MachineSpxMultiplaneInterface>,
}

impl MachineSpxMultiplane {
    /// Declare CX9 ports for observation without creating maintenance requests.
    /// Hosts without these declarations do not have multiplane configuration.
    pub fn from_expected_interfaces(
        interfaces: &[ExpectedInterface],
    ) -> Result<Option<Self>, String> {
        let mut configuration = Self::default();
        for interface in interfaces {
            if interface.nic_type.as_deref() != Some("CX9") {
                continue;
            }
            let Some(name) = interface.logical_ifname.as_deref() else {
                continue;
            };
            if Self::interface_coordinates(name).is_none() {
                continue;
            }
            if configuration
                .interfaces
                .insert(name.to_string(), Default::default())
                .is_some()
            {
                return Err(format!("duplicate declared logical interface {name}"));
            }
        }
        // Canonical names and unique keys limit this map to four rails of eight planes.
        if configuration.interfaces.len() > 32 {
            return Err("at most 32 multiplane interfaces are supported".to_string());
        }
        Ok((!configuration.interfaces.is_empty()).then_some(configuration))
    }

    /// Coordinates of a canonical `eth_r<0..3>_p<0..7>` logical name.
    pub fn interface_coordinates(name: &str) -> Option<(u8, u8)> {
        let (rail, plane) = name.strip_prefix("eth_r")?.split_once("_p")?;
        let rail = rail.parse::<u8>().ok()?;
        let plane = plane.parse::<u8>().ok()?;
        (rail <= 3 && plane <= 7 && name == format!("eth_r{rail}_p{plane}"))
            .then_some((rail, plane))
    }

    /// Reconcile declarations without resetting existing operator requests.
    pub fn synchronize_declarations(&mut self, declared: Self) {
        self.interfaces
            .retain(|name, _| declared.interfaces.contains_key(name));
        for (name, initial) in declared.interfaces {
            self.interfaces.entry(name).or_insert(initial);
        }
    }
}

/// Desired maintenance state and revision for one declared Astra interface.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineSpxMultiplaneInterface {
    /// Explicit operator request. None observes the flavor-initialized state without changing it.
    #[serde(default)]
    pub cfg_maintenance_mode: Option<bool>,
    /// Optional operator-provided URL describing the maintenance request.
    pub reference: Option<String>,
    /// Version of this interface's desired state, used to match application acknowledgments.
    pub revision: ConfigVersion,
}

/// Maintenance observations stored on the reporting DPU, independently of host intent.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineSpxMultiplaneStatus {
    /// Last observation for each declared logical interface on this DPU.
    pub interfaces: BTreeMap<String, MachineSpxMultiplaneObservation>,
}

/// Readback for one logical interface from one DPU agent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineSpxMultiplaneObservation {
    /// Actual state; absent when readback failed.
    pub maintenance_mode: Option<bool>,
    /// Host desired revision attempted by this DPU.
    pub revision: ConfigVersion,
    /// Resolved primary Linux interface name supplied to Xplane.
    pub netdev: Option<String>,
    /// Failure reported by the agent or detected from a mismatched readback.
    pub error: Option<String>,
    /// Agent observation time, used to reject older reports for the same revision.
    pub observed_at: chrono::DateTime<chrono::Utc>,
}

impl Default for MachineSpxMultiplaneInterface {
    fn default() -> Self {
        Self {
            cfg_maintenance_mode: None,
            reference: None,
            revision: ConfigVersion::initial(),
        }
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::Yields;
    use carbide_test_support::{Case, check_cases};
    use serde_json::json;

    use super::*;

    #[test]
    fn declared_interfaces_initialize_and_preserve_operator_requests() {
        let interface = |name: &str| ExpectedInterface {
            logical_ifname: Some(name.to_string()),
            physical_ifname: Some("C1-1-L1".to_string()),
            nic_type: Some("CX9".to_string()),
            ..Default::default()
        };
        let declaration = interface("eth_r2_p6");
        let mut configuration =
            MachineSpxMultiplane::from_expected_interfaces(std::slice::from_ref(&declaration))
                .unwrap()
                .unwrap();
        assert!(
            configuration.interfaces["eth_r2_p6"]
                .cfg_maintenance_mode
                .is_none()
        );
        configuration
            .interfaces
            .get_mut("eth_r2_p6")
            .unwrap()
            .cfg_maintenance_mode = Some(false);
        let requested_revision = configuration.interfaces["eth_r2_p6"].revision;
        configuration.synchronize_declarations(
            MachineSpxMultiplane::from_expected_interfaces(&[
                declaration.clone(),
                interface("eth_r0_p0"),
            ])
            .unwrap()
            .unwrap(),
        );
        assert_eq!(
            configuration.interfaces["eth_r2_p6"].cfg_maintenance_mode,
            Some(false)
        );
        assert_eq!(
            configuration.interfaces["eth_r2_p6"].revision,
            requested_revision
        );
        assert!(
            configuration.interfaces["eth_r0_p0"]
                .cfg_maintenance_mode
                .is_none()
        );
        assert!(
            MachineSpxMultiplane::from_expected_interfaces(&[declaration.clone(), declaration])
                .is_err()
        );
        assert!(
            MachineSpxMultiplane::from_expected_interfaces(&[
                ExpectedInterface::default(),
                interface("other0")
            ])
            .unwrap()
            .is_none()
        );
    }

    #[test]
    fn new_interface_starts_without_a_maintenance_request() {
        let interface = MachineSpxMultiplaneInterface::default();
        assert!(interface.cfg_maintenance_mode.is_none());
        assert!(interface.reference.is_none());
        assert_eq!(interface.revision.version_nr(), 1);
    }

    #[test]
    fn interface_json_preserves_optional_configured_maintenance_mode() {
        let revision = ConfigVersion::new(7);
        check_cases(
            [
                Case {
                    scenario: "maintenance on with reference",
                    input: json!({
                        "cfg_maintenance_mode": true,
                        "reference": "https://example.com/ticket/123",
                        "revision": revision,
                    }),
                    expect: Yields(MachineSpxMultiplaneInterface {
                        cfg_maintenance_mode: Some(true),
                        reference: Some("https://example.com/ticket/123".to_string()),
                        revision,
                    }),
                },
                Case {
                    scenario: "explicit maintenance off",
                    input: json!({"cfg_maintenance_mode": false, "reference": null, "revision": revision}),
                    expect: Yields(MachineSpxMultiplaneInterface {
                        cfg_maintenance_mode: Some(false),
                        reference: None,
                        revision,
                    }),
                },
                Case {
                    scenario: "missing mode observes without changing a plane",
                    input: json!({"reference": null, "revision": revision}),
                    expect: Yields(MachineSpxMultiplaneInterface {
                        cfg_maintenance_mode: None,
                        reference: None,
                        revision,
                    }),
                },
            ],
            |input| {
                serde_json::from_value::<MachineSpxMultiplaneInterface>(input)
                    .and_then(|request| {
                        // Exercise the persisted JSON representation in both directions.
                        serde_json::from_value(serde_json::to_value(request)?)
                    })
                    .map_err(drop)
            },
        );
    }
}
