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
pub(in crate::tests) mod tests {
    use std::time::Duration;

    use carbide_uuid::machine::MachineId;
    use db::sku::CURRENT_SKU_VERSION;
    use db::{self, DatabaseError, ObjectFilter, WithTransaction};
    use futures_util::FutureExt;
    use model::expected_machine::{ExpectedMachine, ExpectedMachineData};
    use model::machine::machine_search_config::MachineSearchConfig;
    use model::machine::{
        BomValidating, BomValidatingContext, MachineState, MachineValidatingState,
        MachineValidationContext, ManagedHostState, ValidationState,
    };
    use model::metadata::Metadata;
    use model::sku::Sku;
    use model::test_support::ManagedHostConfig;
    use sqlx::PgConnection;

    use crate::test_support::fixture_config::{FixtureDefault as _, ManagedHostConfigExt as _};
    use crate::tests::common::api_fixtures::{
        TestEnv, TestEnvOverrides, TestManagedHost, create_managed_host,
        create_managed_host_with_config, create_test_env, create_test_env_with_overrides,
        get_config,
    };

    // ================================================================================
    // SKU Test Suite Structure
    // ================================================================================
    //
    // This test suite validates SKU (Stock Keeping Unit) validation behavior across
    // different configuration combinations and machine states.
    //
    // Key configuration parameters (A×B matrix):
    //   A = allow_allocation_on_validation_failure (F=block on failure, T=allow allocation despite failures)
    //   B = auto_generate_missing_sku (F=manual, T=auto-generate)
    //
    // When allow_allocation_on_validation_failure=true, machines with assigned
    // SKUs in failed states (SkuVerificationFailed or SkuMissing) can transition
    // back to Ready/MachineValidation instead of staying blocked.
    //
    // ================================================================================
    //
    // SKU VALIDATION TESTS
    // ================================================================================
    //
    // ┌────┬───┬───┬─────────────────────────────┬────────────────────────────┬──────────────────────────────────────────────────────────┐
    // │ #  │ A │ B │ Scenario                    │ Expected Behavior          │ Test Name                                                │
    // ├────┼───┼───┼─────────────────────────────┼────────────────────────────┼──────────────────────────────────────────────────────────┤
    // │ 1. Machine Creation Scenarios                                                                                                    │
    // ├────┼───┼───┼─────────────────────────────┼────────────────────────────┼──────────────────────────────────────────────────────────┤
    // │1.1 │ F │ * │ Machine created, no SKU     │ Ready (fixture assigns SKU)│ test_machine_creation_succeeds_when_not_assigned         │
    // │1.2 │ F │ * │ Same as 1.1, fixture off    │ Stuck in WaitingForSku     │ test_machine_creation_without_auto_sku                   │
    // ├────┼───┼───┼─────────────────────────────┼────────────────────────────┼──────────────────────────────────────────────────────────┤
    // │ 2. WaitingForSkuAssignment State                                                                                                 │
    // ├────┼───┼───┼─────────────────────────────┼────────────────────────────┼──────────────────────────────────────────────────────────┤
    // │2.1 │ F │ F │ WaitingForSkuAssignment     │ Stays in Waiting           │ test_stays_in_waiting_state_when_not_assigned            │
    // │2.2 │ F │ F │ Waiting → Test assigns SKU  │ → UpdatingInventory        │ test_leave_waiting_when_assigned                         │
    // │2.3 │ F │ F │ No SKU assigned             │ Stuck in Waiting           │ test_stuck_in_waiting_without_sku                        │
    // │2.4 │ T │ F │ No SKU assigned             │ Stays in Waiting           │ test_allow_allocation_does_not_bypass_unassigned_sku     │
    // ├────┼───┼───┼─────────────────────────────┼────────────────────────────┼──────────────────────────────────────────────────────────┤
    // │ 3. SkuMissing State                                                                                                              │
    // ├────┼───┼───┼─────────────────────────────┼────────────────────────────┼──────────────────────────────────────────────────────────┤
    // │3.1 │ F │ F │ SKU assigned but missing    │ Stays in SkuMissing        │ test_stays_in_missing_state_when_assigned_sku_is_missing │
    // │3.2 │ F │ F │ SkuMissing → Remove SKU ID  │ → WaitingForSkuAssignment  │ test_proceeds_when_sku_missing                           │
    // │3.3 │ F │ T │ SKU assigned but missing    │ Auto-gen → Ready           │ test_auto_generates_sku_when_missing                     │
    // │3.4 │ T │ F │ SkuMissing state            │ Escapes → MachineValidation│ test_allow_allocation_escapes_from_sku_missing           │
    // ├────┼───┼───┼─────────────────────────────┼────────────────────────────┼──────────────────────────────────────────────────────────┤
    // │ 4. SKU Verification Success Scenarios                                                                                            │
    // ├────┼───┼───┼─────────────────────────────┼────────────────────────────┼──────────────────────────────────────────────────────────┤
    // │4.1 │ F │ * │ SKU assigned and matches    │ MachineValidation → Ready  │ test_discovery_moves_to_machine_validation_state         │
    // │4.2 │ F │ * │ Ready → Manual re-verify    │ Skip MachineVal → Ready    │ test_manual_verify_skips_machine_validation              │
    // │4.3 │ F │ * │ Ready → Manual verify (F→S) │ Skip MachineVal → Ready    │ test_manual_verify_to_failed_to_passed_skips_machine_... │
    // ├────┼───┼───┼─────────────────────────────┼────────────────────────────┼──────────────────────────────────────────────────────────┤
    // │ 5. SKU Verification Failure Scenarios                                                                                            │
    // ├────┼───┼───┼─────────────────────────────┼────────────────────────────┼──────────────────────────────────────────────────────────┤
    // │5.1 │ F │ * │ Re-verify mismatched SKU    │ → VerificationFailed       │ test_stays_in_failed_when_verification_fails             │
    // │5.2 │ F │ * │ Replace SKU, no re-verify   │ Stays Ready (no validation)│ test_continues_to_ready_when_verification_fails          │
    // │5.3 │ T │ * │ SkuVerificationFailed state │ Escapes → Ready            │ test_allow_allocation_escapes_from_sku_failed            │
    // ├────┼───┼───┼─────────────────────────────┼────────────────────────────┼──────────────────────────────────────────────────────────┤
    // │ 6. SKU Auto-Matching Scenarios                                                                                                   │
    // ├────┼───┼───┼─────────────────────────────┼────────────────────────────┼──────────────────────────────────────────────────────────┤
    // │6.1 │ F │ * │ Second machine, same HW     │ Auto-match existing → Ready│ test_auto_match_sku                                      │
    // │6.2 │ F │ * │ SKU replacement triggers    │ Re-verify (to BomValidating)│ test_replace_triggers_verify                            │
    // │6.3 │ F │ * │ Unassign + retained status  │ Immediate re-match → Ready │ test_auto_match_after_unassign                           │
    // ├────┼───┼───┼─────────────────────────────┼────────────────────────────┼──────────────────────────────────────────────────────────┤
    // │ 7. SKU Version Compatibility                                                                                                     │
    // ├────┼───┼───┼─────────────────────────────┼────────────────────────────┼──────────────────────────────────────────────────────────┤
    // │7.1 │ F │ * │ Different SKU schema vers   │ Version compatibility test │ test_match_all_sku_versions                              │
    // └────┴───┴───┴─────────────────────────────┴────────────────────────────┴──────────────────────────────────────────────────────────┘
    //
    // Legend:
    //   A = allow_allocation_on_validation_failure (F=false/block, T=true/allow, *=not relevant)
    //   B = auto_generate_missing_sku (F=false/manual, T=true/auto-gen, *=not relevant)
    //
    // ================================================================================

    pub(in crate::tests) const FULL_SKU_DATA: &str = r#"
    {
        "id": "sku id",
        "description": "PowerEdge R760; 2xCPU; 8xGPU; 256 GiB",
        "created": "2025-01-22T04:33:27.950438037Z",
        "machines_associated_count": 0,
        "components": {
          "chassis": {
            "vendor": "Dell Inc.",
            "model": "PowerEdge R760",
            "architecture": "x86_64"
          },
          "cpus": [
            {
              "vendor": "GenuineIntel",
              "model": "Intel(R) Xeon(R) Gold 6442Y",
              "thread_count": 48,
              "count": 2
            }
          ],
          "gpus": [
            {
              "vendor": "NVIDIA",
              "model": "NVIDIA L40",
              "count": 8,
              "total_memory": "46068 MiB"
            }
          ],
          "ethernet_devices": [],
          "infiniband_devices": [],
          "memory": [
            {
              "memory_type": "DDR5",
              "capacity_mb": 32768,
              "count": 8
            }
          ],
          "storage": [
            {
              "model": "Dell Ent NVMe CM6 RI 1.92TB",
              "count": 1,
              "min_size_mb": 1800000,
              "max_size_mb": 2000000,
              "pci_patterns": ["/devices/pci0000:00/0000:64:00.0/nvme/nvme0/nvme0n1"]
            }
          ],
          "tpm":
            {
              "vendor": "NPCT75x",
              "model": "",
              "count": 1,
              "version": "2.0"
            }
        },
        "schema_version": 5
    }"#;

