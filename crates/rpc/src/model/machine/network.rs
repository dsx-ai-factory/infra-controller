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
use std::collections::HashSet;
use std::time::SystemTime;

use chrono::{DateTime, Utc};
use model::instance::status::network::{
    InstanceInterfaceStatusObservation, InstanceNetworkStatusObservation,
};
use model::machine::network::{
    DpuFabricInterfaceStatusObservation, DpuLinkStatusObservation, MachineNetworkStatusObservation,
    ManagedHostQuarantineMode, ManagedHostQuarantineState, ServiceInterfaceStatusObservation,
    ServiceNetworkStatusObservation,
};

use crate::errors::RpcDataConversionError;
use crate::forge as rpc;

impl From<rpc::FabricInterfaceData> for DpuFabricInterfaceStatusObservation {
    fn from(interface: rpc::FabricInterfaceData) -> Self {
        Self {
            interface_name: interface.interface_name,
            link_data: interface.link_data.map(Into::into),
        }
    }
}

impl From<rpc::LinkData> for DpuLinkStatusObservation {
    fn from(link: rpc::LinkData) -> Self {
        Self {
            link_type: link.link_type,
            state: link.state,
            carrier_up: link.carrier_up,
            mtu: link.mtu,
            carrier_up_count: link.carrier_up_count,
            carrier_down_count: link.carrier_down_count,
        }
    }
}

impl From<DpuFabricInterfaceStatusObservation> for rpc::FabricInterfaceData {
    fn from(interface: DpuFabricInterfaceStatusObservation) -> Self {
        Self {
            interface_name: interface.interface_name,
            link_data: interface.link_data.map(Into::into),
        }
    }
}

impl From<DpuLinkStatusObservation> for rpc::LinkData {
    fn from(link: DpuLinkStatusObservation) -> Self {
        Self {
            link_type: link.link_type,
            state: link.state,
            carrier_up: link.carrier_up,
            mtu: link.mtu,
            carrier_up_count: link.carrier_up_count,
            carrier_down_count: link.carrier_down_count,
        }
    }
}

impl TryFrom<rpc::ServiceNetworkStatusObservation> for ServiceNetworkStatusObservation {
    type Error = RpcDataConversionError;

    /// Validates service identities and selected-VPC NSGs before they become applied evidence.
    fn try_from(report: rpc::ServiceNetworkStatusObservation) -> Result<Self, Self::Error> {
        // Reject the complete report when an identity or policy cannot provide applied evidence.
        let mut identities = HashSet::new();
        let interfaces = report
            .interfaces
            .into_iter()
            .map(|interface| {
                let identity = interface.internal_uuid.ok_or(Self::Error::MissingArgument(
                    "service_interface.internal_uuid",
                ))?;
                let internal_uuid: uuid::Uuid = (&identity).try_into().map_err(|_| {
                    Self::Error::InvalidUuid(
                        "service_interface.internal_uuid",
                        identity.to_string(),
                    )
                })?;
                // Repeated identities cannot unambiguously acknowledge one service interface.
                if !identities.insert(internal_uuid) {
                    return Err(Self::Error::InvalidArgument(
                        "duplicate service-interface internal_uuid".to_string(),
                    ));
                }
                let network_security_group = interface
                    .network_security_group
                    .map(|nsg| {
                        // Service policy comes only from the selected VPC, never an instance override.
                        if nsg.source != rpc::NetworkSecurityGroupSource::NsgSourceVpc as i32 {
                            return Err(Self::Error::InvalidArgument(
                                "service-interface NSG must come from the selected VPC".to_string(),
                            ));
                        }
                        nsg.try_into()
                    })
                    .transpose()?;
                Ok(ServiceInterfaceStatusObservation {
                    internal_uuid,
                    network_security_group,
                })
            })
            .collect::<Result<Vec<_>, Self::Error>>()?;
        Ok(Self { interfaces })
    }
}

impl TryFrom<rpc::DpuNetworkStatus> for MachineNetworkStatusObservation {
    type Error = RpcDataConversionError;

