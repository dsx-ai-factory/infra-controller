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

//! Selects DPF deployments and checks compatibility before DPU reprovisioning.

use std::collections::HashSet;
use std::time::Duration;

use carbide_dpf::{DpuDeploymentType, dpu_node_cr_name};
use carbide_libmlx_model::nvconfig::DpuNvConfigProfile;
use carbide_uuid::machine::DpuMachineId;
use model::expected_machine::ExpectedMachine;
use model::hardware_info::HardwareInfo;
use model::machine::ManagedHostStateSnapshot;
use model::rack_type::{RackProductFamily, RackProfileConfig, select_dpu_nvconfig_profile};
use sqlx::pool::PoolConnection;
use sqlx::{PgPool, Postgres};
use state_controller::state_handler::{ExternalServiceError, StateHandlerError};

use super::DpfOperations;

// `deployment_type_for_dpu_profile` refines exactly one deployment:
// generic BF3 becomes GB200 BF3. Only that forward migration is supported.
pub(crate) const DEPLOYMENT_MIGRATION_SOURCE: DpuDeploymentType = DpuDeploymentType::Bf3;
pub(crate) const DEPLOYMENT_MIGRATION_TARGET: DpuDeploymentType = DpuDeploymentType::Bf3Gb200;

// Ready checks have this whole-attempt budget, including pool waits. Automatic
// admission additionally shares one deadline across its candidate pass.
const REPROVISION_COMPATIBILITY_TIMEOUT: Duration = Duration::from_secs(30);

/// SQLx can retain a pool slot while a cancelled query drains. Discarding an
/// unsuccessful read frees that slot; the transaction-local statement cap
/// separately bounds PostgreSQL work after setup.
struct DeploymentReadConnection {
    connection: Option<PoolConnection<Postgres>>,
}

impl DeploymentReadConnection {
    async fn acquire(pool: &PgPool) -> Result<Self, sqlx::Error> {
        Ok(Self {
            connection: Some(pool.acquire().await?),
        })
    }

    async fn begin(&mut self) -> Result<db::Transaction<'_>, db::DatabaseError> {
        self.begin_with_timeout(REPROVISION_COMPATIBILITY_TIMEOUT)
            .await
    }

    async fn begin_with_timeout(
        &mut self,
        statement_timeout: Duration,
    ) -> Result<db::Transaction<'_>, db::DatabaseError> {
        let connection = self
            .connection
            .as_deref_mut()
            .expect("the connection is retained until return_to_pool consumes the guard");
        let mut txn = db::Transaction::begin_inner(connection).await?;
        // Zero disables the operator timeout; otherwise preserve a shorter
        // value. Transaction scope restores the setting for the next borrower.
        let query = "SELECT set_config('statement_timeout', \
            LEAST(NULLIF(setting::bigint, 0), $1::text::bigint)::text, true) \
            FROM pg_settings WHERE name = 'statement_timeout'";
        sqlx::query(query)
            .bind(statement_timeout.as_millis().to_string())
            .execute(txn.as_pgconn())
            .await
            .map_err(|error| db::DatabaseError::query(query, error))?;
        Ok(txn)
    }

    fn return_to_pool(mut self) {
        drop(self.connection.take());
    }
}

impl Drop for DeploymentReadConnection {
    fn drop(&mut self) {
        if let Some(connection) = self.connection.take() {
            // Detach releases local capacity immediately. The server may retain
            // this session until its bounded statement finishes or times out.
            drop(connection.detach());
        }
    }
}