    const SKU_DATA: &str = r#"
{
  "id": "sku id",
  "description": "PowerEdge R750; 1xCPU; 1xGPU; 2 GiB; 6xIB",
  "created": "2025-01-24T21:51:12.465131195Z",
  "machines_associated_count": 0,
  "components": {
    "chassis": {
      "vendor": "Dell Inc.",
      "model": "PowerEdge R750",
      "architecture": "x86_64"
    },
    "cpus": [
      {
        "vendor": "GenuineIntel",
        "model": "Intel(R) Xeon(R) Gold 6354 CPU @ 3.00GHz",
        "thread_count": 72,
        "count": 1
      }
    ],
    "gpus": [
      {
        "vendor": "NVIDIA",
        "model": "NVIDIA H100 PCIe",
        "total_memory": "81559 MiB",
        "count": 1
      }
    ],
    "ethernet_devices": [],
    "infiniband_devices": [
      {
        "vendor": "0x15b3",
        "model": "MT27800 Family [ConnectX-5]",
        "count": 2,
        "inactive_devices": [0,1]
      },
      {
        "vendor": "0x15b3",
        "model": "MT2910 Family [ConnectX-7]",
        "count": 4,
        "inactive_devices": [0,1,2,3]
      }
    ],
    "storage": [
      {
        "model": "Dell Ent NVMe CM6 RI 1.92TB",
        "count": 1,
        "min_size_mb": 1831420,
        "max_size_mb": 1831420,
        "pci_patterns": ["/devices/pci0000:00/0000:64:00.0/0000:65:00.0/nvme/nvme0/nvme0n1"]
      }
    ],
    "memory": [
      {
        "memory_type": "DDR4",
        "capacity_mb": 1024,
        "count": 2
      }
    ],
    "tpm":
      {
        "vendor": "NPCT75x",
        "model": "",
        "count": 1,
        "version": "2.0"
      }
  },
  "schema_version": 5
}"#;

    pub(in crate::tests) async fn handle_inventory_update(
        pool: &sqlx::PgPool,
        env: &TestEnv,
        mh: &TestManagedHost,
    ) {
        env.run_machine_state_controller_iteration_until_state_condition(
            &mh.host().id,
            3,
            |machine| {
                tracing::info!(
                    machine_state = %machine.current_state(),
                    "waiting for inventory update",
                );
                matches!(
                    machine.current_state(),
                    ManagedHostState::BomValidating {
                        bom_validating_state: BomValidating::UpdatingInventory { .. }
                    }
                )
            },
        )
        .await;

        let mut txn = pool.begin().await.unwrap();
        db::machine::update_discovery_time(&mh.host().id, &mut txn)
            .await
            .unwrap();
        txn.commit().await.unwrap();
    }

    async fn create_test_env_for_bom_validation(
        db_pool: sqlx::PgPool,
        allow_allocation_on_validation_failure: bool,
        find_match_interval: Option<Duration>,
        auto_generate_missing_sku: bool,
    ) -> TestEnv {
        let mut overrides = TestEnvOverrides::default();
        let mut config = get_config();
        config.bom_validation.enabled = true;
        config.bom_validation.allow_allocation_on_validation_failure =
            allow_allocation_on_validation_failure;
        config.bom_validation.ignore_unassigned_machines = false; // Deprecated, will be removed
        config.bom_validation.auto_generate_missing_sku = auto_generate_missing_sku;
        config.bom_validation.auto_generate_missing_sku_interval = Duration::from_secs(2);

        if let Some(find_match_interval) = find_match_interval {
            config.bom_validation.find_match_interval = find_match_interval;
        }
        overrides.config = Some(config);

        let test_env = create_test_env_with_overrides(db_pool, overrides).await;

        assert!(test_env.config.bom_validation.enabled);

        test_env
    }

    async fn create_test_env_ignoring_unassigned_machines(db_pool: sqlx::PgPool) -> TestEnv {
        let mut overrides = TestEnvOverrides::default();
        let mut config = get_config();
        config.bom_validation.enabled = true;
        config.bom_validation.ignore_unassigned_machines = true;
        config.bom_validation.allow_allocation_on_validation_failure = false;
        overrides.config = Some(config);

        create_test_env_with_overrides(db_pool, overrides).await
    }

    async fn get_machine_state(pool: &sqlx::PgPool, mh: &TestManagedHost) -> ManagedHostState {
        let mut txn = pool.begin().await.unwrap();
        let machine = mh.host().db_machine(&mut txn).await;
        machine.current_state().clone()
    }

    /// Helper: Create and assign a mismatched SKU to a machine for testing verification failures
    async fn assign_mismatched_sku(
        txn: &mut PgConnection,
        machine_id: &MachineId,
        current_sku_id: &str,
    ) -> Result<String, eyre::Error> {
        let correct_sku = db::sku::find(txn, &[current_sku_id.to_string()])
            .await?
            .pop()
            .unwrap();

        let mut wrong_sku = correct_sku.clone();
        wrong_sku.id = format!("wrong-{}", current_sku_id);
        wrong_sku.components.cpus[0].count += 1; // Make it mismatch

        db::sku::create(txn, &wrong_sku).await?;
        db::machine::unassign_sku(txn, machine_id).await?;
        db::machine::assign_sku(txn, machine_id, &wrong_sku.id).await?;

        Ok(wrong_sku.id)
    }

    /// Helper: Get machine by ID from database
    async fn get_machine_by_id(
        txn: &mut PgConnection,
        machine_id: &MachineId,
    ) -> Result<model::machine::AnyMachine, eyre::Error> {
        db::machine::find(
            txn,
            ObjectFilter::One(*machine_id),
            MachineSearchConfig::default(),
        )
        .await?
        .pop()
        .ok_or_else(|| eyre::eyre!("machine not found: {}", machine_id))
    }

    /// Helper: Test SKU version compatibility and backward compatibility
    /// Creates a machine with latest SKU version, then generates an older version SKU
    /// from the same hardware to verify version handling and diff behavior
    async fn test_match_sku_versions(
        env: &TestEnv,
        old_schema_version: u32,
    ) -> Result<(), eyre::Error> {
        let (machine_id, _dpu_id) = create_managed_host(env).await.into();

        let mut txn = env.pool.begin().await?;
        let machine = db::machine::find(
            txn.as_mut(),
            ObjectFilter::One(machine_id),
            MachineSearchConfig::default(),
        )
        .await?
        .pop()
        .unwrap();

        assert_eq!(machine.current_state(), &ManagedHostState::Ready);
        assert!(machine.config.hw_sku.is_some());

        let new_sku = db::sku::find(&mut txn, &[machine.config.hw_sku.unwrap()])
            .await?
            .pop()
            .unwrap();
        assert_eq!(new_sku.schema_version, db::sku::CURRENT_SKU_VERSION);
        assert_ne!(new_sku.components.storage.len(), 0);

        // create an older version sku from new topology data will create a backwards compatible sku
        let old_sku = db::sku::generate_sku_from_machine_at_version(
            txn.as_mut(),
            &machine_id,
            old_schema_version,
        )
        .await?;
        assert_eq!(old_sku.schema_version, old_schema_version);

        // diff does not check version.  comparing SKUs of different versions will fail
        let diffs = model::sku::diff_skus(&old_sku, &new_sku);
        assert!(!diffs.is_empty());

        let diffs = model::sku::diff_skus(&old_sku, &old_sku);
        assert!(diffs.is_empty());

        let diffs = model::sku::diff_skus(&old_sku, &new_sku);
        assert!(!diffs.is_empty());

        Ok(())
    }

    // ==================== Core SKU Validation Tests ====================

    #[crate::sqlx_test]
    pub(in crate::tests) async fn test_sku_create(pool: sqlx::PgPool) -> Result<(), eyre::Error> {
        let mut txn = pool.begin().await?;
        let rpc_sku: rpc::forge::Sku = serde_json::de::from_str(FULL_SKU_DATA)?;
        let expected_sku: Sku = rpc_sku.into();
        let expected_sku_json = serde_json::ser::to_string_pretty(&expected_sku)?;

        db::sku::create(&mut txn, &expected_sku).await?;

        let mut actual_sku = db::sku::find(&mut txn, std::slice::from_ref(&expected_sku.id))
            .await?
            .remove(0);
        // cheat the created timestamp
        actual_sku.created = expected_sku.created;

        let actual_sku_json = serde_json::ser::to_string_pretty(&actual_sku)?;

        assert_eq!(actual_sku_json, expected_sku_json);

        let error = db::sku::create(&mut txn, &expected_sku)
            .await
            .expect_err("Duplicate SKU create should have failed");

        assert_eq!(
            error.to_string(),
            format!(
                "argument is invalid: Specified SKU matches SKU with ID: {}",
                expected_sku.id
            )
        );
        Ok(())
    }

    #[crate::sqlx_test]
    pub(in crate::tests) async fn test_sku_create_duplicate_id_returns_already_exists(
        pool: sqlx::PgPool,
    ) -> Result<(), eyre::Error> {
        use rpc::forge::SkuList;
        use rpc::forge::forge_server::Forge;

        let env = create_test_env(pool).await;
        let sku: rpc::forge::Sku = serde_json::de::from_str(FULL_SKU_DATA)?;
        let sku_id = sku.id.clone();
        let mut conflicting_sku = sku.clone();
        conflicting_sku.components.as_mut().unwrap().cpus[0].thread_count *= 2;

        env.api
            .create_sku(tonic::Request::new(SkuList { skus: vec![sku] }))
            .await?;

        let error = env
            .api
            .create_sku(tonic::Request::new(SkuList {
                skus: vec![conflicting_sku],
            }))
            .await
            .expect_err("Creating a SKU with a duplicate ID should fail");

        assert_eq!(error.code(), tonic::Code::AlreadyExists);
        assert_eq!(error.message(), format!("SKU already exists: {sku_id}"));

        Ok(())
    }

    #[crate::sqlx_test]
    pub(in crate::tests) async fn test_sku_create_rejects_empty_id(
        pool: sqlx::PgPool,
    ) -> Result<(), eyre::Error> {
        let mut txn = pool.begin().await?;
        let rpc_sku: rpc::forge::Sku = serde_json::de::from_str(FULL_SKU_DATA)?;
        let mut sku: Sku = rpc_sku.into();
        sku.id = String::new();

        let error = db::sku::create(&mut txn, &sku)
            .await
            .expect_err("Creating a SKU with an empty ID should have failed");

        assert_eq!(
            error.to_string(),
            "argument is invalid: SKU ID must not be empty"
        );
        Ok(())
    }

