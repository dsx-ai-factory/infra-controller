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

use carbide_instrument::testing::MetricsCapture;
use rpc::forge::forge_server::Forge;

use super::*;

/// Rejects final BF4 target publication with a serialization failure and records attempts outside rollback so retries cannot pass without reaching the failed write.
async fn fail_registered_target_writes(pool: &PgPool) -> Result<(), Box<dyn Error>> {
    // Sequence increments survive transaction rollback and identify the injected failure's exact write.
    sqlx::raw_sql(
        "CREATE SEQUENCE bf4_metric_repair_attempts;
         CREATE FUNCTION reject_bf4_metric_target_write() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
             PERFORM nextval('bf4_metric_repair_attempts');
             RAISE EXCEPTION USING ERRCODE = '40001', MESSAGE = 'forced BF4 repair serialization failure';
         END $$;
         CREATE TRIGGER reject_bf4_metric_target_write
             BEFORE INSERT OR UPDATE ON machine_boot_interfaces
             FOR EACH ROW EXECUTE FUNCTION reject_bf4_metric_target_write();",
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Counts a failed BF4 repair after DiscoverMachine commits while preserving newly registered inventory, so post-commit failures remain visible without rejecting registration.
#[sqlx_test]
async fn discovery_counts_registered_repair_failure_after_inventory_commits(
    pool: PgPool,
) -> Result<(), Box<dyn Error>> {
    // Use one DPU so the first controller-2 registration supplies all evidence needed for repair.
    let fixture = Bf4HostFixture::new(pool, 1).await?;
    // Return the host to DPUInit and reject the separate repair's final desired-target write.
    fixture.initialize().await;
    fail_registered_target_writes(&fixture.pool).await?;
    let metrics = MetricsCapture::start();

    // Commit trusted inventory through the public request before the separate repair transaction fails.
    let request = &fixture.requests[0];
    fixture
        .harness
        .api()
        .discover_machine(Request::new(request.clone()))
        .await?;

    // Reload inventory through FindMachinesByIds to prove the failed repair did not reject registration.
    let persisted_inventory = fixture
        .harness
        .find_machine(fixture.host.dpus[0].id.into())
        .await
        .remove(0)
        .status
        .expect("registered DPU status")
        .discovery_info
        .expect("registered discovery information")
        .dpu_info
        .expect("registered DPU inventory");
    let Some(rpc::DiscoveryData::Info(reported_inventory)) = request.discovery_data.as_ref() else {
        panic!("fixture DPU discovery inventory")
    };
    assert_eq!(
        persisted_inventory.host_pfs,
        reported_inventory
            .dpu_info
            .as_ref()
            .expect("reported DPU inventory")
            .host_pfs
    );

    // Count the exact injected write once through the existing counter, which has no metric labels.
    assert!(
        sqlx::query_scalar::<_, bool>("SELECT is_called FROM bf4_metric_repair_attempts")
            .fetch_one(&fixture.pool)
            .await?
    );
    assert_eq!(
        metrics.counter_delta(
            "carbide_api_host_admin_address_reconcile_failures_total",
            &[]
        ),
        1.0
    );
    Ok(())
}

/// Counts each failed BF4 repair during exploration and scans of initializing hosts, then publishes the registered target on a later iteration, keeping retries observable without inflating successful attempts.
#[sqlx_test]
async fn site_explorer_counts_registered_repair_failures_until_retry_succeeds(
    pool: PgPool,
) -> Result<(), Box<dyn Error>> {
    // Register while Ready, then permit initialization with one DPU and the fixture's provisional target.
    let fixture = Bf4HostFixture::new(pool, 1).await?;
    fixture.register_inventory().await?;
    fixture.initialize().await;
    // Keep only the desired MAC so both repair scans retry the same incomplete target after failure.
    sqlx::query(
        "UPDATE machine_boot_interfaces SET desired_interface_id = NULL WHERE machine_id = $1",
    )
    .bind(fixture.host.host.id)
    .execute(&fixture.pool)
    .await?;
    // Confirm the persisted target is MAC-only so the incomplete-target scan has work to retry.
    let provisional_target = fixture
        .boot_interfaces()
        .await?
        .reconciliation
        .expect("provisional reconciliation")
        .desired_boot_interface
        .expect("MAC-only provisional target");
    assert!(provisional_target.interface_id.is_none());

    // Refresh the cached host and DPU reports so the first iteration also reaches the explored-host repair.
    let endpoints = db::explored_endpoints::find_all(&fixture.pool).await?;
    let addresses = endpoints
        .iter()
        .map(|endpoint| endpoint.address)
        .collect::<Vec<_>>();
    fixture.explorer.insert_endpoints(
        endpoints
            .into_iter()
            .map(|endpoint| (endpoint.address, endpoint.report))
            .collect(),
    );
    let mut txn = fixture.pool.begin().await?;
    db::explored_endpoints::request_exploration_for_addresses(&addresses, txn.as_mut()).await?;
    txn.commit().await?;
    fail_registered_target_writes(&fixture.pool).await?;
    let metrics = MetricsCapture::start();

    // Fail the explored-host repair and both cached repair scans in the same iteration.
    fixture.explorer.run_single_iteration().await?;

    // All three attempts count while the public desired target remains MAC-only.
    assert_eq!(
        metrics.counter_delta(
            "carbide_site_explorer_host_admin_address_reconcile_failures_total",
            &[]
        ),
        3.0
    );
    assert_eq!(
        fixture
            .boot_interfaces()
            .await?
            .reconciliation
            .expect("unrepaired reconciliation")
            .desired_boot_interface,
        Some(provisional_target)
    );

    // Keep the fault active without another exploration request to exercise the two cached repair scans.
    fixture.explorer.run_single_iteration().await?;

    // Match the counter to all five injected failures, including both later repair attempts.
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT last_value FROM bf4_metric_repair_attempts")
            .fetch_one(&fixture.pool)
            .await?,
        5
    );
    assert_eq!(
        metrics.counter_delta(
            "carbide_site_explorer_host_admin_address_reconcile_failures_total",
            &[]
        ),
        5.0
    );

    // Remove only the injected failure so the next iteration must retry the same persisted evidence.
    sqlx::query("DROP TRIGGER reject_bf4_metric_target_write ON machine_boot_interfaces")
        .execute(&fixture.pool)
        .await?;
    fixture.explorer.run_single_iteration().await?;

    // Reload the published pair through the public API and keep the counter unchanged after success.
    let repaired_target = fixture
        .boot_interfaces()
        .await?
        .reconciliation
        .expect("repaired reconciliation")
        .desired_boot_interface
        .expect("registered target");
    assert_eq!(repaired_target.mac_address, fixture.targets[0].to_string());
    assert_eq!(
        repaired_target.interface_id.as_deref(),
        Some("BF4.Controller2.Dpu0")
    );
    assert_eq!(
        metrics.counter_delta(
            "carbide_site_explorer_host_admin_address_reconcile_failures_total",
            &[]
        ),
        5.0
    );
    Ok(())
}