    fn try_from(obs: rpc::DpuNetworkStatus) -> Result<Self, Self::Error> {
        let observed_at = match obs.observed_at {
            Some(timestamp) => {
                let system_time = SystemTime::try_from(timestamp)
                    .map_err(|_| Self::Error::InvalidTimestamp(timestamp.to_string()))?;
                DateTime::from(system_time)
            }
            None => Utc::now(),
        };

        // We're going to piggy-back on InstanceNetworkStatusObservation
        // to get the instance_config_version for now.
        let instance_config_version = match obs.instance_config_version {
            Some(version_string) => match version_string.as_str().parse() {
                Ok(version) => Some(version),
                _ => {
                    return Err(RpcDataConversionError::InvalidConfigVersion(format!(
                        "applied_config.instance_config_version: {version_string}"
                    )));
                }
            },
            _ => None,
        };

        let instance_network_observation =
            if let Some(version_string) = obs.instance_network_config_version {
                let Ok(version) = version_string.as_str().parse() else {
                    return Err(RpcDataConversionError::InvalidConfigVersion(format!(
                        "applied_config.instance_network_config_version: {version_string}"
                    )));
                };
                let mut interfaces: Vec<InstanceInterfaceStatusObservation> = vec![];
                for iface in obs.interfaces {
                    let v = iface.try_into()?;
                    interfaces.push(v);
                }

                Some(InstanceNetworkStatusObservation {
                    config_version: version,
                    instance_config_version,
                    observed_at,
                    interfaces,
                })
            } else {
                None
            };

        // Service metadata must not gate the existing host and instance acknowledgements.
        let service_network_observation = obs.service_network_observation.and_then(|report| {
            ServiceNetworkStatusObservation::try_from(report)
                .inspect_err(|error| {
                    tracing::warn!(machine_id = ?obs.dpu_machine_id, %error, "Ignoring malformed service network observation");
                })
                .ok()
        });

        Ok(MachineNetworkStatusObservation {
            observed_at,
            machine_id: obs
                .dpu_machine_id
                .ok_or(Self::Error::MissingArgument("dpu_machine_id"))?,
            agent_version: obs.dpu_agent_version.clone(),
            network_config_version: obs.network_config_version.and_then(|n| n.parse().ok()),
            client_certificate_expiry: obs.client_certificate_expiry_unix_epoch_secs,
            agent_version_superseded_at: None,
            instance_network_observation,
            service_network_observation,
            fabric_interfaces: obs.fabric_interfaces.into_iter().map(Into::into).collect(),
            accepted_lldp_snapshot_fingerprint: None,
        })
    }
}

// TODO: This API is only used by the carbide-web generating the Network Status page
// It improperly returns the values of a lot of things - since those are not actually
// persisted.
// It would be preferable to migrate carbide-web from reading the status to using
// a better supported API. E.g. the FindMachinesByIds one.
impl From<MachineNetworkStatusObservation> for rpc::DpuNetworkStatus {
    fn from(m: MachineNetworkStatusObservation) -> rpc::DpuNetworkStatus {
        rpc::DpuNetworkStatus {
            dpu_machine_id: Some(m.machine_id),
            dpu_agent_version: m.agent_version.clone(),
            observed_at: Some(m.observed_at.into()),
            network_config_version: m.network_config_version.map(|v| v.version_string()),
            instance_id: None,
            instance_config_version: None,
            instance_network_config_version: None,
            interfaces: vec![],
            network_config_error: None,
            client_certificate_expiry_unix_epoch_secs: None,
            dpu_health: None,
            fabric_interfaces: m.fabric_interfaces.into_iter().map(Into::into).collect(),
            last_dhcp_requests: vec![],
            dpu_extension_service_version: None,
            dpu_extension_services: vec![],
            astra_config_status: None,
            lldp: None,
            lldp_snapshot_fingerprint: None,
            service_network_observation: None, // Keep service-interface details out of the aggregate response.
        }
    }
}

impl From<ManagedHostQuarantineState> for rpc::ManagedHostQuarantineState {
    fn from(m: ManagedHostQuarantineState) -> Self {
        Self {
            mode: rpc::ManagedHostQuarantineMode::from(m.mode) as i32,
            reason: m.reason,
        }
    }
}

impl From<ManagedHostQuarantineMode> for rpc::ManagedHostQuarantineMode {
    fn from(m: ManagedHostQuarantineMode) -> Self {
        match m {
            ManagedHostQuarantineMode::BlockAllTraffic => {
                rpc::ManagedHostQuarantineMode::BlockAllTraffic
            }
        }
    }
}