    #[crate::sqlx_test]
    pub(in crate::tests) async fn test_sku_delete(pool: sqlx::PgPool) -> Result<(), eyre::Error> {
        let mut txn = pool.begin().await?;
        let rpc_sku: rpc::forge::Sku = serde_json::de::from_str(FULL_SKU_DATA)?;
        let expected_sku: Sku = rpc_sku.into();

        db::sku::create(&mut txn, &expected_sku).await?;
        let actual_sku = db::sku::find(&mut txn, std::slice::from_ref(&expected_sku.id))
            .await?
            .remove(0);

        db::sku::delete(&mut txn, &actual_sku.id).await?;

        match db::sku::find(&mut txn, &[expected_sku.id]).await {
            Ok(sku) => {
                if !sku.is_empty() {
                    let sku_name = sku[0].id.clone();
                    panic!("Found a SKU when querying for deleted SKU: {sku_name}")
                }
            }
            Err(carbide_error_type) => panic!("Unexpected error: {carbide_error_type}"),
        }

        Ok(())
    }

    #[crate::sqlx_test]
    async fn test_generate_sku_from_machine(pool: sqlx::PgPool) -> Result<(), eyre::Error> {
        let env = create_test_env(pool.clone()).await;
        let (machine_id, _dpu_id) = create_managed_host(&env).await.into();

        let expected_sku: Sku = serde_json::de::from_str::<rpc::forge::Sku>(SKU_DATA)?.into();

        let mut actual_sku = db::sku::generate_sku_from_machine(&pool, &machine_id).await?;
        // cheat the created timestamp and id
        actual_sku.id = "sku id".to_string();
        actual_sku.created = expected_sku.created;

        let actual_sku_json: String = serde_json::ser::to_string_pretty(&actual_sku)?;
        tracing::info!(
            actual_sku_json = %actual_sku_json,
            "Serialized actual SKU",
        );
        let expected_sku_json = serde_json::ser::to_string_pretty(&expected_sku)?;
        tracing::info!(
            expected_sku_json = %expected_sku_json,
            "Serialized expected SKU",
        );

        assert_eq!(actual_sku_json, expected_sku_json);

        Ok(())
    }

    #[crate::sqlx_test]
    async fn test_api_happy_path(pool: sqlx::PgPool) -> Result<(), eyre::Error> {
        let env = create_test_env(pool.clone()).await;
        let (machine_id, _dpu_id) = create_managed_host(&env).await.into();
        let mut txn = pool.begin().await?;

        let actual_sku = db::sku::generate_sku_from_machine(txn.as_mut(), &machine_id).await?;
        db::sku::create(&mut txn, &actual_sku).await?;
        let actual_sku = db::sku::find(&mut txn, &[actual_sku.id]).await?.remove(0);

        db::machine::assign_sku(&mut txn, &machine_id, &actual_sku.id).await?;

        let machine = db::machine::find(
            txn.as_mut(),
            ObjectFilter::One(machine_id),
            MachineSearchConfig::default(),
        )
        .await?
        .pop()
        .unwrap();
        assert_eq!(machine.config.hw_sku.unwrap(), actual_sku.id);

        db::machine::unassign_sku(&mut txn, &machine_id).await?;

        let machine = db::machine::find(
            txn.as_mut(),
            ObjectFilter::One(machine_id),
            MachineSearchConfig::default(),
        )
        .await?
        .pop()
        .unwrap();

        assert!(machine.config.hw_sku.is_none());

        let sku_id = actual_sku.id.clone();
        db::sku::delete(&mut txn, &actual_sku.id).await?;

        match db::sku::find(&mut txn, &[sku_id]).await {
            // We expect an okay result, but that it should be an empty list.
            Ok(sku) => {
                if !sku.is_empty() {
                    let sku_name = sku[0].id.clone();
                    panic!("Found a SKU when querying for deleted SKU: {sku_name}")
                }
            }
            Err(carbide_error_type) => panic!("Unexpected error: {carbide_error_type}"),
        }

        Ok(())
    }

    /// Test 1.1: Machine Creation (with auto SKU assignment)
    /// Conditions:
    /// - allow_allocation_on_validation_failure = false (standard mode)
    /// - auto_generate_missing_sku = false
    /// - auto_assign_sku_in_fixture = true (default)
    /// - no SKU assigned initially
    /// Expected: machine creation succeeds, reaches Ready state
    /// Note: Test fixture automatically generates and assigns SKU when machine enters WaitingForSkuAssignment
    #[crate::sqlx_test]
    async fn test_machine_creation_succeeds_when_not_assigned(
        pool: sqlx::PgPool,
    ) -> Result<(), eyre::Error> {
        let env = create_test_env_for_bom_validation(pool.clone(), false, None, false).await;

        let (machine_id, _dpu_id) = create_managed_host(&env).await.into();

        let machine = db::machine::find(
            &pool,
            ObjectFilter::One(machine_id),
            MachineSearchConfig::default(),
        )
        .await?
        .pop()
        .unwrap();

        // Machine should reach Ready state (test fixture generates and assigns SKU)
        assert_eq!(machine.current_state(), &ManagedHostState::Ready);
        assert!(machine.config.hw_sku.is_some());

        Ok(())
    }

    /// Test 1.2: Machine Creation (without auto SKU assignment)
    /// Conditions:
    /// - allow_allocation_on_validation_failure = false (standard mode)
    /// - auto_generate_missing_sku = false
    /// - auto_assign_sku_in_fixture = false
    /// - no SKU assigned initially
    /// Expected: machine gets stuck in WaitingForSkuAssignment, fixture returns early
    #[crate::sqlx_test]
    async fn test_machine_creation_without_auto_sku(pool: sqlx::PgPool) -> Result<(), eyre::Error> {
        let env = create_test_env_for_bom_validation(pool.clone(), false, None, false).await;

        let config = ManagedHostConfig {
            auto_assign_sku_in_fixture: false,
            ..ManagedHostConfig::default()
        };

        let mh = create_managed_host_with_config(&env, config).await;
        let (machine_id, _dpu_id) = mh.into();

        let machine = db::machine::find(
            &pool,
            ObjectFilter::One(machine_id),
            MachineSearchConfig::default(),
        )
        .await?
        .pop()
        .unwrap();

        // Machine should be stuck in WaitingForSkuAssignment (no auto SKU assignment by fixture)
        assert!(matches!(
            machine.current_state(),
            ManagedHostState::BomValidating {
                bom_validating_state: BomValidating::WaitingForSkuAssignment(_)
            }
        ));
        assert!(machine.config.hw_sku.is_none());

        Ok(())
    }

    /// Test 2.1: WaitingForSkuAssignment State
    /// Conditions:
    /// - allow_allocation_on_validation_failure = false (standard mode)
    /// - auto_generate_missing_sku = false
    /// - machine has no assigned SKU
    /// Expected: machine stays in WaitingForSkuAssignment state
    #[crate::sqlx_test]
    async fn test_stays_in_waiting_state_when_not_assigned(
        pool: sqlx::PgPool,
    ) -> Result<(), eyre::Error> {
        let env = create_test_env_for_bom_validation(pool.clone(), false, None, false).await;

        // Create machine config with expected state
        let managed_host_config =
            ManagedHostConfig::default().with_expected_state(ManagedHostState::BomValidating {
                bom_validating_state: BomValidating::WaitingForSkuAssignment(
                    BomValidatingContext {
                        machine_validation_context: Some(MachineValidationContext::Discovery),
                        ..BomValidatingContext::default()
                    },
                ),
            });
        // Note: auto_assign_sku_in_fixture doesn't matter here because when
        // expected_state == WaitingForSkuAssignment, fixture returns immediately
        // when machine reaches that state, without calling assign_sku_if_needed

        let mh = create_managed_host_with_config(&env, managed_host_config).await;

        // Verify machine stays in WaitingForSkuAssignment (run state machine a few times to ensure it's stable)
        env.run_machine_state_controller_iteration().await;
        env.run_machine_state_controller_iteration().await;

        let mut txn = pool.begin().await?;
        let machine = mh.host().db_machine(&mut txn).await;

        assert!(matches!(
            machine.current_state(),
            ManagedHostState::BomValidating {
                bom_validating_state: BomValidating::WaitingForSkuAssignment(_)
            }
        ));

        Ok(())
    }

    /// Test 2.2: WaitingForSkuAssignment → Manual SKU assignment
    /// Conditions:
    /// - allow_allocation_on_validation_failure = false (testing standard state machine flow)
    /// - auto_generate_missing_sku = false
    /// - machine is in WaitingForSkuAssignment state
    /// - SKU is assigned to machine
    /// Expected: transitions from WaitingForSkuAssignment to UpdatingInventory
    #[crate::sqlx_test]
    async fn test_leave_waiting_when_assigned(pool: sqlx::PgPool) -> Result<(), eyre::Error> {
        let env = create_test_env_for_bom_validation(pool.clone(), false, None, false).await;
        let managed_host_config =
            ManagedHostConfig::default().with_expected_state(ManagedHostState::BomValidating {
                bom_validating_state: BomValidating::WaitingForSkuAssignment(
                    BomValidatingContext {
                        machine_validation_context: Some(MachineValidationContext::Discovery),
                        ..BomValidatingContext::default()
                    },
                ),
            });

        let mh = create_managed_host_with_config(&env, managed_host_config).await;
        let machine_id = mh.host().id;

        let mut txn = pool.begin().await?;

        let actual_sku = db::sku::generate_sku_from_machine(txn.as_mut(), &machine_id).await?;
        db::sku::create(&mut txn, &actual_sku).await?;

        db::machine::assign_sku(&mut txn, &machine_id, &actual_sku.id).await?;

        txn.commit().await?;

        // once the sku is assigned, the state machine should move to update inventory before it verifies it.
        env.run_machine_state_controller_iteration().await;

        let mut txn = pool.begin().await?;
        let machine = mh.host().db_machine(&mut txn).await;
        assert!(matches!(
            machine.current_state(),
            ManagedHostState::BomValidating {
                bom_validating_state: BomValidating::UpdatingInventory(_)
            }
        ));

        Ok(())
    }