/// `validate_reprovision_compatibility` checks deployment selection and
/// `DPUNode` labels before DPF reprovisioning.
///
/// Returns an error for a missing host BMC MAC or DPU DMI product name,
/// incompatible deployments or labels, or failed database/DPF reads.
/// The complete check has a 30-second deadline, including database pool waits;
/// expiry returns an error and cancelled database reads release their pool slot.
/// Metadata statements also have a transaction-local server limit of at most
/// 30 seconds, preserving shorter operator limits; this excludes setup.
/// A missing `expected_machines` declaration is allowed. Callers can defer
/// before recording a request or restarting a DPU.
///
/// Migrating a generic BF3 node to `Bf3Gb200` requires the same nonempty set of
/// DPU IDs in `requested_dpus`, the host's attachments, and `state.dpu_snapshots`.
/// A missing `DPUNode` passes the SDK label check; this function does not
/// validate the complete DPF resource set or target deployment readiness.
/// No DPF resources are changed.
pub async fn validate_reprovision_compatibility(
    state: &ManagedHostStateSnapshot,
    requested_dpus: &[DpuMachineId],
    pool: &PgPool,
    rack_profiles: &RackProfileConfig,
    dpf_sdk: &dyn DpfOperations,
) -> Result<(), StateHandlerError> {
    let check = async {
        let expected_machine = expected_machine_for_host(state, pool).await?;
        let deployment_types = deployment_types_for_host(
            state,
            pool,
            rack_profiles,
            dpf_sdk,
            expected_machine.as_ref(),
        )
        .await?;
        let deployment_type = consistent_deployment_type(&deployment_types).map_err(|error| {
            StateHandlerError::InvalidState(format!("{error}: {deployment_types:?}"))
        })?;
        let node_id =
            state
                .host_snapshot
                .dpf_id()
                .ok_or_else(|| StateHandlerError::MissingData {
                    object_id: state.host_snapshot.id.to_string(),
                    missing: "bmc_mac_address",
                })?;
        let node_name = dpu_node_cr_name(&node_id);
        let verify_labels = async |deployment_type| {
            dpf_sdk
                .verify_node_labels(&node_name, deployment_type)
                .await
                .map_err(|error| {
                    ExternalServiceError::with_source(
                        "dpf",
                        "verify_node_labels",
                        format!("cannot verify DPUNode {node_name} {deployment_type:?} deployment labels: {error}"),
                        "dpf_error",
                        error,
                    )
                })
        };
        if verify_labels(deployment_type).await? {
            return Ok(());
        }

        if deployment_type == DEPLOYMENT_MIGRATION_TARGET
            && verify_labels(DEPLOYMENT_MIGRATION_SOURCE).await?
        {
            let requested_dpus = requested_dpus.iter().copied().collect::<HashSet<_>>();
            let attached_dpus = state
                .host_snapshot
                .associated_dpu_machine_ids()
                .into_iter()
                .collect::<HashSet<_>>();
            let snapshot_dpus = state
                .dpu_snapshots
                .iter()
                .map(|dpu| dpu.id)
                .collect::<HashSet<_>>();
            if !requested_dpus.is_empty()
                && requested_dpus == attached_dpus
                && requested_dpus == snapshot_dpus
            {
                return Ok(());
            }
            return Err(StateHandlerError::InvalidState(format!(
                "DPUNode {node_name} requires a complete host request to migrate from \
                 {DEPLOYMENT_MIGRATION_SOURCE:?} to {deployment_type:?}"
            )));
        }

        Err(StateHandlerError::InvalidState(format!(
            "DPUNode {node_name} labels do not match the selected {deployment_type:?} deployment; \
             reconcile expected hardware inventory and deployment selector labels before reprovisioning"
        )))
    };

    tokio::time::timeout(REPROVISION_COMPATIBILITY_TIMEOUT, check)
        .await
        .map_err(|error| {
            StateHandlerError::GenericError(eyre::Report::new(error).wrap_err(format!(
                "DPF reprovision compatibility check timed out after {} seconds",
                REPROVISION_COMPATIBILITY_TIMEOUT.as_secs()
            )))
        })?
}

