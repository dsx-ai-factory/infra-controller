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

use super::*;

/// Detects initial BF4 host states from current DPU topology and retained agent classification so implicit boot publishers defer provisional targets.
/// Treats a failed first initialization as pending until retained history proves HostInit or later entry.
///
/// Callers publishing an initial automatic target must hold the machine-group locks and evaluate
/// this predicate after acquiring them. Administrative reapplication callers must hold the host
/// machine row lock while checking existing explicit boot intent.
pub async fn bf4_host_pf_initialization_pending(
    txn: &mut PgConnection,
    host_id: &HostMachineId,
) -> DatabaseResult<bool> {
    let state = lookup_managed_host_state(&mut *txn, *host_id.as_machine_id()).await?;
    if !matches!(
        state,
        Some(
            ManagedHostState::Created
                | ManagedHostState::ConfigureAstra { .. }
                | ManagedHostState::DPUInit { .. }
                | ManagedHostState::DpuDiscoveringState { .. }
                | ManagedHostState::Failed { .. }
        )
    ) || !host_has_bf4_dpu(txn, host_id).await?
    {
        return Ok(false);
    }
    match state {
        // An early failure still defers provisional boot intent; retained HostInit or later history permits ordinary recovery.
        Some(ManagedHostState::Failed { .. }) => Ok(!crate::state_history::for_object(
            txn,
            crate::state_history::StateHistoryTableId::Machine,
            host_id,
        )
        .await?
        .iter()
        .any(|record| {
            serde_json::from_str::<ManagedHostState>(&record.state).is_ok_and(|state| {
                matches!(
                    state,
                    ManagedHostState::HostInit { .. }
                        | ManagedHostState::Ready
                        | ManagedHostState::Assigned { .. }
                        | ManagedHostState::BootConfiguring { .. }
                )
            })
        })),
        _ => Ok(true),
    }
}