    /// Test 2.3: WaitingForSkuAssignment with allow_allocation=false
    /// Conditions:
    /// - allow_allocation_on_validation_failure = false
    /// - auto_generate_missing_sku = false
    /// - auto_assign_sku_in_fixture = false
    /// - machine starts from default state, no SKU
    /// Expected: machine enters WaitingForSkuAssignment and stays stuck
    #[crate::sqlx_test]
    async fn test_stuck_in_waiting_without_sku(pool: sqlx::PgPool) -> Result<(), eyre::Error> {
        let env = create_test_env_for_bom_validation(pool.clone(), false, None, false).await;

        let mut config =
            ManagedHostConfig::default().with_expected_state(ManagedHostState::BomValidating {
                bom_validating_state: BomValidating::WaitingForSkuAssignment(
                    BomValidatingContext {
                        machine_validation_context: Some(MachineValidationContext::Discovery),
                        ..BomValidatingContext::default()
                    },
                ),
            });
        config.auto_assign_sku_in_fixture = false;

        let mh = create_managed_host_with_config(&env, config).await;

        let mut txn = pool.begin().await?;
        let machine = mh.host().db_machine(&mut txn).await;

        // Machine should be stuck in WaitingForSkuAssignment
        assert!(
            matches!(
                machine.current_state(),
                ManagedHostState::BomValidating {
                    bom_validating_state: BomValidating::WaitingForSkuAssignment(_)
                }
            ),
            "Machine should be stuck in WaitingForSkuAssignment"
        );
        assert!(machine.config.hw_sku.is_none());
        txn.commit().await?;

        Ok(())
    }

    /// Test 2.4: WaitingForSkuAssignment with allow_allocation_on_validation_failure=true
    /// Conditions:
    /// - allow_allocation_on_validation_failure = true
    /// - auto_generate_missing_sku = false
    /// - auto_assign_sku_in_fixture = false
    /// Expected: machine enters WaitingForSkuAssignment because no SKU validation occurred
    #[crate::sqlx_test]
    async fn test_allow_allocation_does_not_bypass_unassigned_sku(
        pool: sqlx::PgPool,
    ) -> Result<(), eyre::Error> {
        let env = create_test_env_for_bom_validation(pool.clone(), true, None, false).await;

        let mut config =
            ManagedHostConfig::default().with_expected_state(ManagedHostState::BomValidating {
                bom_validating_state: BomValidating::WaitingForSkuAssignment(
                    BomValidatingContext {
                        machine_validation_context: Some(MachineValidationContext::Discovery),
                        ..BomValidatingContext::default()
                    },
                ),
            });
        config.auto_assign_sku_in_fixture = false;

        let mh = create_managed_host_with_config(&env, config).await;
        let mut txn = pool.begin().await?;
        let machine = mh.host().db_machine(&mut txn).await;

        assert!(matches!(
            machine.current_state(),
            ManagedHostState::BomValidating {
                bom_validating_state: BomValidating::WaitingForSkuAssignment(_)
            }
        ));
        assert!(machine.config.hw_sku.is_none());

        Ok(())
    }

    /// Test 2.5: Machine creation with ignore_unassigned_machines=true
    /// Conditions:
    /// - ignore_unassigned_machines = true
    /// - no SKU is assigned
    /// Expected: machine reaches Ready state without an assigned SKU
    #[crate::sqlx_test]
    async fn test_ignore_unassigned_machines_bypasses_unassigned_sku(
        pool: sqlx::PgPool,
    ) -> Result<(), eyre::Error> {
        let env = create_test_env_ignoring_unassigned_machines(pool.clone()).await;
        let config = ManagedHostConfig {
            auto_assign_sku_in_fixture: false,
            ..ManagedHostConfig::default()
        };

        let mh = create_managed_host_with_config(&env, config).await;
        let mut txn = pool.begin().await?;
        let machine = mh.host().db_machine(&mut txn).await;

        assert_eq!(machine.current_state(), &ManagedHostState::Ready);
        assert!(machine.config.hw_sku.is_none());
        assert!(
            machine
                .status
                .hw_sku
                .is_some_and(|status| status.last_match_attempt.is_some())
        );

        Ok(())
    }

    /// Test 3.1: SkuMissing State → stays in SkuMissing
    /// Conditions:
    /// - allow_allocation_on_validation_failure = false (testing standard state machine flow from SkuMissing state)
    /// - auto_generate_missing_sku = false
    /// - machine has assigned SKU but SKU doesn't exist in database
    /// Expected: machine stays in SkuMissing state until SKU is created
    #[crate::sqlx_test]
    async fn test_stays_in_missing_state_when_assigned_sku_is_missing(
        pool: sqlx::PgPool,
    ) -> Result<(), eyre::Error> {
        let env = create_test_env_for_bom_validation(pool.clone(), false, None, false).await;

        let managed_host_config =
            ManagedHostConfig::default().with_expected_state(ManagedHostState::BomValidating {
                bom_validating_state: BomValidating::SkuMissing(BomValidatingContext {
                    machine_validation_context: Some(MachineValidationContext::Discovery),
                    ..BomValidatingContext::default()
                }),
            });

        let mut txn = pool.begin().await?;
        db::expected_machine::create(
            &mut txn,
            ExpectedMachine {
                id: None,
                bmc_mac_address: managed_host_config.bmc_mac_address,
                data: ExpectedMachineData {
                    bmc_username: "admin".to_string(),
                    bmc_password: "password".to_string(),
                    serial_number: "1234567890".to_string(),
                    fallback_dpu_serial_numbers: vec![],
                    metadata: Metadata::new_with_default_name(),
                    sku_id: Some("no-sku".to_string()),
                    default_pause_ingestion_and_poweron: None,
                    interfaces: vec![],
                    rack_id: None,
                    dpf_enabled: Some(true),
                    bmc_ip_address: None,
                    bmc_retain_credentials: None,
                    dpu_policy: Default::default(),
                    bmc_ip_allocation: Default::default(),
                    host_lifecycle_profile: Default::default(),
                },
            },
        )
        .await?;
        txn.commit().await?;

        let mh = create_managed_host_with_config(&env, managed_host_config).await;

        // Verify machine stays stuck in SkuMissing state (run state machine multiple times to ensure it doesn't progress)
        env.run_machine_state_controller_iteration().await;
        env.run_machine_state_controller_iteration().await;
        env.run_machine_state_controller_iteration().await;

        let mut txn = pool.begin().await?;
        let machine = mh.host().db_machine(&mut txn).await;
        let machine_id = mh.host().id;

        assert!(matches!(
            machine.current_state(),
            ManagedHostState::BomValidating {
                bom_validating_state: BomValidating::SkuMissing(_)
            }
        ));

        let mut sku = db::sku::generate_sku_from_machine(txn.as_mut(), &machine_id).await?;
        sku.id = "no-sku".to_string();

        db::sku::create(&mut txn, &sku).await?;

        txn.commit().await?;

        handle_inventory_update(&pool, &env, &mh).await;

        env.run_machine_state_controller_iteration_until_state_condition(&machine_id, 10, |m| {
            matches!(m.current_state(), ManagedHostState::Validation { .. })
        })
        .await;

        // Complete machine validation process (see "Test Pattern Notes" at top of file)
        mh.host().reboot_completed().await;
        env.run_machine_state_controller_iteration().await; // Process reboot event
        mh.machine_validation_completed().await;
        env.run_machine_state_controller_iteration().await; // Process validation completion
        mh.host().reboot_completed().await;

        // run until ready
        env.run_machine_state_controller_iteration_until_state_matches(
            &machine_id,
            20,
            ManagedHostState::Ready,
        )
        .await;

        Ok(())
    }

    /// Test 3.2: SkuMissing → transitions to WaitingForSkuAssignment when SKU ID is removed
    /// Conditions:
    /// - allow_allocation_on_validation_failure = false (standard mode)
    /// - auto_generate_missing_sku = false
    /// - machine is in SkuMissing state but then SKU ID is removed
    /// Expected: machine transitions to WaitingForSkuAssignment
    #[crate::sqlx_test]
    async fn test_proceeds_when_sku_missing(pool: sqlx::PgPool) -> Result<(), eyre::Error> {
        let env = create_test_env_for_bom_validation(pool.clone(), false, None, false).await;

        let managed_host_config =
            ManagedHostConfig::default().with_expected_state(ManagedHostState::BomValidating {
                bom_validating_state: BomValidating::SkuMissing(BomValidatingContext {
                    machine_validation_context: Some(MachineValidationContext::Discovery),
                    ..BomValidatingContext::default()
                }),
            });

        let mut txn = pool.begin().await?;
        db::expected_machine::create(
            &mut txn,
            ExpectedMachine {
                id: None,
                bmc_mac_address: managed_host_config.bmc_mac_address,
                data: ExpectedMachineData {
                    bmc_username: "admin".to_string(),
                    bmc_password: "password".to_string(),
                    serial_number: "1234567890".to_string(),
                    fallback_dpu_serial_numbers: vec![],
                    metadata: Metadata::new_with_default_name(),
                    sku_id: Some("no-sku-missing".to_string()),
                    default_pause_ingestion_and_poweron: None,
                    interfaces: vec![],
                    rack_id: None,
                    dpf_enabled: Some(true),
                    bmc_ip_address: None,
                    bmc_retain_credentials: None,
                    dpu_policy: Default::default(),
                    bmc_ip_allocation: Default::default(),
                    host_lifecycle_profile: Default::default(),
                },
            },
        )
        .await?;
        txn.commit().await?;

        let mh = create_managed_host_with_config(&env, managed_host_config).await;
        let machine_id = mh.host().id;

        // Remove the SKU assignment from the machine
        let mut txn = pool.begin().await?;
        db::machine::unassign_sku(&mut txn, &machine_id).await?;
        txn.commit().await?;

        // Run state controller - should transition to WaitingForSkuAssignment
        env.run_machine_state_controller_iteration().await;

        let mut txn = pool.begin().await?;
        let machine = mh.host().db_machine(&mut txn).await;

        // Should transition to WaitingForSkuAssignment when SKU ID is removed
        assert!(matches!(
            machine.current_state(),
            ManagedHostState::BomValidating {
                bom_validating_state: BomValidating::WaitingForSkuAssignment(_)
            }
        ));

        Ok(())
    }

