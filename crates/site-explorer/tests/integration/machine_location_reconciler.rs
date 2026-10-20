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

use std::sync::Arc;
use std::time::Duration;

use carbide_site_explorer::test_support::{MockEndpointExplorer, TestSiteExplorer};
use carbide_site_explorer::{EndpointExplorationService, MachineLocationReconciler, SiteExplorer};
use carbide_test_harness::prelude::*;
use carbide_test_harness::test_support::fixture_config::{
    FixtureDefault as _, ManagedHostConfigExt as _,
};
use carbide_uuid::machine::MachineId;
use carbide_uuid::rack::{RackId, RackProfileId};
use component_manager::{MachineLocationObservation, TestMachineInfoProvider};
use db::ObjectFilter;
use model::expected_machine::{ExpectedMachineData, HostDpuPolicy};
use model::expected_rack::ExpectedRack;
use model::machine::machine_search_config::MachineSearchConfig;
use model::test_support::ManagedHostConfig;

use super::machine_fixtures::{
    Env, TEST_MACHINE_INFO_RACK_PROFILE_ID, expected_machine, explored_host_fixture,
    machine_creator_config, machine_creator_with_info_provider, machine_info_rack_profiles,
};

#[sqlx_test]
async fn test_machine_location_reconciler_preserves_existing_values(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = Env::new(pool).await;
    let provider = Arc::new(TestMachineInfoProvider::default());
    let creator = machine_creator_with_info_provider(&env, provider.clone());
    let reconciler = MachineLocationReconciler::new(
        env.pool.clone(),
        Arc::new(machine_info_rack_profiles()),
        Some(provider.clone()),
        env.api().credential_manager().clone(),
    );
    let rack_id = RackId::new(uuid::Uuid::new_v4().to_string());
    let expected_rack = ExpectedRack {
        rack_group_id: None,
        rack_id: rack_id.clone(),
        rack_profile_id: RackProfileId::new(TEST_MACHINE_INFO_RACK_PROFILE_ID),
        metadata: Default::default(),
    };

    let mut txn = env.pool.begin().await?;
    db::expected_rack::create(txn.as_mut(), &expected_rack).await?;
    txn.commit().await?;
    let managed_host =
        ManagedHostConfig::default().with_expected_machine_data(ExpectedMachineData {
            rack_id: Some(rack_id.clone()),
            ..Default::default()
        });
    let mut fixture = explored_host_fixture(&env, &managed_host).await;

    assert!(
        creator
            .create_managed_host(
                &fixture.host,
                &mut fixture.host_report,
                Some(&expected_machine(&managed_host)),
                &env.pool,
            )
            .await?
    );

    assert_eq!(provider.call_count(), 0);

    let machines = db::machine::find(
        &env.pool,
        ObjectFilter::<MachineId>::All,
        MachineSearchConfig {
            include_predicted_host: true,
            ..Default::default()
        },
    )
    .await?;
    let host = machines
        .iter()
        .find(|machine| !machine.is_dpu())
        .ok_or_else(|| std::io::Error::other("created host machine was not found"))?;

    let mut txn = env.pool.begin().await?;
    db::machine::update_slot_and_tray(txn.as_mut(), &host.id, Some(7), None).await?;
    txn.commit().await?;

    provider
        .enqueue_locations(vec![MachineLocationObservation {
            node_id: host.id.to_string(),
            slot_number: None,
            tray_index: Some(3),
        }])
        .await;

    reconciler
        .reconcile_machine_locations(&[fixture.host.host_bmc_ip])
        .await?;

    assert_eq!(provider.call_count(), 1);

    let machines = db::machine::find(
        &env.pool,
        ObjectFilter::<MachineId>::All,
        MachineSearchConfig {
            include_predicted_host: true,
            ..Default::default()
        },
    )
    .await?;
    let host = machines
        .iter()
        .find(|machine| !machine.is_dpu())
        .ok_or_else(|| std::io::Error::other("created host machine was not found"))?;
    assert_eq!(host.status.slot_number, Some(7));
    assert_eq!(host.status.tray_index, Some(3));

    Ok(())
}

