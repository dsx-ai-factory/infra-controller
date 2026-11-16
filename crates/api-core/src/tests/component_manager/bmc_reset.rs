// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use ::rpc::forge::forge_server::Forge;
use ::rpc::forge::{self as rpc};
use carbide_redfish::libredfish::test_support::RedfishSimAction;
use carbide_uuid::machine::{HostMachineId, MachineId, MachineIdSource, MachineType};
use model::machine::{CURRENT_STATE_MODEL_VERSION, ManagedHostState};
use model::machine_interface_address::MachineInterfaceAssociation;

use super::*;

#[crate::sqlx_test]
async fn component_bmc_reset_routes_all_selectors_to_core(pool: sqlx::PgPool) {
    use rpc::component_reset_request::Target;
    let env = create_test_env(pool.clone()).await;
    let mut txn = pool.begin().await.unwrap();
    let machine_id = MachineId::new(
        MachineIdSource::ProductBoardChassisSerial,
        [95; 32],
        MachineType::Host,
    );
    let host_id = HostMachineId::try_from(machine_id).unwrap();
    db::machine::create(
        &mut txn,
        None,
        &machine_id,
        ManagedHostState::Ready,
        None,
        CURRENT_STATE_MODEL_VERSION,
    )
    .await
    .unwrap();
    let switch = db::test_support::switch::create_seeded_discovered(&mut txn, 94, "bmc-reset")
        .await
        .unwrap();
    let pre_switch =
        db::test_support::switch::create_seeded_discovered(&mut txn, 97, "pre-bmc-reset")
            .await
            .unwrap();
    let pre_switch_mac = pre_switch.bmc_mac_address.unwrap();
    sqlx::query("DELETE FROM switches WHERE id = $1")
        .bind(pre_switch.id)
        .execute(&mut *txn)
        .await
        .unwrap();
    // Before ingestion, both credential sets come from expected-switch inventory.
    sqlx::query("UPDATE expected_switches SET nvos_username = $1, nvos_password = $2 WHERE bmc_mac_address = $3")
        .bind("expected-nvos-user")
        .bind("test-password")
        .bind(pre_switch_mac)
        .execute(&mut *txn)
        .await
        .unwrap();
    let shelf = db::test_support::power_shelf::create_seeded(&mut txn, 96, "pmc-reset")
        .await
        .unwrap();
    let compute_mac: MacAddress = "02:00:00:00:00:95".parse().unwrap();
    let shelf_mac: MacAddress = "02:00:00:00:00:96".parse().unwrap();
    let pre_ingestion_mac: MacAddress = "02:00:00:00:00:99".parse().unwrap();
    db::expected_power_shelf::create(
        &mut txn,
        model::expected_power_shelf::ExpectedPowerShelf {
            bmc_mac_address: shelf_mac,
            serial_number: "pmc-reset".into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    sqlx::query("UPDATE power_shelves SET bmc_mac_address = $1 WHERE id = $2")
        .bind(shelf_mac)
        .bind(shelf.id)
        .execute(&mut *txn)
        .await
        .unwrap();
    let underlay = db::network_segment::find_by_name(&mut txn, "UNDERLAY")
        .await
        .unwrap();
    for (mac, association) in [
        (
            compute_mac,
            Some(MachineInterfaceAssociation::Machine(machine_id)),
        ),
        (
            shelf_mac,
            Some(MachineInterfaceAssociation::PowerShelf(shelf.id)),
        ),
        (pre_ingestion_mac, None),
    ] {
        let interface = db::machine_interface::create(
            &mut txn,
            std::slice::from_ref(&underlay),
            &mac,
            false,
            model::address_selection_strategy::AddressSelectionStrategy::NextAvailableIp,
            None,
        )
        .await
        .unwrap();
        if let Some(association) = association {
            db::machine_interface::associate_bmc_interface(&interface.id, association, &mut txn)
                .await
                .unwrap();
        }
    }
    txn.commit().await.unwrap();
    let switch_mac = switch.bmc_mac_address.unwrap();
    for mac in [compute_mac, switch_mac, shelf_mac, pre_ingestion_mac] {
        env.api
            .credential_manager
            .set_credentials(
                &CredentialKey::BmcCredentials {
                    credential_type: BmcCredentialType::BmcRoot {
                        bmc_mac_address: mac,
                    },
                },
                &Credentials::new("bmc-reset", "test-password"),
            )
            .await
            .unwrap();
    }
    env.api
        .credential_manager
        .set_credentials(
            &CredentialKey::SwitchNvosAdmin {
                bmc_mac_address: switch_mac,
            },
            &Credentials::new("nvos-user", "nvos-password"),
        )
        .await
        .unwrap();
    // Every selected backend is RMS, but BMC resets must use the real Core path.
    let rms = Arc::new(api_test_helper::mock_rms::MockRmsApi::new());
    let cm = component_manager::component_manager::build_component_manager(
        &component_manager::config::ComponentManagerConfig::default(),
        crate::tests::common::api_fixtures::get_config_with_rack_profiles().rack_profiles,
        Some(rms.clone()),
        None,
        Some(pool.clone()),
        Some(env.redfish_sim.clone()),
        None,
        carbide_instrument::Instrumentation::global(),
    )
    .await
    .unwrap();
    let api = TestApiBuilder::new(
        pool.clone(),
        env.common_pools.clone(),
        env.api.work_lock_manager_handle.clone(),
    )
    .with_component_manager(Arc::new(cm))
    .with_credential_manager(env.api.credential_manager.clone())
    .build();
    let mac_targets = rpc::MacAddressList {
        mac_addresses: vec![
            "invalid".into(),
            "02:00:00:00:ff:ff".into(),
            pre_ingestion_mac.to_string(),
        ],
    };
    let mut switch_mac_targets = mac_targets.clone();
    switch_mac_targets.mac_addresses[2] = pre_switch_mac.to_string();
    for (target, mac, id) in [
        (
            Target::MachineIds(::rpc::common::HostMachineIdList {
                machine_ids: vec![host_id],
            }),
            compute_mac,
            Some(host_id.to_string()),
        ),
        (
            Target::SwitchIds(rpc::SwitchIdList {
                ids: vec![switch.id],
            }),
            switch_mac,
            Some(switch.id.to_string()),
        ),
        (
            Target::PowerShelfIds(rpc::PowerShelfIdList {
                ids: vec![shelf.id],
            }),
            shelf_mac,
            Some(shelf.id.to_string()),
        ),
        (
            Target::ComputeBmcMacs(mac_targets.clone()),
            pre_ingestion_mac,
            None,
        ),
        (
            Target::SwitchBmcMacs(switch_mac_targets),
            pre_switch_mac,
            None,
        ),
        (
            Target::SwitchBmcMacs(rpc::MacAddressList {
                mac_addresses: vec![switch_mac.to_string()],
            }),
            switch_mac,
            Some(switch.id.to_string()),
        ),
        (
            Target::PowerShelfPmcMacs(mac_targets),
            pre_ingestion_mac,
            None,
        ),
    ] {
        let before = env.redfish_sim.timepoint();
        let ip = db::machine_interface::lookup_bmc_ip_by_mac_address(&pool, mac)
            .await
            .unwrap()[0];
        let response = api
            .component_reset(Request::new(rpc::ComponentResetRequest {
                target: Some(target),
                action: rpc::ComponentResetAction::BmcReset as i32,
                domain_name: None,
            }))
            .await
            .unwrap()
            .into_inner();
        if id.is_none() {
            assert_eq!(response.results.len(), 3);
            assert_eq!(
                response.results[0].status,
                rpc::ComponentManagerStatusCode::InvalidArgument as i32
            );
            assert_eq!(response.results[0].mac_address.as_deref(), Some("invalid"));
            assert_eq!(
                response.results[1].status,
                rpc::ComponentManagerStatusCode::NotFound as i32
            );
        } else {
            assert_eq!(response.results.len(), 1);
        }
        let result = response.results.last().unwrap();
        assert_eq!(
            result.status,
            rpc::ComponentManagerStatusCode::Success as i32,
            "{result:?}"
        );
        assert_eq!(
            result.mac_address.as_deref(),
            Some(mac.to_string().as_str())
        );
        assert_eq!(result.component_id, id);
        assert_eq!(
            env.redfish_sim
                .actions_since(&before)
                .for_host(&ip.to_string()),
            vec![RedfishSimAction::BmcReset(None)]
        );
    }
    // Ingested switches still require NVOS credentials for BMC reset.
    env.api
        .credential_manager
        .delete_credentials(&CredentialKey::SwitchNvosAdmin {
            bmc_mac_address: switch_mac,
        })
        .await
        .unwrap();
    let before = env.redfish_sim.timepoint();
    let missing_nvos = api
        .component_reset(Request::new(rpc::ComponentResetRequest {
            target: Some(Target::SwitchIds(rpc::SwitchIdList {
                ids: vec![switch.id],
            })),
            action: rpc::ComponentResetAction::BmcReset as i32,
            domain_name: None,
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(
        missing_nvos.results[0].status,
        rpc::ComponentManagerStatusCode::NotFound as i32
    );
    assert!(
        env.redfish_sim
            .actions_since(&before)
            .all_hosts()
            .is_empty()
    );
    assert!(rms.batch_set_power_state_calls().await.is_empty());
    assert!(
        rms.batch_reset_switch_factory_default_calls()
            .await
            .is_empty()
    );
    assert!(
        rms.batch_reset_switch_sdn_factory_default_calls()
            .await
            .is_empty()
    );
    let machine = db::machine::find_one(&pool, &host_id, Default::default())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(machine.state.value, ManagedHostState::Ready);
    assert!(machine.machine_maintenance_requested.is_none());
    use rpc::get_component_reset_status_request::Target as StatusTarget;
    let macs = rpc::MacAddressList {
        mac_addresses: vec!["invalid".into(), "02:00:00:00:FF:FF".into()],
    };
    let before = env.redfish_sim.timepoint();
    for (target, identity, by_mac) in [
        (
            StatusTarget::MachineIds(::rpc::common::HostMachineIdList {
                machine_ids: vec![host_id],
            }),
            host_id.to_string(),
            false,
        ),
        (
            StatusTarget::SwitchIds(rpc::SwitchIdList {
                ids: vec![switch.id],
            }),
            switch.id.to_string(),
            false,
        ),
        (
            StatusTarget::PowerShelfIds(rpc::PowerShelfIdList {
                ids: vec![shelf.id],
            }),
            shelf.id.to_string(),
            false,
        ),
        (
            StatusTarget::ComputeBmcMacs(macs.clone()),
            macs.mac_addresses[1].clone(),
            true,
        ),
        (
            StatusTarget::SwitchBmcMacs(macs.clone()),
            macs.mac_addresses[1].clone(),
            true,
        ),
        (
            StatusTarget::PowerShelfPmcMacs(macs.clone()),
            macs.mac_addresses[1].clone(),
            true,
        ),
    ] {
        let response = api
            .get_component_reset_status(Request::new(rpc::GetComponentResetStatusRequest {
                target: Some(target),
                action: rpc::ComponentResetAction::BmcReset as i32,
            }))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(response.statuses.len(), if by_mac { 2 } else { 1 });
        if by_mac {
            assert_eq!(
                response.statuses[0].result.as_ref().unwrap().status,
                rpc::ComponentManagerStatusCode::InvalidArgument as i32
            );
        }
        let status = response.statuses.last().unwrap();
        assert_eq!(status.action, rpc::ComponentResetAction::BmcReset as i32);
        assert_eq!(status.state, rpc::ComponentResetState::Completed as i32);
        let result = status.result.as_ref().unwrap();
        assert_eq!(
            result.status,
            rpc::ComponentManagerStatusCode::Success as i32
        );
        assert_eq!(
            if by_mac {
                result.mac_address.as_ref()
            } else {
                result.component_id.as_ref()
            },
            Some(&identity)
        );
    }
    assert!(
        env.redfish_sim
            .actions_since(&before)
            .all_hosts()
            .is_empty()
    );
}