    /// Test 3.3: SkuMissing → auto-generates SKU
    /// Conditions:
    /// - allow_allocation_on_validation_failure = false (testing standard state machine flow from SkuMissing state)
    /// - auto_generate_missing_sku = true
    /// - machine has assigned SKU but SKU doesn't exist in database
    /// Expected: SKU is auto-generated, machine proceeds to verification and reaches Ready
    #[crate::sqlx_test]
    async fn test_auto_generates_sku_when_missing(pool: sqlx::PgPool) -> Result<(), eyre::Error> {
        let env = create_test_env_for_bom_validation(pool.clone(), false, None, true).await;

        let managed_host_config =
            ManagedHostConfig::default().with_expected_state(ManagedHostState::BomValidating {
                bom_validating_state: BomValidating::SkuMissing(BomValidatingContext {
                    machine_validation_context: Some(MachineValidationContext::Discovery),
                    ..BomValidatingContext::default()
                }),
            });

        let mut txn = pool.begin().await?;
        db::expected_machine::create(
            &mut txn,
            ExpectedMachine {
                id: None,
                bmc_mac_address: managed_host_config.bmc_mac_address,
                data: ExpectedMachineData {
                    bmc_username: "admin".to_string(),
                    bmc_password: "password".to_string(),
                    serial_number: "1234567890".to_string(),
                    fallback_dpu_serial_numbers: vec![],
                    metadata: Metadata::new_with_default_name(),
                    sku_id: Some("no-sku".to_string()),
                    default_pause_ingestion_and_poweron: None,
                    interfaces: vec![],
                    rack_id: None,
                    dpf_enabled: Some(true),
                    bmc_ip_address: None,
                    bmc_retain_credentials: None,
                    dpu_policy: Default::default(),
                    bmc_ip_allocation: Default::default(),
                    host_lifecycle_profile: Default::default(),
                },
            },
        )
        .await?;
        txn.commit().await?;

        let mh = create_managed_host_with_config(&env, managed_host_config).await;
        let machine_id = mh.host().id;

        let mut txn = pool.begin().await?;
        let machine = mh.host().db_machine(&mut txn).await;

        assert!(matches!(
            machine.current_state(),
            ManagedHostState::BomValidating {
                bom_validating_state: BomValidating::SkuMissing(_)
            }
        ));
        txn.commit().await?;

        // Auto-gen enabled: SKU should be auto-generated
        handle_inventory_update(&pool, &env, &mh).await;

        env.run_machine_state_controller_iteration_until_state_condition(&machine_id, 10, |m| {
            matches!(m.current_state(), ManagedHostState::Validation { .. })
        })
        .await;

        // Process machine validation events (each iteration processes the event above it)
        mh.host().reboot_completed().await;
        env.run_machine_state_controller_iteration().await;
        mh.machine_validation_completed().await;
        env.run_machine_state_controller_iteration().await;
        mh.host().reboot_completed().await;

        // Should reach Ready state
        env.run_machine_state_controller_iteration_until_state_matches(
            &machine_id,
            20,
            ManagedHostState::Ready,
        )
        .await;

        Ok(())
    }

    /// Test 3.4: SkuMissing → Escapes to MachineValidation with allow_allocation_on_validation_failure=true
    /// Conditions:
    /// - allow_allocation_on_validation_failure = true (allow machines to escape from failed states)
    /// - auto_generate_missing_sku = false
    /// - machine has assigned SKU but SKU doesn't exist in database
    /// Expected: Machine escapes SkuMissing state and proceeds to MachineValidation/Ready
    #[crate::sqlx_test]
    async fn test_allow_allocation_escapes_from_sku_missing(
        pool: sqlx::PgPool,
    ) -> Result<(), eyre::Error> {
        let env = create_test_env_for_bom_validation(pool.clone(), true, None, false).await;

        let managed_host_config =
            ManagedHostConfig::default().with_expected_state(ManagedHostState::BomValidating {
                bom_validating_state: BomValidating::SkuMissing(BomValidatingContext {
                    machine_validation_context: Some(MachineValidationContext::Discovery),
                    ..BomValidatingContext::default()
                }),
            });

        let mut txn = pool.begin().await?;
        db::expected_machine::create(
            &mut txn,
            ExpectedMachine {
                id: None,
                bmc_mac_address: managed_host_config.bmc_mac_address,
                data: ExpectedMachineData {
                    bmc_username: "admin".to_string(),
                    bmc_password: "password".to_string(),
                    serial_number: "1234567890".to_string(),
                    fallback_dpu_serial_numbers: vec![],
                    metadata: Metadata::new_with_default_name(),
                    sku_id: Some("non-existent-sku".to_string()),
                    default_pause_ingestion_and_poweron: None,
                    interfaces: vec![],
                    rack_id: None,
                    dpf_enabled: Some(true),
                    bmc_ip_address: None,
                    bmc_retain_credentials: None,
                    dpu_policy: Default::default(),
                    bmc_ip_allocation: Default::default(),
                    host_lifecycle_profile: Default::default(),
                },
            },
        )
        .await?;
        txn.commit().await?;

        let mh = create_managed_host_with_config(&env, managed_host_config).await;
        let machine_id = mh.host().id;

        let mut txn = pool.begin().await?;
        let machine = mh.host().db_machine(&mut txn).await;

        assert!(matches!(
            machine.current_state(),
            ManagedHostState::BomValidating {
                bom_validating_state: BomValidating::SkuMissing(_)
            }
        ));
        txn.commit().await?;

        // With allow_allocation_on_validation_failure=true, machine should escape SkuMissing
        // and proceed to MachineValidation
        env.run_machine_state_controller_iteration_until_state_condition(&machine_id, 10, |m| {
            matches!(m.current_state(), ManagedHostState::Validation { .. })
        })
        .await;

        // Process machine validation events (each iteration processes the event above it)
        mh.host().reboot_completed().await;
        env.run_machine_state_controller_iteration().await;
        mh.machine_validation_completed().await;
        env.run_machine_state_controller_iteration().await;
        mh.host().reboot_completed().await;

        // Should reach Ready state
        env.run_machine_state_controller_iteration_until_state_matches(
            &machine_id,
            20,
            ManagedHostState::Ready,
        )
        .await;

        Ok(())
    }

    /// Test 4.1: SKU Verification Success → MachineValidation
    /// Conditions:
    /// - allow_allocation_on_validation_failure = false (standard mode)
    /// - auto_generate_missing_sku = false
    /// - machine validation context = "Discovery"
    /// - SKU is assigned and verified
    /// Expected: machine progresses through VerifyingSku to MachineValidation state
    #[crate::sqlx_test]
    async fn test_discovery_moves_to_machine_validation_state(
        pool: sqlx::PgPool,
    ) -> Result<(), eyre::Error> {
        let env = create_test_env_for_bom_validation(pool.clone(), false, None, false).await;
        let managed_host_config =
            ManagedHostConfig::default().with_expected_state(ManagedHostState::BomValidating {
                bom_validating_state: BomValidating::WaitingForSkuAssignment(
                    BomValidatingContext {
                        machine_validation_context: Some(MachineValidationContext::Discovery),
                        ..BomValidatingContext::default()
                    },
                ),
            });

        let mh = create_managed_host_with_config(&env, managed_host_config).await;
        let machine_id = mh.host().id;

        let mut txn = pool.begin().await?;

        let actual_sku = db::sku::generate_sku_from_machine(txn.as_mut(), &machine_id).await?;
        db::sku::create(&mut txn, &actual_sku).await?;

        db::machine::assign_sku(&mut txn, &machine_id, &actual_sku.id).await?;

        txn.commit().await?;

        handle_inventory_update(&pool, &env, &mh).await;

        env.run_machine_state_controller_iteration().await;

        let state = get_machine_state(&pool, &mh).await;
        assert!(matches!(
            state,
            ManagedHostState::BomValidating {
                bom_validating_state: BomValidating::VerifyingSku(_)
            }
        ));

        env.run_machine_state_controller_iteration().await;
        let state = get_machine_state(&pool, &mh).await;
        assert!(matches!(
            state,
            ManagedHostState::Validation {
                validation_state: ValidationState::MachineValidation {
                    machine_validation: MachineValidatingState::RebootHost { .. }
                }
            }
        ));
        env.run_machine_state_controller_iteration().await;
        let state = get_machine_state(&pool, &mh).await;
        assert!(matches!(
            state,
            ManagedHostState::Validation {
                validation_state: ValidationState::MachineValidation {
                    machine_validation: MachineValidatingState::MachineValidating { .. }
                }
            }
        ));

        Ok(())
    }

    /// Test 4.2: Manual re-verify skips MachineValidation
    /// Conditions:
    /// - allow_allocation_on_validation_failure = false (standard mode)
    /// - auto_generate_missing_sku = false
    /// - verify_request_time is manually updated
    /// Expected: machine skips MachineValidation and goes directly to Ready state
    #[crate::sqlx_test]
    async fn test_manual_verify_skips_machine_validation(
        pool: sqlx::PgPool,
    ) -> Result<(), eyre::Error> {
        let env = create_test_env_for_bom_validation(pool.clone(), false, None, false).await;

        let mh = create_managed_host(&env).await;

        let state = get_machine_state(&pool, &mh).await;

        assert!(matches!(state, ManagedHostState::Ready));

        let mut txn = pool.begin().await?;

        let machine = mh.host().db_machine(&mut txn).await;
        let machine_id = mh.host().id;

        db::machine::update_sku_status_verify_request_time(&mut txn, &machine_id).await?;

        txn.commit().await?;

        let mut state = machine.current_state().clone();

        for _ in 0..20 {
            env.run_machine_state_controller_iteration().await;
            state = get_machine_state(&pool, &mh).await;
            assert!(!matches!(
                state,
                ManagedHostState::Validation {
                    validation_state: ValidationState::MachineValidation {
                        machine_validation: MachineValidatingState::MachineValidating { .. }
                    }
                }
            ));
            if state == ManagedHostState::Ready {
                break;
            }
            if matches!(
                state,
                ManagedHostState::BomValidating {
                    bom_validating_state: BomValidating::UpdatingInventory(..)
                }
            ) {
                let mut txn = pool.begin().await?;

                db::machine::update_discovery_time(&machine.id, &mut txn)
                    .await
                    .unwrap();
                txn.commit().await.unwrap();
            }
        }

        assert_eq!(state, ManagedHostState::Ready);

        Ok(())
    }