#[sqlx_test]
async fn test_machine_location_reconciler_retries_after_failure(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = Env::new(pool).await;
    let provider = Arc::new(TestMachineInfoProvider::default());
    let creator = machine_creator_with_info_provider(&env, provider.clone());
    let reconciler = MachineLocationReconciler::new(
        env.pool.clone(),
        Arc::new(machine_info_rack_profiles()),
        Some(provider.clone()),
        env.api().credential_manager().clone(),
    );
    let rack_id = RackId::new(uuid::Uuid::new_v4().to_string());
    let expected_rack = ExpectedRack {
        rack_group_id: None,
        rack_id: rack_id.clone(),
        rack_profile_id: RackProfileId::new(TEST_MACHINE_INFO_RACK_PROFILE_ID),
        metadata: Default::default(),
    };

    let mut txn = env.pool.begin().await?;
    db::expected_rack::create(txn.as_mut(), &expected_rack).await?;
    txn.commit().await?;

    provider.enqueue_error("provider unavailable").await;

    let managed_host =
        ManagedHostConfig::default().with_expected_machine_data(ExpectedMachineData {
            rack_id: Some(rack_id),
            ..Default::default()
        });
    let mut fixture = explored_host_fixture(&env, &managed_host).await;
    let expected_machine = expected_machine(&managed_host);

    assert!(
        creator
            .create_managed_host(
                &fixture.host,
                &mut fixture.host_report,
                Some(&expected_machine),
                &env.pool,
            )
            .await?
    );

    assert_eq!(provider.call_count(), 0);

    reconciler
        .reconcile_machine_locations(&[fixture.host.host_bmc_ip])
        .await?;

    assert_eq!(provider.call_count(), 1);

    let machines = db::machine::find(
        &env.pool,
        ObjectFilter::<MachineId>::All,
        MachineSearchConfig {
            include_predicted_host: true,
            ..Default::default()
        },
    )
    .await?;
    let host = machines
        .iter()
        .find(|machine| !machine.is_dpu())
        .ok_or_else(|| std::io::Error::other("created host machine was not found"))?;
    assert_eq!(host.status.slot_number, None);
    assert_eq!(host.status.tray_index, None);

    provider
        .enqueue_locations(vec![MachineLocationObservation {
            node_id: host.id.to_string(),
            slot_number: Some(9),
            tray_index: Some(4),
        }])
        .await;

    reconciler
        .reconcile_machine_locations(&[fixture.host.host_bmc_ip])
        .await?;

    assert_eq!(provider.call_count(), 2);

    let machines = db::machine::find(
        &env.pool,
        ObjectFilter::<MachineId>::All,
        MachineSearchConfig {
            include_predicted_host: true,
            ..Default::default()
        },
    )
    .await?;
    let host = machines
        .iter()
        .find(|machine| !machine.is_dpu())
        .ok_or_else(|| std::io::Error::other("created host machine was not found"))?;
    assert_eq!(host.status.slot_number, Some(9));
    assert_eq!(host.status.tray_index, Some(4));

    Ok(())
}

#[sqlx_test]
async fn test_site_explorer_retries_machine_location_after_request_deadline(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = Env::new(pool).await;
    let provider = Arc::new(TestMachineInfoProvider::default());

    provider.set_delay(Duration::from_secs(60)).await;

    let rack_id = RackId::new(uuid::Uuid::new_v4().to_string());
    let managed_host = ManagedHostConfig {
        dpus: vec![],
        ..ManagedHostConfig::default()
    }
    .with_expected_machine_data(ExpectedMachineData {
        rack_id: Some(rack_id.clone()),
        dpu_policy: HostDpuPolicy::Ignore,
        ..Default::default()
    });
    let fixture = explored_host_fixture(&env, &managed_host).await;
    let host_bmc_ip = fixture.host.host_bmc_ip;

    let mut txn = env.pool.begin().await?;
    db::expected_rack::create(
        txn.as_mut(),
        &ExpectedRack {
            rack_id,
            rack_group_id: None,
            rack_profile_id: RackProfileId::new(TEST_MACHINE_INFO_RACK_PROFILE_ID),
            metadata: Default::default(),
        },
    )
    .await?;
    db::expected_machine::create(txn.as_mut(), expected_machine(&managed_host)).await?;
    db::explored_endpoints::insert(host_bmc_ip, &fixture.host_report, false, txn.as_mut()).await?;
    db::explored_endpoints::set_preingestion_complete(host_bmc_ip, txn.as_mut()).await?;
    txn.commit().await?;

    let endpoint_explorer = Arc::new(MockEndpointExplorer::default());
    let explorer = TestSiteExplorer::new(
        SiteExplorer::new(
            env.pool.clone(),
            machine_creator_config(),
            env.test_harness.test_meter.meter(),
            Arc::new(EndpointExplorationService::new(
                env.pool.clone(),
                endpoint_explorer.clone(),
                Arc::new(env.api().runtime_config.get_firmware_config()),
            )),
            endpoint_explorer.clone(),
            env.api().common_pools().clone(),
            env.api().work_lock_manager_handle(),
            machine_info_rack_profiles(),
            Some(provider.clone()),
            env.api().credential_manager().clone(),
            false,
        ),
        endpoint_explorer,
    );
    explorer.insert_endpoints(vec![(host_bmc_ip, fixture.host_report)]);

    tokio::time::timeout(Duration::from_secs(30), explorer.run_single_iteration())
        .await
        .expect(
            "a stalled machine-information request must not hold the Site Explorer iteration open",
        )?;

    assert_eq!(provider.call_count(), 1);
    let identities = db::machine::find_rms_identities_by_bmc_ips(&env.pool, &[host_bmc_ip]).await?;
    assert_eq!(
        identities.len(),
        1,
        "the host must be committed before machine-information lookup times out"
    );
    let host = &identities[0];
    assert_eq!((host.slot_number, host.tray_index), (None, None));

    provider.set_delay(Duration::ZERO).await;
    provider
        .enqueue_locations(vec![MachineLocationObservation {
            node_id: host.id.clone(),
            slot_number: Some(9),
            tray_index: Some(4),
        }])
        .await;

    // A subsequent production iteration must reacquire the work lock and retry
    // the existing host, without going through machine creation again.
    tokio::time::timeout(Duration::from_secs(30), explorer.run_single_iteration())
        .await
        .expect("Site Explorer must resume after a machine-information deadline")?;

    assert_eq!(provider.call_count(), 2);
    let identities = db::machine::find_rms_identities_by_bmc_ips(&env.pool, &[host_bmc_ip]).await?;
    assert_eq!(identities.len(), 1);
    assert_eq!(identities[0].id, host.id);
    assert_eq!(
        (identities[0].slot_number, identities[0].tray_index),
        (Some(9), Some(4))
    );
    Ok(())
}
