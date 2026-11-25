/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use carbide_test_support::Outcome::*;
use carbide_test_support::scenarios;

use super::*;
use crate::rack::{ConfigureNmxClusterState, MaintenanceActivity, RackPowerState};

#[test]
fn member_type_preserves_storage_and_wire_values() {
    scenarios!(
        run = |name: &str| -> Result<NvLinkDomainMemberType, ()> {
            let parsed = name.parse::<NvLinkDomainMemberType>().map_err(drop)?;
            let stored = serde_json::to_value(&parsed).unwrap();
            assert_eq!(stored, serde_json::Value::String(name.into()));
            assert_eq!(serde_json::from_value::<NvLinkDomainMemberType>(stored).unwrap(), parsed);
            assert_eq!(parsed.to_string(), name);
            Ok(parsed)
        };
        "compute" { "Compute" => Yields(NvLinkDomainMemberType::Compute) }
        "switch" { "Switch" => Yields(NvLinkDomainMemberType::Switch) }
        "power shelf is not a domain member" { "PowerShelf" => Fails }
        "names are case sensitive" { "switch" => Fails }
    );
}

#[test]
fn persisted_domain_states_remain_readable() {
    scenarios!(
        run = |json| serde_json::from_str::<NvLinkDomainState>(json).map_err(drop);
        "fabric job survives restart" {
            r#"{"state":"maintenance","maintenance_state":{"ConfigureNmxCluster":{"configure_nmx_cluster":{"WaitForScaleUpFabricManagerJob":{"job_id":"fabric-job-1"}}}}}"# =>
                Yields(NvLinkDomainState::Maintenance {
                    maintenance_state: RackMaintenanceState::ConfigureNmxCluster {
                        configure_nmx_cluster: ConfigureNmxClusterState::WaitForScaleUpFabricManagerJob {
                            job_id: "fabric-job-1".into(),
                        },
                    },
                }),
        }
        "power reset without a submitted batch" {
            r#"{"state":"maintenance","maintenance_state":{"PowerSequence":{"rack_power":"PowerReset"}}}"# =>
                Yields(NvLinkDomainState::Maintenance {
                    maintenance_state: RackMaintenanceState::PowerSequence {
                        rack_power: RackPowerState::PowerReset,
                        restart_machine_ids: None,
                    },
                }),
        }
        "validation run survives restart" {
            r#"{"state":"validating","validating_state":{"InProgress":{"run_id":"rvs-run-1"}}}"# =>
                Yields(NvLinkDomainState::Validating {
                    validating_state: RackValidationState::InProgress { run_id: "rvs-run-1".into() },
                }),
        }
    );
}

#[test]
fn persisted_domain_config_remains_readable() {
    scenarios!(
        run = |json| serde_json::from_str::<NvLinkDomainConfig>(json).map_err(drop);
        "omitted lifecycle requests" {
            "{}" => Yields(NvLinkDomainConfig::default()),
        }
        "scope preserves selection while omitted fields keep their defaults" {
            r#"{"maintenance_requested":{"machine_ids":["fm100htes3rn1npvbtm5qd57dkilaag7ljugl1llmm7rfuq1ov50i0rpl30"],"activities":["ConfigureNmxCluster"]}}"# =>
                Yields(NvLinkDomainConfig {
                    maintenance_requested: Some(MaintenanceScope {
                        machine_ids: vec!["fm100htes3rn1npvbtm5qd57dkilaag7ljugl1llmm7rfuq1ov50i0rpl30".parse().unwrap()],
                        activities: vec![MaintenanceActivity::ConfigureNmxCluster],
                        ..Default::default()
                    }),
                    maintenance_termination_requested: false,
                }),
        }
    );
}