    /// Test 4.3: Manual verify from failed to passed skips MachineValidation
    /// Conditions:
    /// - allow_allocation_on_validation_failure = false (required: ensures machine stays in SkuVerificationFailed
    ///   state waiting for manual fix, rather than escaping back to Ready)
    /// - auto_generate_missing_sku = * (not relevant - SKUs are manually created)
    /// - machine initially assigned wrong SKU (verification fails → SkuVerificationFailed)
    /// - then assigned correct SKU with manual verify request
    /// Expected: machine skips MachineValidation and goes directly to Ready
    #[crate::sqlx_test]
    async fn test_manual_verify_to_failed_to_passed_skips_machine_validation(
        pool: sqlx::PgPool,
    ) -> Result<(), eyre::Error> {
        let env = create_test_env_for_bom_validation(pool.clone(), false, None, false).await;

        let mh = create_managed_host(&env).await;

        let state = get_machine_state(&pool, &mh).await;

        assert!(matches!(state, ManagedHostState::Ready));

        let mut txn = pool.begin().await?;

        let machine = mh.host().db_machine(&mut txn).await;
        let machine_id = mh.host().id;

        let original_sku = db::sku::find(&mut txn, &[machine.config.hw_sku.clone().unwrap()])
            .await?
            .pop()
            .unwrap();

        tracing::info!(
            original_sku = ?original_sku,
            "Original SKU before mismatch",
        );

        let mut broken_sku = original_sku.clone();
        broken_sku.id = "Broken SKU".to_string();
        broken_sku.components.cpus[0].count += 1;

        db::sku::create(&mut txn, &broken_sku).await?;

        tracing::info!(
            broken_sku = ?broken_sku,
            "Broken SKU after mismatch",
        );

        db::machine::unassign_sku(&mut txn, &machine_id).await?;
        db::machine::assign_sku(&mut txn, &machine_id, &broken_sku.id).await?;

        db::machine::update_sku_status_verify_request_time(&mut txn, &machine_id).await?;

        txn.commit().await?;

        handle_inventory_update(&pool, &env, &mh).await;

        env.run_machine_state_controller_iteration_until_state_condition(
            &machine_id,
            3,
            |machine| {
                assert!(!matches!(
                    machine.current_state(),
                    ManagedHostState::Validation {
                        validation_state: ValidationState::MachineValidation {
                            machine_validation: MachineValidatingState::MachineValidating { .. }
                        },
                    }
                ));
                matches!(
                    machine.current_state(),
                    ManagedHostState::BomValidating {
                        bom_validating_state: BomValidating::SkuVerificationFailed(
                            BomValidatingContext { .. },
                        ),
                    }
                )
            },
        )
        .await;

        let mut txn = pool.begin().await?;

        db::machine::unassign_sku(&mut txn, &machine_id).await?;
        db::machine::assign_sku(&mut txn, &machine_id, &original_sku.id).await?;

        db::machine::update_sku_status_verify_request_time(&mut txn, &machine_id).await?;
        txn.commit().await?;

        handle_inventory_update(&pool, &env, &mh).await;

        env.run_machine_state_controller_iteration_until_state_condition(
            &machine_id,
            3,
            |machine| {
                assert!(!matches!(
                    machine.current_state(),
                    ManagedHostState::Validation {
                        validation_state: ValidationState::MachineValidation {
                            machine_validation: MachineValidatingState::MachineValidating { .. }
                        }
                    }
                ));
                matches!(
                    machine.current_state(),
                    ManagedHostState::HostInit {
                        machine_state: MachineState::Discovered { .. },
                    },
                )
            },
        )
        .await;
        mh.host().forge_agent_control().await;

        let mut state = get_machine_state(&pool, &mh).await;
        for _ in 0..3 {
            env.run_machine_state_controller_iteration().await;

            state = get_machine_state(&pool, &mh).await;
            assert!(!matches!(
                state,
                ManagedHostState::Validation {
                    validation_state: ValidationState::MachineValidation {
                        machine_validation: MachineValidatingState::MachineValidating { .. }
                    }
                }
            ));
            if state == ManagedHostState::Ready {
                break;
            }
        }

        assert_eq!(state, ManagedHostState::Ready);
        Ok(())
    }

    /// Test 5.1: SKU Verification Failure → stays in failed state
    /// Conditions:
    /// - allow_allocation_on_validation_failure = false (testing standard state machine flow from SkuVerificationFailed state)
    /// - auto_generate_missing_sku = * (doesn't matter - SKU exists, just mismatched)
    /// - machine has assigned SKU that doesn't match hardware, explicit re-verification is triggered
    /// Expected: machine stays in SkuVerificationFailed state (blocked from allocation)
    #[crate::sqlx_test]
    async fn test_stays_in_failed_when_verification_fails(
        pool: sqlx::PgPool,
    ) -> Result<(), eyre::Error> {
        let env = create_test_env_for_bom_validation(pool.clone(), false, None, false).await;

        let mh = create_managed_host(&env).await;
        let machine_id = mh.host().id;

        let mut txn = pool.begin().await?;
        let machine = mh.host().db_machine(&mut txn).await;
        let current_sku_id = machine.config.hw_sku.clone().unwrap();

        // Assign a mismatched SKU
        assign_mismatched_sku(&mut txn, &machine_id, &current_sku_id).await?;

        // Trigger re-verification
        db::machine::update_sku_status_verify_request_time(&mut txn, &machine_id).await?;
        txn.commit().await?;

        handle_inventory_update(&pool, &env, &mh).await;

        // Run state controller to process SKU verification
        env.run_machine_state_controller_iteration().await;
        env.run_machine_state_controller_iteration().await;

        let mut txn = pool.begin().await?;
        let machine = get_machine_by_id(&mut txn, &machine_id).await?;

        // With prevent_allocation=true, should stay in SkuVerificationFailed
        assert!(matches!(
            machine.current_state(),
            ManagedHostState::BomValidating {
                bom_validating_state: BomValidating::SkuVerificationFailed(_)
            }
        ));

        Ok(())
    }

    /// Test 5.1a: Assigned SKU verification failure with ignore_unassigned_machines=true
    /// Conditions:
    /// - ignore_unassigned_machines = true
    /// - allow_allocation_on_validation_failure = false
    /// - machine has an assigned SKU that does not match hardware
    /// Expected: machine enters SkuVerificationFailed and remains unallocatable
    #[crate::sqlx_test]
    async fn test_ignore_unassigned_machines_does_not_bypass_assigned_sku_verification(
        pool: sqlx::PgPool,
    ) -> Result<(), eyre::Error> {
        let env = create_test_env_ignoring_unassigned_machines(pool.clone()).await;

        let mh = create_managed_host(&env).await;
        let machine_id = mh.host().id;

        let mut txn = pool.begin().await?;
        let current_sku = db::sku::generate_sku_from_machine(txn.as_mut(), &machine_id).await?;
        db::sku::create(&mut txn, &current_sku).await?;
        assign_mismatched_sku(&mut txn, &machine_id, &current_sku.id).await?;
        db::machine::update_sku_status_verify_request_time(&mut txn, &machine_id).await?;
        txn.commit().await?;

        handle_inventory_update(&pool, &env, &mh).await;
        env.run_machine_state_controller_iteration().await;
        env.run_machine_state_controller_iteration().await;

        let mut txn = pool.begin().await?;
        let machine = get_machine_by_id(&mut txn, &machine_id).await?;
        assert!(matches!(
            machine.current_state(),
            ManagedHostState::BomValidating {
                bom_validating_state: BomValidating::SkuVerificationFailed(_)
            }
        ));

        Ok(())
    }

    /// Test 5.2: SKU replaced but no re-verification triggered → continues to Ready
    /// Conditions:
    /// - allow_allocation_on_validation_failure = false (standard mode)
    /// - auto_generate_missing_sku = * (doesn't matter - SKU exists, just mismatched)
    /// - machine is progressing to Ready state, SKU is replaced with mismatched one in DB
    /// - No explicit re-verification is triggered (no call to update_sku_status_verify_request_time)
    /// Expected: machine continues to Ready (passive SKU data changes don't trigger validation or block state transitions)
    /// Key insight: SKU validation is explicit/on-demand, not triggered by data modifications
    #[crate::sqlx_test]
    async fn test_continues_to_ready_when_verification_fails(
        pool: sqlx::PgPool,
    ) -> Result<(), eyre::Error> {
        let env = create_test_env_for_bom_validation(pool.clone(), false, None, false).await;

        let mh = create_managed_host(&env).await;
        let machine_id = mh.host().id;

        let mut txn = pool.begin().await?;
        let machine = mh.host().db_machine(&mut txn).await;
        let current_sku_id = machine.config.hw_sku.clone().unwrap();

        // Assign a mismatched SKU
        assign_mismatched_sku(&mut txn, &machine_id, &current_sku_id).await?;
        txn.commit().await?;

        // Run state controller without triggering re-verification - machine should continue to Ready
        env.run_machine_state_controller_iteration().await;
        env.run_machine_state_controller_iteration_until_state_matches(
            &machine_id,
            20,
            ManagedHostState::Ready,
        )
        .await;

        let mut txn = pool.begin().await?;
        let machine = get_machine_by_id(&mut txn, &machine_id).await?;

        // Should reach Ready state despite SKU mismatch
        assert_eq!(machine.current_state(), &ManagedHostState::Ready);

        Ok(())
    }

