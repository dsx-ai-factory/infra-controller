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

use carbide_test_harness::prelude::*;
use carbide_test_harness::test_support::fixture_config::{
    FixtureDefault as _, ManagedHostConfigExt as _,
};
use carbide_uuid::machine::{
    DpuMachineId, MachineId, MachineIdSource, MachineInterfaceId, MachineType, StableHostMachineId,
};
use model::expected_machine::{ExpectedMachineData, HostDpuPolicy};
use model::test_support::ManagedHostConfig;
use rpc::forge;
use rpc::forge::forge_server::Forge;

// On a zero-DPU host, set-primary-dpu has no DPU to resolve to an interface, so
// the alias rejects up-front with `FailedPrecondition` and a message that names
// the underlying reason -- rather than failing later, more confusingly, when the
// DPU-to-interface lookup comes up empty.
#[sqlx_test]
async fn test_set_primary_dpu_rejects_zero_dpu_host(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    // Zero-DPU host ingestion needs a HostInband network segment whose CIDR
    // covers the relay address; the default test env doesn't define one.
    let env = TestHarness::builder(pool)
        .with_resource_pools(
            ResourcePoolBuilder::default()
                .with_vlan_ids(1, 3)
                .with_vnis(10_001, 10_003)
                .build(),
        )
        .build()
        .await;
    let domain = env.test_domain().await;
    let network_controller = env.network_controller();
    let underlay_segment = network_controller.create_underlay_segment(&domain).await;
    // HostInband segments must live in a Flat VPC. The test doesn't otherwise
    // need a non-Flat VPC, so create only a Flat one for the segment.
    network_controller.create_admin_segment(&domain).await;
    let host_inband_segment = network_controller.create_host_inband_segment(&domain).await;

    let site_explorer = env.default_test_site_explorer();
    let config = ManagedHostConfig::zero_dpu();
    let expected_machine_data = ExpectedMachineData {
        serial_number: config.serial.clone(),
        dpu_policy: HostDpuPolicy::Ignore,
        ..Default::default()
    };
    let (mut zero_dpu_host, _) = env
        .managed_host_builder(&site_explorer, underlay_segment)
        .with_config(config.with_expected_machine_data(expected_machine_data))
        .build()
        .await;
    zero_dpu_host
        .host
        .discover_primary_iface(host_inband_segment)
        .await;

    let result = env
        .api()
        .set_primary_dpu(tonic::Request::new(forge::SetPrimaryDpuRequest {
            host_machine_id: Some(
                zero_dpu_host
                    .host
                    .id
                    .try_into()
                    .expect("zero-dpu host should have a stable ID after discovery"),
            ),
            // Any well-formed DPU id; the handler bails before reading it.
            dpu_machine_id: Some(
                MachineId::new(
                    MachineIdSource::ProductBoardChassisSerial,
                    [0u8; 32],
                    MachineType::Dpu,
                )
                .try_into()
                .unwrap(),
            ),
            force_reconcile: false,
            ..Default::default()
        }))
        .await;

    match result {
        Err(e) if e.code() == tonic::Code::FailedPrecondition => {
            assert!(
                e.message().contains("zero-DPU"),
                "error message should explicitly name zero-DPU as the reason; got: {}",
                e.message(),
            );
        }
        _ => panic!(
            "Expected zero-DPU host to reject set_primary_dpu with FailedPrecondition, got: {result:?}"
        ),
    };

    Ok(())
}