/// `expected_machine_for_host` looks up expected inventory using the host's
/// BMC MAC address.
///
/// Returns `None` when no matching declaration exists. Missing BMC identity
/// and database errors are returned to the caller.
pub(crate) async fn expected_machine_for_host(
    mh_snapshot: &ManagedHostStateSnapshot,
    pool: &PgPool,
) -> Result<Option<ExpectedMachine>, StateHandlerError> {
    let Some(bmc_mac_address) = mh_snapshot.host_snapshot.status.bmc_info.mac else {
        tracing::error!(
            machine_id = %mh_snapshot.host_snapshot.id,
            "Cannot look up expected inventory without the host BMC MAC address"
        );
        return Err(StateHandlerError::MissingData {
            object_id: mh_snapshot.host_snapshot.id.to_string(),
            missing: "bmc_mac_address",
        });
    };

    let mut conn = DeploymentReadConnection::acquire(pool).await?;
    let mut txn = conn.begin().await?;

    let expected_machine =
        db::expected_machine::find_by_bmc_mac_address(txn.as_pgconn(), bmc_mac_address)
            .await
            .map_err(|err| {
                tracing::error!(
                    machine_id = %mh_snapshot.host_snapshot.id,
                    %bmc_mac_address,
                    error = %err,
                    "Failed to look up expected inventory"
                );
                StateHandlerError::DBError(Box::new(err))
            })?;

    txn.commit().await?;
    conn.return_to_pool();

    Ok(expected_machine)
}

/// Returns the product family reported by Site Explorer, or falls back to the
/// configured rack profile when that report does not name a known family.
async fn host_product_family(
    state: &ManagedHostStateSnapshot,
    pool: &PgPool,
    rack_profiles: &RackProfileConfig,
) -> Result<Option<RackProductFamily>, StateHandlerError> {
    let mut conn = DeploymentReadConnection::acquire(pool).await?;
    let mut txn = conn.begin().await?;

    if let Some(host_bmc_ip) = state.host_snapshot.status.bmc_info.ip {
        let endpoints =
            db::explored_endpoints::find_by_ips(txn.as_pgconn(), vec![host_bmc_ip]).await?;
        if let Some(product_family) = endpoints
            .first()
            .and_then(|endpoint| endpoint.report.model())
            .as_deref()
            .and_then(RackProductFamily::from_hardware_model)
        {
            txn.commit().await?;
            conn.return_to_pool();
            return Ok(Some(product_family));
        }
    }

    let Some(rack_id) = state.host_snapshot.rack_id.as_ref() else {
        txn.commit().await?;
        conn.return_to_pool();
        return Ok(None);
    };
    let rack = db::rack::find_by(
        txn.as_pgconn(),
        db::ObjectColumnFilter::One(db::rack::IdColumn, rack_id),
    )
    .await?
    .pop();
    txn.commit().await?;
    conn.return_to_pool();
    let Some(rack) = rack else {
        return Ok(None);
    };
    let Some(profile_id) = rack.rack_profile_id.as_ref() else {
        return Ok(None);
    };
    let Some(profile) = rack_profiles.get(profile_id.as_str()) else {
        return Ok(None);
    };

    Ok(profile.product_family.clone())
}

/// `deployment_types_for_host` returns one deployment type per entry in
/// `state.dpu_snapshots`, preserving their order. Expected `CX9` inventory
/// selects Astra for BF4 before discovered DPA interfaces are available.
pub(crate) async fn deployment_types_for_host(
    state: &ManagedHostStateSnapshot,
    pool: &PgPool,
    rack_profiles: &RackProfileConfig,
    dpf_sdk: &dyn DpfOperations,
    expected_machine: Option<&ExpectedMachine>,
) -> Result<Vec<DpuDeploymentType>, StateHandlerError> {
    let product_family = host_product_family(state, pool, rack_profiles).await?;
    let astra_nics = expected_machine.is_some_and(|machine| {
        machine
            .data
            .interfaces
            .iter()
            .any(|nic| nic.nic_type.as_deref() == Some("CX9"))
    });
    state
        .dpu_snapshots
        .iter()
        .map(|dpu| {
            let base = dpf_sdk
                .deployment_type_for_dpu(dpu, astra_nics)
                .map_err(|error| {
                    ExternalServiceError::with_source(
                        "dpf",
                        "",
                        error.to_string(),
                        "dpf_error",
                        error,
                    )
                })?;
            Ok::<_, StateHandlerError>(deployment_type_for_dpu_profile(
                base,
                product_family.as_ref(),
                dpu.status.hardware_info.as_ref(),
            ))
        })
        .collect()
}