    /// Test 5.3: SkuVerificationFailed → Escapes to MachineValidation with allow_allocation_on_validation_failure=true
    /// Conditions:
    /// - allow_allocation_on_validation_failure = true (allow machines to escape from failed states)
    /// - auto_generate_missing_sku = false
    /// - machine has assigned SKU but verification would fail (hardware mismatch)
    /// Expected: Machine escapes any potential SkuVerificationFailed state and proceeds directly to Ready
    /// Note: With allow_allocation_on_validation_failure=true, the machine never actually stays in
    /// SkuVerificationFailed state—it escapes immediately, so we just verify it reaches Ready.
    #[crate::sqlx_test]
    async fn test_allow_allocation_escapes_from_sku_failed(
        pool: sqlx::PgPool,
    ) -> Result<(), eyre::Error> {
        let env = create_test_env_for_bom_validation(pool.clone(), true, None, false).await;

        // Create a SKU that will mismatch with the actual hardware
        let mut txn = pool.begin().await?;
        let mismatched_sku: Sku = serde_json::from_str(SKU_DATA)?;
        db::sku::create(&mut txn, &mismatched_sku).await?;
        txn.commit().await?;

        // Create machine with the mismatched SKU
        let managed_host_config = ManagedHostConfig::default();
        let mut txn = pool.begin().await?;
        db::expected_machine::create(
            &mut txn,
            ExpectedMachine {
                id: None,
                bmc_mac_address: managed_host_config.bmc_mac_address,
                data: ExpectedMachineData {
                    bmc_username: "admin".to_string(),
                    bmc_password: "password".to_string(),
                    serial_number: "1234567890".to_string(),
                    fallback_dpu_serial_numbers: vec![],
                    metadata: Metadata::new_with_default_name(),
                    sku_id: Some(mismatched_sku.id.clone()),
                    default_pause_ingestion_and_poweron: None,
                    interfaces: vec![],
                    rack_id: None,
                    dpf_enabled: Some(true),
                    bmc_ip_address: None,
                    bmc_retain_credentials: None,
                    dpu_policy: Default::default(),
                    bmc_ip_allocation: Default::default(),
                    host_lifecycle_profile: Default::default(),
                },
            },
        )
        .await?;
        txn.commit().await?;

        let mh = create_managed_host_with_config(&env, managed_host_config).await;
        let machine_id = mh.host().id;

        // With allow_allocation_on_validation_failure=true, the machine should reach Ready state
        // even though the SKU doesn't match the hardware (it would have failed verification
        // if allow_allocation_on_validation_failure was false)
        let mut txn = pool.begin().await?;
        let machine = get_machine_by_id(&mut txn, &machine_id).await?;
        assert_eq!(machine.current_state(), &ManagedHostState::Ready);
        txn.commit().await?;

        Ok(())
    }

    /// Test 6.1: Auto-matching SKU for second machine
    /// Conditions:
    /// - allow_allocation_on_validation_failure = false (testing standard state machine flow from WaitingForSkuAssignment)
    /// - auto_generate_missing_sku = false
    /// - first machine has SKU assigned
    /// - second machine has identical hardware
    /// Expected: second machine automatically gets assigned the same SKU
    #[crate::sqlx_test]
    async fn test_auto_match_sku(pool: sqlx::PgPool) -> Result<(), eyre::Error> {
        let env = create_test_env_for_bom_validation(pool.clone(), false, None, false).await;
        let managed_host_config =
            ManagedHostConfig::default().with_expected_state(ManagedHostState::BomValidating {
                bom_validating_state: BomValidating::WaitingForSkuAssignment(
                    BomValidatingContext {
                        machine_validation_context: Some(MachineValidationContext::Discovery),
                        ..BomValidatingContext::default()
                    },
                ),
            });

        let mh = create_managed_host_with_config(&env, managed_host_config).await;
        let machine_id = mh.host().id;

        let mut txn = pool.begin().await?;

        let actual_sku = db::sku::generate_sku_from_machine(txn.as_mut(), &machine_id).await?;
        db::sku::create(&mut txn, &actual_sku).await?;

        db::machine::assign_sku(&mut txn, &machine_id, &actual_sku.id).await?;

        txn.commit().await?;

        // once the sku is assigned, the state machine should move to update inventory before it verifies it.
        env.run_machine_state_controller_iteration().await;

        let mut txn = pool.begin().await?;
        let machine = mh.host().db_machine(&mut txn).await;
        assert!(matches!(
            machine.current_state(),
            ManagedHostState::BomValidating {
                bom_validating_state: BomValidating::UpdatingInventory(_)
            }
        ));

        let expected_sku_id = machine.config.hw_sku.unwrap();

        // A new machine with the same hardware is automatically assigned the above
        // sku and moves on.

        let mh2 = create_managed_host(&env).await;

        let machine2 = mh2.host().db_machine(&mut txn).await;

        assert_eq!(machine2.config.hw_sku, Some(expected_sku_id));

        Ok(())
    }

    /// Test 6.2: SKU replacement triggers re-verification
    /// Conditions:
    /// - allow_allocation_on_validation_failure = false (standard mode)
    /// - auto_generate_missing_sku = false
    /// - machine has assigned SKU
    /// - SKU components are replaced
    /// Expected: verify_request_time is updated, triggering re-verification
    #[crate::sqlx_test]
    async fn test_replace_triggers_verify(pool: sqlx::PgPool) -> Result<(), eyre::Error> {
        let env = create_test_env_for_bom_validation(pool.clone(), false, None, false).await;
        let (machine_id, _dpu_id) = create_managed_host(&env).await.into();
        let mut txn = pool.begin().await?;

        // The SKU should have been auto-created and assigned by create_managed_host when BOM validation is enabled
        let machine = db::machine::find(
            txn.as_mut(),
            ObjectFilter::One(machine_id),
            MachineSearchConfig::default(),
        )
        .await?
        .pop()
        .unwrap();

        let sku_id = machine
            .config
            .hw_sku
            .clone()
            .expect("SKU should have been assigned");
        let mut actual_sku = db::sku::find(&mut txn, std::slice::from_ref(&sku_id))
            .await?
            .pop()
            .expect("SKU should exist");

        let original_verify_time = machine.status.hw_sku.map(|s| s.verify_request_time);
        assert_eq!(machine.config.hw_sku.unwrap(), actual_sku.id);

        txn.commit().await?;
        let mut txn = pool.begin().await?;

        actual_sku.components.cpus[0].thread_count *= 2;
        db::sku::replace(&mut txn, &actual_sku).await?;

        txn.commit().await?;

        let machine = db::machine::find(
            &pool,
            ObjectFilter::One(machine_id),
            MachineSearchConfig::default(),
        )
        .await?
        .pop()
        .unwrap();

        let replace_verify_time = machine.status.hw_sku.map(|s| s.verify_request_time);

        assert_ne!(original_verify_time, replace_verify_time);

        env.run_machine_state_controller_iteration().await;

        let machine = db::machine::find(
            &pool,
            ObjectFilter::One(machine_id),
            MachineSearchConfig::default(),
        )
        .await?
        .pop()
        .unwrap();

        assert!(matches!(
            machine.current_state(),
            ManagedHostState::BomValidating { .. }
        ));

        Ok(())
    }

    /// Test 6.3: Auto-matching SKU after unassign with retained SKU status
    /// Conditions:
    /// - allow_allocation_on_validation_failure = false (standard mode)
    /// - auto_generate_missing_sku = false
    /// - Machine reaches Ready with auto-matched SKU
    /// - SKU is unassigned while hw_sku_status retains a verification request
    /// Expected: Machine re-enters BOM validation flow and successfully re-matches the same SKU
    #[crate::sqlx_test]
    async fn test_auto_match_after_unassign(pool: sqlx::PgPool) -> Result<(), eyre::Error> {
        let env = create_test_env_for_bom_validation(
            pool.clone(),
            false,
            Some(Duration::from_secs(10)),
            false,
        )
        .await;

        let (machine_id, _dpu_id) = create_managed_host(&env).await.into();

        let mut txn = pool.begin().await?;
        let machine = db::machine::find(
            txn.as_mut(),
            ObjectFilter::One(machine_id),
            MachineSearchConfig::default(),
        )
        .await?
        .pop()
        .unwrap();

        assert_eq!(machine.current_state(), &ManagedHostState::Ready);

        // The SKU should have been auto-created by create_managed_host when BOM validation is enabled
        let expected_sku_id = machine
            .config
            .hw_sku
            .clone()
            .expect("SKU should have been assigned");
        let expected_sku = db::sku::find(&mut txn, std::slice::from_ref(&expected_sku_id))
            .await?
            .pop()
            .expect("SKU should exist");

        txn.commit().await?;

        // A new machine with the same hardware is automatically assigned the above
        // sku and moves on.

        let (machine_id, _dpu_id) = create_managed_host(&env).await.into();

        let mut txn = pool.begin().await?;

        let machine = db::machine::find(
            txn.as_mut(),
            ObjectFilter::One(machine_id),
            MachineSearchConfig::default(),
        )
        .await?
        .pop()
        .unwrap();

        assert_eq!(machine.config.hw_sku, Some(expected_sku.id.clone()));
        assert_eq!(machine.current_state(), &ManagedHostState::Ready);

        db::machine::update_sku_status_verify_request_time(&mut txn, &machine_id).await?;
        db::machine::unassign_sku(&mut txn, &machine_id).await?;
        txn.commit().await?;

        // Wait for machine to leave Ready and enter UpdatingInventory
        env.run_machine_state_controller_iteration_until_state_condition(
            &machine_id,
            10,
            |machine| {
                matches!(
                    machine.current_state(),
                    ManagedHostState::BomValidating {
                        bom_validating_state: BomValidating::UpdatingInventory(_)
                    }
                )
            },
        )
        .await;

        // Small delay to ensure discovery time will be after state transition timestamp
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

        // Update discovery time to satisfy the inventory freshness check
        let mut txn = pool.begin().await.unwrap();
        db::machine::update_discovery_time(&machine_id, &mut txn)
            .await
            .unwrap();
        txn.commit().await.unwrap();

        // Now machine should progress through MatchingSku -> VerifyingSku -> Ready
        env.run_machine_state_controller_iteration_until_state_condition(
            &machine_id,
            10,
            |machine| machine.current_state() == &ManagedHostState::Ready,
        )
        .await;

        let machine = db::machine::find(
            &pool,
            ObjectFilter::One(machine_id),
            MachineSearchConfig::default(),
        )
        .await?
        .pop()
        .unwrap();

        assert_eq!(machine.config.hw_sku, Some(expected_sku.id));
        assert_eq!(machine.current_state(), &ManagedHostState::Ready);

        Ok(())
    }