// `set_primary_dpu` resolves the requested DPU from the host's locked
// interface rows. A stale DPU id must fail before either the primary flag or
// desired target changes.
#[sqlx_test]
async fn test_set_primary_dpu_rejects_a_stale_host_relationship_without_writes(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = TestHarness::builder(pool).build().await;
    let domain = env.test_domain().await;
    let network_controller = env.network_controller();
    let underlay_segment = network_controller.create_underlay_segment(&domain).await;
    let admin_segment = network_controller.create_admin_segment(&domain).await;
    let site_explorer = env.default_test_site_explorer();
    let (mut host, _) = env
        .managed_host_builder(&site_explorer, underlay_segment)
        .with_config(ManagedHostConfig::default().with_dpu_count(2))
        .build()
        .await;
    host.host.discover_primary_iface(admin_segment).await;
    let host_id: StableHostMachineId = host
        .host
        .id
        .try_into()
        .expect("host should have a stable ID after discovery");

    let (original_primary_id, stale_interface_id, stale_dpu_id, surviving_dpu_id): (
        MachineInterfaceId,
        MachineInterfaceId,
        DpuMachineId,
        DpuMachineId,
    ) = {
        let mut txn = env.db_txn().await;
        let interfaces = db::machine_interface::find_by_machine_ids(txn.as_mut(), &[host_id])
            .await?
            .remove(&host_id)
            .expect("host should have interface rows");
        let original_primary = interfaces
            .iter()
            .find(|interface| interface.primary_interface)
            .expect("host should start with a primary interface");
        let stale_interface = interfaces
            .iter()
            .find(|interface| {
                !interface.primary_interface && interface.attached_dpu_machine_id.is_some()
            })
            .expect("host should have a non-primary DPU-backed interface");
        let surviving_dpu_id = original_primary
            .attached_dpu_machine_id
            .expect("the primary interface should be DPU-backed");
        let stale_dpu_id = stale_interface
            .attached_dpu_machine_id
            .expect("the non-primary interface should be DPU-backed");
        txn.commit().await?;
        (
            original_primary.id,
            stale_interface.id,
            stale_dpu_id,
            surviving_dpu_id,
        )
    };

    // Leave the stale DPU machine in place, but reassign its host interface to
    // the surviving DPU as a stale discovery/update could. This preserves the
    // host's DPU-backed Admin shape while ensuring the request is rejected
    // because no current interface names the stale DPU.
    sqlx::query("UPDATE machine_interfaces SET attached_dpu_machine_id = $1 WHERE id = $2")
        .bind(surviving_dpu_id)
        .bind(stale_interface_id)
        .execute(&env.api().database_connection)
        .await?;
    let desired_before =
        db::machine_desired_boot_interface::get(&env.api().database_connection, &host_id)
            .await?
            .expect("ingestion should initialize the desired target");
    sqlx::query("DELETE FROM machine_state_controller_queued_objects WHERE object_id = $1")
        .bind(host_id.to_string())
        .execute(&env.api().database_connection)
        .await?;

    let error = env
        .api()
        .set_primary_dpu(tonic::Request::new(forge::SetPrimaryDpuRequest {
            host_machine_id: Some(host_id),
            dpu_machine_id: Some(stale_dpu_id),
            force_reconcile: false,
            ..Default::default()
        }))
        .await
        .expect_err("a DPU without a current host interface must be rejected");
    assert_eq!(error.code(), tonic::Code::InvalidArgument);
    assert!(
        error.message().contains("has no interface on host"),
        "expected the stale-DPU lookup error, got: {}",
        error.message(),
    );

    let primary_ids = {
        let mut txn = env.db_txn().await;
        let primary_ids = db::machine_interface::find_by_machine_ids(txn.as_mut(), &[host_id])
            .await?
            .remove(&host_id)
            .expect("host should still have interface rows")
            .into_iter()
            .filter(|interface| interface.primary_interface)
            .map(|interface| interface.id)
            .collect::<Vec<_>>();
        txn.commit().await?;
        primary_ids
    };
    assert_eq!(primary_ids, vec![original_primary_id]);
    let desired_after =
        db::machine_desired_boot_interface::get(&env.api().database_connection, &host_id)
            .await?
            .expect("the original desired target should remain");
    assert_eq!(desired_after.value, desired_before.value);
    assert_eq!(desired_after.version, desired_before.version);

    let is_queued: bool = sqlx::query_scalar(
        "SELECT EXISTS (
            SELECT 1
            FROM machine_state_controller_queued_objects
            WHERE object_id = $1
        )",
    )
    .bind(host_id.to_string())
    .fetch_one(&env.api().database_connection)
    .await?;
    assert!(
        !is_queued,
        "the rejected request must not queue controller work"
    );

    Ok(())
}
