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
                    ..Default::default()
                }),
        }
    );
}

#[test]
fn managed_members_validate_positions_per_type() {
    use crate::expected_rack_group::{ExpectedRackGroupMember, ExpectedRackGroupRack};
    for (second_type, second_index, error) in [
        (
            RackCapabilityType::Compute,
            0,
            Some("multiple Compute members"),
        ),
        (
            RackCapabilityType::Switch,
            i32::MAX as u32,
            Some("exceeds slot range"),
        ),
        (RackCapabilityType::Switch, 0, None),
    ] {
        let first = ExpectedRackGroupMember {
            cartridge_index: Some(0),
            device_type: RackCapabilityType::Compute,
            manufacturer: "NVIDIA".into(),
            id: "c1".into(),
        };
        let second = ExpectedRackGroupMember {
            cartridge_index: Some(second_index),
            device_type: second_type,
            id: "tray2".into(),
            ..first.clone()
        };
        let group = ExpectedRackGroup {
            racks: vec![ExpectedRackGroupRack {
                rack_id: "rack-a".parse().unwrap(),
                members: vec![first, second],
            }],
            ..Default::default()
        };
        let result = derive_managed_members(&group);
        match error {
            Some(reason) => assert!(result.unwrap_err().contains(reason)),
            None => assert_eq!(result.unwrap().len(), 2),
        }
    }
}

#[test]
fn compute_anchors_slots_in_physical_order() {
    let mut group: ExpectedRackGroup = serde_json::from_value(serde_json::json!({
        "rack_group_id":"group","topology":"topology","racks":[{"rack_id":"rack","members":[
            {"type":"PowerShelf","cartridge_index":4,"manufacturer":"v","id":"p"},
            {"type":"Compute","cartridge_index":13,"manufacturer":"v","id":"c2"},
            {"type":"Switch","cartridge_index":12,"manufacturer":"v","id":"s1"},
            {"type":"Compute","cartridge_index":11,"manufacturer":"v","id":"c1"}
        ]}]
    }))
    .unwrap();
    let positions = derive_managed_members(&group).unwrap();
    let slots: Vec<_> = positions
        .into_iter()
        .map(|p| (p.component_type, p.slot_index))
        .collect();
    use NvLinkDomainMemberType::{Compute, Switch};
    assert_eq!(slots, [(Compute, 3), (Switch, 2), (Compute, 1)]);
    let members = &mut group.racks[0].members;
    members[2].cartridge_index = Some(9);
    assert!(
        derive_managed_members(&group)
            .unwrap_err()
            .contains("relative to base 11")
    );
    group.racks[0]
        .members
        .retain(|m| m.device_type != RackCapabilityType::Compute);
    assert!(
        derive_managed_members(&group)
            .unwrap_err()
            .contains("no compute cartridge")
    );
}