fn deployment_type_for_dpu_profile(
    base: DpuDeploymentType,
    product_family: Option<&RackProductFamily>,
    hardware_info: Option<&HardwareInfo>,
) -> DpuDeploymentType {
    match select_dpu_nvconfig_profile(product_family, hardware_info) {
        Some(DpuNvConfigProfile::Gb200B3240V1) if base == DpuDeploymentType::Bf3 => {
            DpuDeploymentType::Bf3Gb200
        }
        Some(DpuNvConfigProfile::Gb200B3240V1) | None => base,
    }
}

pub(crate) fn consistent_deployment_type(
    deployment_types: &[DpuDeploymentType],
) -> Result<DpuDeploymentType, String> {
    let Some(first) = deployment_types.first().copied() else {
        return Err(
            "cannot determine DPF deployment type for a host without attached DPUs".to_string(),
        );
    };
    if deployment_types.iter().all(|candidate| *candidate == first) {
        Ok(first)
    } else {
        Err("host has mixed DPF deployment types across attached DPUs".to_string())
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_harness::prelude::{sqlx_test, sqlx_testing};
    use carbide_test_support::{Check, check_values};
    use model::hardware_info::DpuData;

    use super::*;

    /// Check the production setting without waiting for it, then exercise the
    /// same SQL with a short cap. PostgreSQL must cancel the statement itself,
    /// including when the operator has disabled its default deadline.
    #[sqlx_test]
    async fn deployment_reads_bound_server_work_and_preserve_operator_limits(
        pool: PgPool,
    ) -> eyre::Result<()> {
        let mut connection = DeploymentReadConnection::acquire(&pool).await?;
        sqlx::query("SET statement_timeout = 0")
            .execute(connection.connection.as_deref_mut().unwrap())
            .await?;
        let mut txn = connection.begin().await?;
        let production_timeout: String = sqlx::query_scalar("SHOW statement_timeout")
            .fetch_one(txn.as_pgconn())
            .await?;
        assert_eq!(production_timeout, "30s");
        txn.commit().await?;
        let restored_timeout: String = sqlx::query_scalar("SHOW statement_timeout")
            .fetch_one(connection.connection.as_deref_mut().unwrap())
            .await?;
        assert_eq!(restored_timeout, "0");

        for (scenario, operator_timeout, expected_timeout) in [
            ("install a limit when disabled", "0", "250ms"),
            ("bound a longer operator limit", "5s", "250ms"),
            ("preserve a shorter operator limit", "100ms", "100ms"),
        ] {
            sqlx::query("SELECT set_config('statement_timeout', $1, false)")
                .bind(operator_timeout)
                .execute(connection.connection.as_deref_mut().unwrap())
                .await?;
            let mut txn = connection
                .begin_with_timeout(Duration::from_millis(250))
                .await?;
            let configured_timeout: String = sqlx::query_scalar("SHOW statement_timeout")
                .fetch_one(txn.as_pgconn())
                .await?;
            assert_eq!(configured_timeout, expected_timeout, "{scenario}");
            let error = tokio::time::timeout(
                Duration::from_secs(2),
                sqlx::query("SELECT pg_sleep(1)").execute(txn.as_pgconn()),
            )
            .await?
            .expect_err(scenario);
            assert_eq!(
                error
                    .as_database_error()
                    .and_then(|error| error.code())
                    .as_deref(),
                Some("57014"),
                "{scenario}: PostgreSQL must cancel the statement: {error}",
            );
            txn.rollback().await?;
            let restored_timeout: String = sqlx::query_scalar("SHOW statement_timeout")
                .fetch_one(connection.connection.as_deref_mut().unwrap())
                .await?;
            assert_eq!(restored_timeout, operator_timeout, "{scenario}");
        }
        connection.return_to_pool();
        Ok(())
    }

    fn hardware_info(part_number: &str) -> HardwareInfo {
        HardwareInfo {
            dpu_info: Some(DpuData {
                part_number: part_number.to_string(),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn gb200_b3240_profile_selects_only_the_specialized_bf3_deployment() {
        /// Hardware and rack inputs used to refine one base DPF deployment class.
        struct ProfileInput {
            base: DpuDeploymentType,
            product_family: Option<RackProductFamily>,
            hardware_info: Option<HardwareInfo>,
        }

        check_values(
            [
                Check {
                    scenario: "GB200 B3240 BF3",
                    input: ProfileInput {
                        base: DpuDeploymentType::Bf3,
                        product_family: Some(RackProductFamily::Gb200),
                        hardware_info: Some(hardware_info("900-9D3B6-00CN-AB0")),
                    },
                    expect: DpuDeploymentType::Bf3Gb200,
                },
                Check {
                    scenario: "non-GB200 B3240 BF3",
                    input: ProfileInput {
                        base: DpuDeploymentType::Bf3,
                        product_family: Some(RackProductFamily::Other("other".to_string())),
                        hardware_info: Some(hardware_info("900-9D3B6-00CN-AB0")),
                    },
                    expect: DpuDeploymentType::Bf3,
                },
                Check {
                    scenario: "GB200 other BF3",
                    input: ProfileInput {
                        base: DpuDeploymentType::Bf3,
                        product_family: Some(RackProductFamily::Gb200),
                        hardware_info: Some(hardware_info("900-9D3B6-00CV-AA0")),
                    },
                    expect: DpuDeploymentType::Bf3,
                },
                Check {
                    scenario: "BF3 without rack profile or hardware information",
                    input: ProfileInput {
                        base: DpuDeploymentType::Bf3,
                        product_family: None,
                        hardware_info: None,
                    },
                    expect: DpuDeploymentType::Bf3,
                },
                Check {
                    scenario: "GB200 BF4 remains BF4",
                    input: ProfileInput {
                        base: DpuDeploymentType::Bf4Generic,
                        product_family: Some(RackProductFamily::Gb200),
                        hardware_info: Some(hardware_info("900-9D3B6-00CN-AB0")),
                    },
                    expect: DpuDeploymentType::Bf4Generic,
                },
                Check {
                    scenario: "Astra remains Astra",
                    input: ProfileInput {
                        base: DpuDeploymentType::Bf4Astra,
                        product_family: Some(RackProductFamily::Gb200),
                        hardware_info: Some(hardware_info("900-9D3B6-00CN-AB0")),
                    },
                    expect: DpuDeploymentType::Bf4Astra,
                },
            ],
            |input| {
                deployment_type_for_dpu_profile(
                    input.base,
                    input.product_family.as_ref(),
                    input.hardware_info.as_ref(),
                )
            },
        );
    }

    #[test]
    fn attached_dpus_require_one_consistent_deployment_type() {
        check_values(
            [
                Check {
                    scenario: "ordinary BF3 pair",
                    input: vec![DpuDeploymentType::Bf3, DpuDeploymentType::Bf3],
                    expect: Some(DpuDeploymentType::Bf3),
                },
                Check {
                    scenario: "GB200 BF3 pair",
                    input: vec![DpuDeploymentType::Bf3Gb200, DpuDeploymentType::Bf3Gb200],
                    expect: Some(DpuDeploymentType::Bf3Gb200),
                },
                Check {
                    scenario: "mixed GB200 eligibility",
                    input: vec![DpuDeploymentType::Bf3Gb200, DpuDeploymentType::Bf3],
                    expect: None,
                },
                Check {
                    scenario: "host without attached DPUs",
                    input: vec![],
                    expect: None,
                },
            ],
            |deployment_types| consistent_deployment_type(&deployment_types).ok(),
        );
    }
}