/// Detects attached BF4 hardware or retained agent classification independently of lifecycle state so boot-NIC adoption can preserve DPU networking.
///
/// Callers changing interface ownership or primary flags must retain their transaction's segment and interface locks.
pub async fn host_has_bf4_dpu(
    txn: &mut PgConnection,
    host_id: &HostMachineId,
) -> DatabaseResult<bool> {
    let query = "SELECT DISTINCT attached_dpu_machine_id FROM machine_interfaces
                 WHERE machine_id = $1 AND interface_type != 'Bmc'
                   AND attached_dpu_machine_id IS NOT NULL";
    let dpu_ids: Vec<DpuMachineId> = sqlx::query_scalar(query)
        .bind(host_id)
        .fetch_all(&mut *txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;
    Ok(
        crate::machine_topology::find_latest_by_machine_ids(txn, &dpu_ids)
            .await?
            .values()
            .any(|topology| {
                topology.topology.discovery_data.info.is_bf4_dpu()
                    || topology
                        .dpu_host_pf_inventory
                        .as_ref()
                        .is_some_and(|inventory| inventory.is_bf4)
            }),
    )
}

/// Fences initial HostInit publication against state, network-version, instance and DPU-membership changes while retaining fresh-report locks.
///
/// Callers must perform no interface repair in this transaction, reload DPU ACK/health after success, retain it through the state CAS, and roll back on false.
/// History locks precede machine locks; interface and DPU locks acquired after the host never wait.
pub async fn lock_host_init_network_config(
    txn: &mut PgConnection,
    host_id: &HostMachineId,
    expected_state_version: ConfigVersion,
    expected_network_version: ConfigVersion,
    expected_dpus: &[DpuMachineId],
) -> DatabaseResult<bool> {
    // Match controller-state publication's history-before-machine order before taking any row.
    let query = "SELECT pg_advisory_xact_lock(hashtextextended($1, 'machine_state_history'::regclass::bigint))";
    sqlx::query(query)
        .bind(host_id.to_string())
        .execute(&mut *txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;
    let query = "SELECT id FROM machines WHERE id = $1
                 AND controller_state_version = $2 AND network_config_version = $3
                 AND controller_state->>'state' = 'dpuinit' FOR UPDATE";
    let locked: Option<HostMachineId> = sqlx::query_scalar(query)
        .bind(host_id)
        .bind(expected_state_version)
        .bind(expected_network_version)
        .fetch_optional(&mut *txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;
    if locked.is_none()
        || crate::instance::find_id_by_machine_id(&mut *txn, host_id.as_machine_id())
            .await?
            .is_some()
        || !try_lock_host_interfaces_and_dpus(txn, host_id).await?
    {
        return Ok(false);
    }

    // Compare the locked attachments with the exact DPU set used to settle PF facts.
    let query = "SELECT DISTINCT attached_dpu_machine_id FROM machine_interfaces
                 WHERE machine_id = $1 AND interface_type != 'Bmc'
                   AND attached_dpu_machine_id IS NOT NULL ORDER BY attached_dpu_machine_id";
    let attached: Vec<DpuMachineId> = sqlx::query_scalar(query)
        .bind(host_id)
        .fetch_all(txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;
    let mut expected = expected_dpus.to_vec();
    expected.sort();
    expected.dedup();
    Ok(attached == expected)
}

/// Holds declaration and exploration inputs stable through HostInit publication without waiting behind interface-first writers.
///
/// Publication callers must retain successful lock_host_init_network_config locks, recompute selection after success, retain this transaction through the state CAS, and roll back on false.
/// A missing ExpectedMachine row requires a shared table lock to exclude a concurrent declaration insert; existing declarations use only a row lock.
/// Missing endpoint reports return false so reports inserted later cannot escape the publication locks.
pub async fn lock_host_init_selection_inputs(
    txn: &mut PgConnection,
    bmc_mac: Option<MacAddress>,
    bmc_ips: &[IpAddr],
) -> DatabaseResult<bool> {
    // Try every additional lock so writers holding declaration or report rows cannot invert publication's lock order.
    let lock_inputs = async |txn: &mut PgConnection| -> DatabaseResult<bool> {
        if let Some(bmc_mac) = bmc_mac {
            let query = "SELECT bmc_mac_address FROM expected_machines
                         WHERE bmc_mac_address = $1 FOR SHARE NOWAIT";
            let existing: Option<MacAddress> = sqlx::query_scalar(query)
                .bind(bmc_mac)
                .fetch_optional(&mut *txn)
                .await
                .map_err(|error| DatabaseError::query(query, error))?;
            // An absent declaration has no row to lock, so prevent inserts until publication commits.
            if existing.is_none() {
                let query = "LOCK TABLE expected_machines IN SHARE MODE NOWAIT";
                sqlx::query(query)
                    .execute(&mut *txn)
                    .await
                    .map_err(|error| DatabaseError::query(query, error))?;
            }
        }
        let query = "SELECT address FROM explored_endpoints
                     WHERE address = ANY($1) ORDER BY address FOR SHARE NOWAIT";
        let locked_addresses: Vec<IpAddr> = sqlx::query_scalar(query)
            .bind(bmc_ips)
            .fetch_all(&mut *txn)
            .await
            .map_err(|error| DatabaseError::query(query, error))?;
        // A missing row could be inserted before selection is recomputed without acquiring any report lock.
        Ok(bmc_ips
            .iter()
            .all(|address| locked_addresses.contains(address)))
    };
    match lock_inputs(txn).await {
        Ok(locked) => Ok(locked),
        // The caller rolls back the failed lock statement before ordinary initialization retries.
        Err(error) if error.is_lock_not_available() => Ok(false),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Rejects incomplete report locking while accepting repeated addresses whose reports are present, preventing a report inserted after admission from supplying unlocked selection evidence.
    #[crate::sqlx_test]
    async fn selection_input_fence_requires_every_report(pool: sqlx::PgPool) {
        // Report presence drives admission; empty report contents keep this proof independent of host selection.
        let available_reports: [IpAddr; 2] = [
            "192.0.2.10".parse().expect("first report address"),
            "192.0.2.11".parse().expect("second report address"),
        ];
        let missing_report: IpAddr = "192.0.2.12".parse().expect("missing report address");
        let mut setup = pool.begin().await.expect("report setup");
        for address in available_reports {
            crate::explored_endpoints::insert(address, &Default::default(), false, &mut setup)
                .await
                .expect("insert available report");
        }
        setup.commit().await.expect("commit available reports");

        for (requested_addresses, expected) in [
            // Locking existing reports cannot protect a different requested report that may appear later.
            (
                [available_reports[0], available_reports[1], missing_report],
                false,
            ),
            // Repeated references to one locked report must not require another database row.
            (
                [
                    available_reports[0],
                    available_reports[1],
                    available_reports[0],
                ],
                true,
            ),
        ] {
            // Attempt admission using the same report-lock query required before publication.
            let mut publication = pool.begin().await.expect("publication inputs");
            let inputs_locked =
                lock_host_init_selection_inputs(&mut publication, None, &requested_addresses)
                    .await
                    .expect("lock requested reports");

            // Require every distinct requested report and release the locks before the next case.
            assert_eq!(inputs_locked, expected, "{requested_addresses:?}");
            publication.rollback().await.expect("release report locks");
        }
    }

    /// Blocks insertion of a previously absent declaration until the input-lock transaction commits, preventing AddExpectedMachine from introducing an unfenced primary selection.
    #[crate::sqlx_test]
    async fn absent_declaration_fence_blocks_insert_until_commit(pool: sqlx::PgPool) {
        // Leave this BMC MAC undeclared so the fence must protect absence rather than an existing row.
        let bmc_mac = MacAddress::new([2, 0, 0, 0, 0x95, 1]);
        let declared_mac = MacAddress::new([2, 0, 0, 0, 0x95, 2]);
        let mut publication = pool.begin().await.expect("publication transaction");
        assert!(
            lock_host_init_selection_inputs(&mut publication, Some(bmc_mac), &[])
                .await
                .expect("lock absent declaration")
        );
        let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *publication)
            .await
            .expect("publication backend");

        // Use the public add handler's database writer while the absent-declaration fence is retained.
        let writer_pool = pool.clone();
        let writer = tokio::spawn(async move {
            let mut txn = writer_pool.begin().await.expect("declaration writer");
            let created = crate::expected_machine::create(
                &mut txn,
                model::expected_machine::ExpectedMachine {
                    id: None,
                    bmc_mac_address: bmc_mac, // This absent declaration must not appear before publication commits.
                    data: model::expected_machine::ExpectedMachineData {
                        interfaces: vec![model::expected_machine::ExpectedInterface {
                            mac_address: Some(declared_mac),
                            primary: Some(true), // The inserted declaration introduces new boot intent.
                            ..Default::default()
                        }],
                        ..Default::default()
                    },
                },
            )
            .await
            .expect("insert declaration");
            txn.commit().await.expect("commit declaration");
            created
        });
        crate::test_support::postgres::wait_for_blocked_query(
            &pool,
            blocker_pid,
            "INSERT INTO expected_machines",
        )
        .await;
        assert!(!writer.is_finished());

        // Release the same transaction and find the inserted row so a blocked or discarded insert cannot satisfy the proof.
        publication.commit().await.expect("commit publication");
        let created = writer.await.expect("declaration writer task");
        let persisted = crate::expected_machine::find_by_bmc_mac_address(&pool, bmc_mac)
            .await
            .expect("find inserted declaration")
            .expect("persisted declaration");
        assert_eq!(persisted.id, created.id);
        assert_eq!(persisted.bmc_mac_address, bmc_mac);
        assert_eq!(persisted.data.declared_primary_mac(), Some(declared_mac));
    }
}