impl TryFrom<rpc::ManagedHostQuarantineState> for ManagedHostQuarantineState {
    type Error = RpcDataConversionError;
    fn try_from(value: rpc::ManagedHostQuarantineState) -> Result<Self, Self::Error> {
        Ok(Self {
            reason: value.reason,
            mode: rpc::ManagedHostQuarantineMode::try_from(value.mode)
                .map_err(|_| {
                    RpcDataConversionError::InvalidValue(value.mode.to_string(), "mode".to_string())
                })?
                .into(),
        })
    }
}

impl From<rpc::ManagedHostQuarantineMode> for ManagedHostQuarantineMode {
    fn from(m: rpc::ManagedHostQuarantineMode) -> Self {
        match m {
            rpc::ManagedHostQuarantineMode::BlockAllTraffic => Self::BlockAllTraffic,
        }
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Case;
    use carbide_test_support::Outcome::Yields;

    use super::*;

    /// Keeps valid host acknowledgements when service metadata is omitted or malformed, and
    /// preserves absent NSGs and present empty reports so neither can become false applied evidence.
    /// Retains distinct identities sharing an NSG, and discards the complete service report when
    /// one interface carries malformed NSG metadata.
    #[test]
    fn service_observations_preserve_presence_and_isolate_malformed_metadata() {
        // Stable identities and a VPC-sourced NSG drive conversion; the outer status supplies
        // valid host and instance acknowledgements whose processing must remain independent.
        let valid_interface = rpc::ServiceInterfaceStatusObservation {
            internal_uuid: Some(uuid::uuid!("40000000-0000-0000-0000-000000000001").into()), // Keep UUID validation satisfied for NSG cases.
            network_security_group: Some(rpc::NetworkSecurityGroupStatus {
                source: rpc::NetworkSecurityGroupSource::NsgSourceVpc.into(), // Keep NSG source validation satisfied.
                id: "service-vpc-nsg".to_string(), // Keep NSG ID validation satisfied.
                version: "V1-T1".to_string(),      // Keep NSG version parsing satisfied.
            }),
        };
        let mut valid = rpc::ServiceNetworkStatusObservation {
            interfaces: vec![valid_interface.clone()],
        };
        let invalid_nsg = |source, id: &str, version: &str| {
            Some(rpc::ServiceNetworkStatusObservation {
                interfaces: vec![rpc::ServiceInterfaceStatusObservation {
                    network_security_group: Some(rpc::NetworkSecurityGroupStatus {
                        source,                       // Vary NSG source without changing the valid service UUID.
                        id: id.to_string(),           // Isolate ID validation in the empty-ID case.
                        version: version.to_string(), // Isolate parsing in the malformed-version case.
                    }),
                    ..valid_interface.clone()
                }],
            })
        };
        let mut applied = ServiceNetworkStatusObservation {
            interfaces: vec![ServiceInterfaceStatusObservation {
                internal_uuid: uuid::uuid!("40000000-0000-0000-0000-000000000001"),
                network_security_group: Some(
                    model::network_security_group::NetworkSecurityGroupStatusObservation {
                        source: model::network_security_group::NetworkSecurityGroupSource::Vpc,
                        id: "service-vpc-nsg".parse().expect("NSG ID"),
                        version: "V1-T1".parse().expect("NSG version"),
                    },
                ),
            }],
        };
        let no_nsg = rpc::ServiceNetworkStatusObservation {
            interfaces: vec![rpc::ServiceInterfaceStatusObservation {
                network_security_group: None, // Separate valid NSG absence from malformed metadata.
                ..valid_interface.clone()
            }],
        };
        let no_nsg_applied = ServiceNetworkStatusObservation {
            interfaces: vec![ServiceInterfaceStatusObservation {
                network_security_group: None,
                ..applied.interfaces[0].clone()
            }],
        };

        // Give two distinct service interfaces the same VPC NSG in the supplied and expected reports.
        let second_identity = uuid::uuid!("40000000-0000-0000-0000-000000000002");
        valid
            .interfaces
            .push(rpc::ServiceInterfaceStatusObservation {
                internal_uuid: Some(second_identity.into()), // Distinguish shared NSGs from duplicate service UUIDs.
                ..valid_interface.clone()
            });
        applied.interfaces.push(ServiceInterfaceStatusObservation {
            internal_uuid: second_identity,
            ..applied.interfaces[0].clone()
        });

        // Each row protects a distinct presence or validation contract at the wire conversion.
        let cases = [
            // Legacy reporters supply no evidence of successful service reconciliation.
            ("legacy omission", None, None),
            // A complete empty report acknowledges the absence of all service interfaces.
            (
                "present empty",
                Some(rpc::ServiceNetworkStatusObservation::default()),
                Some(ServiceNetworkStatusObservation { interfaces: vec![] }),
            ),
            // An attached NSG retains its selected-VPC source, ID, and version for both
            // distinct interfaces.
            ("attached NSG", Some(valid.clone()), Some(applied)),
            // No attached NSG still reports the service interface's successful reconciliation.
            ("absent NSG", Some(no_nsg), Some(no_nsg_applied)),
            // A missing correlation identity cannot acknowledge a persisted service interface.
            (
                "missing identity",
                Some(rpc::ServiceNetworkStatusObservation {
                    interfaces: vec![rpc::ServiceInterfaceStatusObservation {
                        internal_uuid: None, // Isolate missing-UUID handling from NSG validation.
                        ..valid_interface.clone()
                    }],
                }),
                None,
            ),
            // An invalid UUID must never be substituted for a different interface.
            (
                "malformed identity",
                Some(rpc::ServiceNetworkStatusObservation {
                    interfaces: vec![rpc::ServiceInterfaceStatusObservation {
                        internal_uuid: Some(crate::common::Uuid {
                            value: "invalid".to_string(), // Fail only UUID parsing.
                        }), // Keep UUID parsing reachable.
                        ..valid_interface.clone()
                    }],
                }),
                None,
            ),
            // Repeating one identity cannot count as two successfully applied interfaces.
            (
                "duplicate identity",
                Some(rpc::ServiceNetworkStatusObservation {
                    interfaces: vec![valid_interface.clone(), valid_interface.clone()], // Exercise duplicate rejection with valid NSGs.
                }),
                None,
            ),
            // A malformed NSG on a distinct interface must also discard the preceding valid entry.
            (
                "valid entry followed by malformed NSG",
                Some(rpc::ServiceNetworkStatusObservation {
                    interfaces: vec![
                        valid_interface.clone(),
                        rpc::ServiceInterfaceStatusObservation {
                            internal_uuid: Some(second_identity.into()), // Keep duplicate rejection from masking the NSG error.
                            network_security_group: Some(rpc::NetworkSecurityGroupStatus {
                                source: rpc::NetworkSecurityGroupSource::NsgSourceVpc.into(),
                                id: "service-vpc-nsg".to_string(),
                                version: "invalid".to_string(), // Fail only this interface's NSG metadata.
                            }),
                        },
                    ],
                }),
                None,
            ),
            // Reject the default source rather than guessing where an NSG came from.
            (
                "unspecified NSG source",
                invalid_nsg(0, "service-vpc-nsg", "V1-T1"),
                None,
            ),
            // Hidden services must never acknowledge an instance-level NSG override.
            (
                "instance NSG",
                invalid_nsg(
                    rpc::NetworkSecurityGroupSource::NsgSourceInstance.into(),
                    "service-vpc-nsg",
                    "V1-T1",
                ),
                None,
            ),
            // A VPC source without an NSG identity supplies no applied policy evidence.
            (
                "empty NSG ID",
                invalid_nsg(
                    rpc::NetworkSecurityGroupSource::NsgSourceVpc.into(),
                    "",
                    "V1-T1",
                ),
                None,
            ),
            // An unparsable policy version cannot acknowledge the desired NSG revision.
            (
                "invalid NSG version",
                invalid_nsg(
                    rpc::NetworkSecurityGroupSource::NsgSourceVpc.into(),
                    "service-vpc-nsg",
                    "invalid",
                ),
                None,
            ),
        ];
        carbide_test_support::check_values(
            cases
                .into_iter()
                .map(|(scenario, input, expect)| carbide_test_support::Check {
                    scenario,
                    input,
                    expect,
                }),
            |report| {
                // Convert the report alongside one host interface and valid acknowledgement versions.
                let observed = MachineNetworkStatusObservation::try_from(rpc::DpuNetworkStatus {
                    dpu_machine_id: Some(
                        "fm100dskla0ihp0pn4tv7v1js2k2mo37sl0jjr8141okqg8pjpdpfihaa80"
                            .parse()
                            .expect("DPU ID"),
                    ),
                    network_config_version: Some("V2-T2".to_string()), // Detect host ACK loss on service rejection.
                    instance_network_config_version: Some("V3-T3".to_string()), // Keep instance ACK processing observable.
                    instance_config_version: Some("V4-T4".to_string()), // Detect instance ACK loss on service rejection.
                    interfaces: vec![rpc::InstanceInterfaceStatusObservation::default()], // Detect instance interface loss on service rejection.
                    service_network_observation: report, // Vary service validity without changing valid ACKs.
                    ..Default::default()
                })
                .expect("convert machine status");

                // Invalid service metadata must not discard existing host or instance acknowledgements.
                assert_eq!(
                    observed.network_config_version,
                    Some("V2-T2".parse().expect("host version"))
                );
                let instance = observed
                    .instance_network_observation
                    .as_ref()
                    .expect("instance acknowledgement");
                assert_eq!(
                    instance.config_version,
                    "V3-T3".parse().expect("instance network version")
                );
                assert_eq!(
                    instance.instance_config_version,
                    Some("V4-T4".parse().expect("instance version"))
                );
                assert_eq!(instance.interfaces.len(), 1);

                // Exercise the JSON representation used by Core.
                let persisted: MachineNetworkStatusObservation = serde_json::from_value(
                    serde_json::to_value(&observed).expect("serialize machine observation"),
                )
                .expect("read persisted observation");
                assert_eq!(persisted, observed);
                observed.service_network_observation
            },
        );
    }

    /// Omits populated service observations from aggregate DPU status so service-interface counts
    /// cannot inflate the response while machine identity and network acknowledgements remain visible.
    #[test]
    fn aggregate_network_status_omits_service_observations() {
        // Populate stored service evidence; the other fields supply machine identity and host version.
        let machine_id = "fm100dskla0ihp0pn4tv7v1js2k2mo37sl0jjr8141okqg8pjpdpfihaa80"
            .parse()
            .expect("DPU ID");
        let observed = MachineNetworkStatusObservation {
            machine_id, // Identify the DPU for the summary assertion.
            agent_version: None,
            observed_at: DateTime::UNIX_EPOCH,
            network_config_version: Some("V2-T2".parse().expect("host version")), // Supply a populated host ACK for the summary assertion.
            client_certificate_expiry: None,
            agent_version_superseded_at: None,
            instance_network_observation: None,
            service_network_observation: Some(ServiceNetworkStatusObservation {
                interfaces: vec![ServiceInterfaceStatusObservation {
                    internal_uuid: uuid::uuid!("40000000-0000-0000-0000-000000000001"), // Identify the stored service interface.
                    network_security_group: None,
                }],
            }), // Seed persisted service evidence to make omission observable.
            fabric_interfaces: vec![],
            accepted_lldp_snapshot_fingerprint: None,
        };

        // Convert the stored observation through the aggregate status conversion.
        let summary: rpc::DpuNetworkStatus = observed.into();

        // Service details must be absent while existing machine identity and host ACK remain.
        assert!(summary.service_network_observation.is_none());
        assert_eq!(summary.dpu_machine_id, Some(machine_id));
        assert_eq!(summary.network_config_version.as_deref(), Some("V2-T2"));
    }

    #[test]
    fn malformed_machine_network_version_is_absent() {
        Case {
            scenario: "a malformed machine network version is treated as unobserved",
            input: rpc::DpuNetworkStatus {
                dpu_machine_id: Some(
                    "fm100dskla0ihp0pn4tv7v1js2k2mo37sl0jjr8141okqg8pjpdpfihaa80"
                        .parse()
                        .expect("valid fixture DPU ID"),
                ),
                observed_at: Some(std::time::UNIX_EPOCH.into()),
                network_config_version: Some("invalid-version".to_string()),
                ..Default::default()
            },
            expect: Yields(None),
        }
        .check(|status| {
            MachineNetworkStatusObservation::try_from(status)
                .map(|observation| observation.network_config_version)
                .map_err(drop)
        });
    }
}