    /// Test 7.1: SKU Version Compatibility
    /// Conditions:
    /// - allow_allocation_on_validation_failure = false (testing standard state machine flow from SkuMissing state)
    /// - auto_generate_missing_sku = false
    /// - tests multiple SKU schema versions
    /// Expected: SKU version compatibility and backward compatibility work correctly
    #[crate::sqlx_test]
    async fn test_match_all_sku_versions(pool: sqlx::PgPool) -> Result<(), eyre::Error> {
        let env = create_test_env_for_bom_validation(pool.clone(), false, None, false).await;
        for old_sku_version in 0..CURRENT_SKU_VERSION {
            test_match_sku_versions(&env, old_sku_version).await?;
        }
        Ok(())
    }

    #[test]
    fn test_thread_differences() -> Result<(), eyre::Error> {
        let rpc_sku1: rpc::forge::Sku = serde_json::de::from_str(FULL_SKU_DATA)?;
        let mut rpc_sku2: rpc::forge::Sku = serde_json::de::from_str(FULL_SKU_DATA)?;

        let sku1 = rpc_sku1.into();
        let sku2 = rpc_sku2.clone().into();

        let diffs = model::sku::diff_skus(&sku1, &sku2);
        assert!(diffs.is_empty());

        rpc_sku2.components.as_mut().unwrap().cpus[0].thread_count *= 2;
        let sku2 = rpc_sku2.into();

        let diffs = model::sku::diff_skus(&sku1, &sku2);
        assert!(!diffs.is_empty());

        Ok(())
    }

    #[crate::sqlx_test]
    async fn test_find_machine_ids_by_sku_ids(pool: sqlx::PgPool) -> Result<(), eyre::Error> {
        let env = create_test_env_for_bom_validation(pool.clone(), false, None, false).await;
        let (machine_id, _dpu_id) = create_managed_host(&env).await.into();

        let machine = db::machine::find(
            &pool,
            ObjectFilter::One(machine_id),
            MachineSearchConfig::default(),
        )
        .await?
        .pop()
        .unwrap();
        let assigned_sku_id = machine.config.hw_sku.unwrap();
        // Make a new sku, using the assigned sku as a base and renaming it "unassigned-sku" (and
        // bump its cpu count.)
        let unassigned_sku = pool
            .with_txn({
                let assigned_sku_id = assigned_sku_id.clone();
                |txn| {
                    async move {
                        let mut unassigned_sku =
                            db::sku::find(txn, std::slice::from_ref(&assigned_sku_id))
                                .await?
                                .remove(0);
                        unassigned_sku.id = "unassigned-sku".to_string();
                        unassigned_sku.components.cpus[0].count += 1;
                        db::sku::create(txn, &unassigned_sku).await?;
                        Ok::<_, DatabaseError>(unassigned_sku)
                    }
                    .boxed()
                }
            })
            .await??;

        let sku_ids = vec![assigned_sku_id.clone(), unassigned_sku.id.clone()];
        let machine_ids_by_sku_ids =
            db::machine::find_machine_ids_by_sku_ids(&pool, &sku_ids).await?;

        assert_eq!(machine_ids_by_sku_ids.len(), 2);

        let assigned_sku_machine_ids = machine_ids_by_sku_ids.get(&assigned_sku_id).unwrap();
        assert_eq!(assigned_sku_machine_ids.len(), 1);
        assert!(assigned_sku_machine_ids.contains(&machine_id));

        let unassigned_sku_machine_ids = machine_ids_by_sku_ids.get(&unassigned_sku.id).unwrap();
        assert!(unassigned_sku_machine_ids.is_empty());

        Ok(())
    }

    #[crate::sqlx_test(fixtures("create_sku"))]
    pub(in crate::tests) fn test_sku_metadata_update(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut txn = pool.begin().await?;

        db::sku::update_metadata(
            &mut txn,
            "sku1".to_string(),
            Some("new description".to_string()),
            Some("fancy device".to_string()),
        )
        .await?;

        let sku = db::sku::find(&mut txn, &["sku1".to_string()])
            .await?
            .pop()
            .unwrap();

        assert_eq!(&sku.description, "new description");
        assert_eq!(&sku.device_type.unwrap(), "fancy device");

        Ok(())
    }

    #[crate::sqlx_test(fixtures("create_sku"))]
    pub(in crate::tests) fn test_sku_metadata_update_description(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut txn = pool.begin().await?;

        db::sku::update_metadata(
            &mut txn,
            "sku1".to_string(),
            Some("new description".to_string()),
            None,
        )
        .await?;

        let sku = db::sku::find(&mut txn, &["sku1".to_string()])
            .await?
            .pop()
            .unwrap();

        assert_eq!(&sku.description, "new description");
        assert_eq!(sku.device_type, Some("device_type".to_string()));

        db::sku::update_metadata(
            &mut txn,
            "sku1".to_string(),
            Some("old description".to_string()),
            Some("old device".to_string()),
        )
        .await?;

        db::sku::update_metadata(
            &mut txn,
            "sku1".to_string(),
            Some("really new description".to_string()),
            None,
        )
        .await?;

        let sku = db::sku::find(&mut txn, &["sku1".to_string()])
            .await?
            .pop()
            .unwrap();

        assert_eq!(&sku.description, "really new description");
        assert_eq!(&sku.device_type.unwrap(), "old device");

        Ok(())
    }

    #[crate::sqlx_test(fixtures("create_sku"))]
    pub(in crate::tests) fn test_sku_metadata_update_device_type(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut txn = pool.begin().await?;

        db::sku::update_metadata(
            &mut txn,
            "sku1".to_string(),
            None,
            Some("new device type".to_string()),
        )
        .await?;

        let sku = db::sku::find(&mut txn, &["sku1".to_string()])
            .await?
            .pop()
            .unwrap();

        assert_eq!(&sku.description, "test description");
        assert_eq!(&sku.device_type.unwrap(), "new device type");

        Ok(())
    }

    #[crate::sqlx_test(fixtures("create_sku"))]
    pub(in crate::tests) fn test_sku_metadata_update_invalid(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut txn = pool.begin().await?;

        let result = db::sku::update_metadata(&mut txn, "sku1".to_string(), None, None).await;

        assert!(result.is_err());
        Ok(())
    }

    #[crate::sqlx_test(fixtures("create_sku"))]
    pub(in crate::tests) fn test_sku_replace(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut txn = pool.begin().await?;
        let sku_id = "sku1".to_string();
        let original_sku = db::sku::find(&mut txn, std::slice::from_ref(&sku_id))
            .await?
            .remove(0);
        let original_sku_json = serde_json::ser::to_string_pretty(&original_sku)?;
        tracing::info!(original_sku_json, "original");

        let rpc_sku: rpc::forge::Sku = serde_json::de::from_str(FULL_SKU_DATA)?;
        let replacement_sku: Sku = rpc_sku.into();
        let replacement_sku_json = serde_json::ser::to_string_pretty(&replacement_sku)?;
        tracing::info!(replacement_sku_json, "replacment");

        let expected_sku = Sku {
            schema_version: CURRENT_SKU_VERSION,
            components: replacement_sku.components,
            ..original_sku
        };
        let expected_sku_json = serde_json::ser::to_string_pretty(&expected_sku)?;

        let returned_sku = db::sku::replace(&mut txn, &expected_sku).await?;

        let returned_sku_json = serde_json::ser::to_string_pretty(&returned_sku)?;

        let mut actual_sku = db::sku::find(&mut txn, &[sku_id]).await?.remove(0);

        // cheat the created timestamp
        actual_sku.created = expected_sku.created;

        let actual_sku_json = serde_json::ser::to_string_pretty(&actual_sku)?;

        assert_eq!(actual_sku_json, returned_sku_json);
        assert_eq!(actual_sku_json, expected_sku_json);

        Ok(())
    }

    #[crate::sqlx_test]
    async fn test_delete_sku_in_use_reports_correct_machine_count(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use rpc::forge::SkuIdList;
        use rpc::forge::forge_server::Forge;

        let env = create_test_env(pool.clone()).await;
        let (machine_id_1, _dpu1) = create_managed_host(&env).await.into();
        let (machine_id_2, _dpu2) = create_managed_host(&env).await.into();

        let sku_id = {
            let mut txn = pool.begin().await?;
            let sku = db::sku::generate_sku_from_machine(txn.as_mut(), &machine_id_1).await?;
            db::sku::create(&mut txn, &sku).await?;
            db::machine::assign_sku(&mut txn, &machine_id_1, &sku.id).await?;
            db::machine::assign_sku(&mut txn, &machine_id_2, &sku.id).await?;
            txn.commit().await?;
            sku.id
        };

        let err = env
            .api
            .delete_sku(tonic::Request::new(SkuIdList { ids: vec![sku_id] }))
            .await
            .expect_err("expected delete of in-use SKU to fail");

        assert_eq!(err.code(), tonic::Code::InvalidArgument);
        assert!(
            err.message().contains("in use by 2 machines"),
            "expected error to report 2 machines but got: {}",
            err.message()
        );

        Ok(())
    }
}
