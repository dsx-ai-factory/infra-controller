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

use carbide_ib_fabric::errors::IbError;
use carbide_ib_fabric::ib::{
    GetPartitionOptions, IBFabric, IBFabricManager, IBFabricManagerConfig,
};
use carbide_instrument::{Event, LabelValue, emit};
use carbide_uuid::infiniband::IBPartitionId;
use model::ib::{DEFAULT_IB_FABRIC_NAME, IBQosConf};
use model::ib_partition::{IBPartition, IBPartitionControllerState, IBPartitionStatus};
use state_controller::state_handler::{
    StateHandler, StateHandlerContext, StateHandlerError, StateHandlerOutcome,
};

use crate::context::IBPartitionStateHandlerContextObjects;
use crate::ufm_error;

/// The controller states in which a persisted partition must already have a
/// pkey.
#[derive(Debug, Clone, Copy, PartialEq, Eq, LabelValue)]
enum MissingPkeyControllerState {
    Ready,
    Deleting,
}

/// An IB partition reached a controller state without a pkey. Each variant is
/// the state it was in.
#[derive(Event)]
#[event(
    event_name = "ib_partition_pkey_missing",
    metric_name = "carbide_ib_partition_pkey_missing_total",
    component = "ib-partition-controller",
    metric = counter,
    describe = "Number of IB partitions missing a pkey, by controller state",
    labels(controller_state: MissingPkeyControllerState),
)]
enum IbPartitionPkeyMissing {
    #[event(
        labels(controller_state = MissingPkeyControllerState::Ready),
        log = error,
        message = "IB partition has no pkey while ready"
    )]
    Ready {
        #[context]
        ib_partition_id: IBPartitionId,
        #[context]
        cause: String,
    },

    #[event(
        labels(controller_state = MissingPkeyControllerState::Deleting),
        log = error,
        message = "IB partition has no pkey while deleting"
    )]
    Deleting {
        #[context]
        ib_partition_id: IBPartitionId,
        #[context]
        cause: String,
    },
}
fn missing_pkey_transition(
    partition_id: IBPartitionId,
    controller_state: MissingPkeyControllerState,
) -> StateHandlerOutcome<IBPartitionControllerState> {
    let cause = match controller_state {
        MissingPkeyControllerState::Ready => "The pkey is None when IBPartition is ready",
        MissingPkeyControllerState::Deleting => "The pkey is None when deleting an IBPartition.",
    }
    .to_string();

    let (ib_partition_id, missing_cause) = (partition_id, cause.clone());
    emit(match controller_state {
        MissingPkeyControllerState::Ready => IbPartitionPkeyMissing::Ready {
            ib_partition_id,
            cause: missing_cause,
        },
        MissingPkeyControllerState::Deleting => IbPartitionPkeyMissing::Deleting {
            ib_partition_id,
            cause: missing_cause,
        },
    });

    StateHandlerOutcome::transition(IBPartitionControllerState::Error { cause })
}

/// The actual IBPartition State handler
#[derive(Debug, Default, Clone)]
pub struct IBPartitionStateHandler {}

#[async_trait::async_trait]
impl StateHandler for IBPartitionStateHandler {
    type ObjectId = IBPartitionId;
    type State = IBPartition;
    type ControllerState = IBPartitionControllerState;
    type ContextObjects = IBPartitionStateHandlerContextObjects;

    async fn handle_object_state(
        &self,
        partition_id: &IBPartitionId,
        state: &mut IBPartition,
        controller_state: &Self::ControllerState,
        ctx: &mut StateHandlerContext<Self::ContextObjects>,
    ) -> Result<StateHandlerOutcome<IBPartitionControllerState>, StateHandlerError> {
        match controller_state {
            IBPartitionControllerState::Provisioning => {
                // TODO(k82cn): get IB network from IB Fabric Manager to avoid duplication.
                let new_state = IBPartitionControllerState::Ready;
                Ok(StateHandlerOutcome::transition(new_state))
            }

            IBPartitionControllerState::Deleting => {
                match state.status.as_ref().and_then(|s| s.pkey) {
                    None => Ok(missing_pkey_transition(
                        *partition_id,
                        MissingPkeyControllerState::Deleting,
                    )),
                    Some(pkey) => {
                        let ib_fabric =
                            connect_ib_fabric(ctx.services.ib_fabric_manager.as_ref()).await?;

                        // When ib_partition is deleting, it should wait until all instances are
                        // released. As releasing instance will also remove ib_port from ib_network,
                        // and the ib_network will be removed when no ports are in it.
                        let res = ib_fabric
                            .get_ib_network(
                                pkey.into(),
                                GetPartitionOptions {
                                    include_guids_data: false,
                                    include_qos_conf: true,
                                },
                            )
                            .await;
                        if let Err(e) = res {
                            match e {
                                // The IBPartition maybe deleted during controller cycle.
                                IbError::NotFoundError { .. } => {
                                    // Before deleting, check if any instances still reference
                                    // this partition. This prevents deleting a partition that
                                    // instances still depend on, which would cause errors when
                                    // the instance state handler tries to unbind ports.
                                    let mut txn = ctx.services.db_pool.begin().await?;
                                    let instance_count =
                                        db::ib_partition::count_instances_referencing_partition(
                                            txn.as_mut(),
                                            *partition_id,
                                        )
                                        .await?;

                                    if instance_count > 0 {
                                        tracing::info!(
                                            ib_partition_id = %partition_id,
                                            instance_count,
                                            "Postponing IB partition deletion because instances still reference it",
                                        );
                                        return Ok(StateHandlerOutcome::wait(format!(
                                            "Waiting for {instance_count} instance(s) to release IB partition"
                                        ))
                                        .with_txn(txn));
                                    }

                                    // Release pkey after ib_partition deleted.
                                    let pkey_pool = ctx
                                        .services
                                        .ib_pools
                                        .pkey_pools
                                        .get(DEFAULT_IB_FABRIC_NAME)
                                        .ok_or_else(|| {
                                            let error = eyre::eyre!(
                                                "pkey pool for fabric \"{DEFAULT_IB_FABRIC_NAME}\" was not found"
                                            );
                                            ufm_error("release_pkey", error)
                                        })?;

                                    db::ib_partition::final_delete(*partition_id, &mut txn).await?;

                                    db::resource_pool::release(pkey_pool, &mut txn, pkey.into())
                                        .await?;
                                    Ok(StateHandlerOutcome::deleted().with_txn(txn))
                                }
                                _ => Err(ufm_error("get_ib_network", e.into())),
                            }
                        } else {
                            Ok(StateHandlerOutcome::wait(
                                "Waiting for all IB instances are released".to_string(),
                            ))
                        }
                    }
                }
            }

            IBPartitionControllerState::Ready => match state.status.as_ref().and_then(|s| s.pkey) {
                None => Ok(missing_pkey_transition(
                    *partition_id,
                    MissingPkeyControllerState::Ready,
                )),
                Some(pkey) => {
                    if state.is_marked_as_deleted() {
                        Ok(StateHandlerOutcome::transition(
                            IBPartitionControllerState::Deleting,
                        ))
                    } else {
                        let ib_fabric =
                            connect_ib_fabric(ctx.services.ib_fabric_manager.as_ref()).await?;
                        // The only arm that compares against the manager's QoS
                        // configuration, so the config deep-clone happens here
                        // rather than on every reconcile.
                        let ib_config = ctx.services.ib_fabric_manager.get_config();
                        let res = ib_fabric
                            .get_ib_network(
                                pkey.into(),
                                GetPartitionOptions {
                                    include_guids_data: false,
                                    include_qos_conf: true,
                                },
                            )
                            .await;

                        match res {
                            Ok(ibnetwork) => {
                                // If found the IBNetwork, update the status accordingly. And check
                                // it whether align with the config; if mismatched, return error.
                                // The mismatched status is still there in DB for debug.
                                // QoS data can be exected here, since the API call above queries for it
                                let qos = ibnetwork.qos_conf.as_ref().ok_or_else(|| {
                                    StateHandlerError::MissingData {
                                        object_id: partition_id.to_string(),
                                        missing: "qos_conf",
                                    }
                                })?;

                                state.status = Some(IBPartitionStatus {
                                    partition: Some(ibnetwork.name.clone()),
                                    pkey: Some(pkey), // We need the pkey to persist.
                                    mtu: Some(qos.mtu.clone()),
                                    rate_limit: Some(qos.rate_limit.clone()),
                                    service_level: Some(qos.service_level.clone()),
                                });

                                let ib_result = if !is_qos_conf_applied(&ib_config, qos) {
                                    // Update the QoS of IBNetwork in UFM.
                                    //
                                    // TODO(k82cn): Currently, the IBNetwork is created only after
                                    // at least one port was bound to the partition.
                                    // In latest version, the UFM will support create partition without
                                    // port.
                                    let desired_qos_conf = IBQosConf {
                                        mtu: ib_config.mtu.clone(),
                                        rate_limit: ib_config.rate_limit.clone(),
                                        service_level: ib_config.service_level.clone(),
                                    };

                                    ib_fabric
                                        .update_partition_qos_conf(
                                            ibnetwork.pkey,
                                            &desired_qos_conf,
                                        )
                                        .await
                                } else {
                                    Ok(())
                                };

                                let mut txn = ctx.services.db_pool.begin().await?;
                                db::ib_partition::update_status(state.id, &state.status, &mut txn)
                                    .await?;

                                if let Err(e) = ib_result {
                                    return Ok(StateHandlerOutcome::transition(
                                        IBPartitionControllerState::Error {
                                            cause: format!("Failed to update IB partition {e}"),
                                        },
                                    )
                                    .with_txn(txn));
                                } else {
                                    Ok(StateHandlerOutcome::do_nothing().with_txn(txn))
                                }
                            }

                            Err(e) => {
                                match e {
                                    // The Partition maybe still empty as it will be only created
                                    // when at least one port associated with the Partition.
                                    IbError::NotFoundError { .. } => {
                                        Ok(StateHandlerOutcome::do_nothing())
                                    }
                                    _ => Err(ufm_error("get_ib_network", e.into())),
                                }
                            }
                        }
                    }
                }
            },

            IBPartitionControllerState::Error { .. } => {
                if state.status.as_ref().and_then(|s| s.pkey).is_some()
                    && state.is_marked_as_deleted()
                {
                    Ok(StateHandlerOutcome::transition(
                        IBPartitionControllerState::Deleting,
                    ))
                } else {
                    // If pkey is none, keep it in error state.
                    Ok(StateHandlerOutcome::do_nothing())
                }
            }
        }
    }
}

/// Builds the UFM client for the state-handling arms that talk to the fabric
/// manager.
///
/// Building a client fetches credentials from the secret manager and sets up a
/// TLS-backed HTTP client, so it happens inside exactly the arms that query or
/// mutate the fabric; arms that resolve purely from Carbide state skip it.
async fn connect_ib_fabric(
    fabric_manager: &dyn IBFabricManager,
) -> Result<Arc<dyn IBFabric>, StateHandlerError> {
    fabric_manager
        .new_client(DEFAULT_IB_FABRIC_NAME)
        .await
        .map_err(|e| ufm_error("connect", e.into()))
}

fn is_qos_conf_applied(c: &IBFabricManagerConfig, actual_qos: &IBQosConf) -> bool {
    c.mtu == actual_qos.mtu
        // NOTE: The rate_limit is defined as 'f64' for lagency device, e.g. 2.5G; so it's ok to
        // convert to i32 for new devices.
        && c.rate_limit == actual_qos.rate_limit
        && c.service_level == actual_qos.service_level
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use carbide_ib_fabric::ib::fakes::{CountingFabricManager, make_partition};
    use carbide_instrument::testing::{MetricsCapture, capture_logs};
    use carbide_test_support::value_scenarios;
    use model::resource_pool::common::IbPools;
    use sqlx::PgPool;
    use state_controller::db_write_batch::DbWriteBatch;

    use super::*;
    use crate::context::IBPartitionStateHandlerServices;

    /// Runs one `handle_object_state` call against counting fakes and returns
    /// how many UFM clients were built alongside the handler outcome.
    ///
    /// The database pool is lazy and points nowhere; the arms under test never
    /// touch the database.
    async fn run_handler(
        controller_state: IBPartitionControllerState,
        mut partition: IBPartition,
    ) -> (
        usize,
        Result<StateHandlerOutcome<IBPartitionControllerState>, StateHandlerError>,
    ) {
        let manager = Arc::new(CountingFabricManager::new());
        let mut services = IBPartitionStateHandlerServices {
            db_pool: PgPool::connect_lazy("postgres://unused:unused@127.0.0.1:1/unused")
                .expect("lazy pool"),
            ib_fabric_manager: manager.clone(),
            ib_pools: IbPools {
                pkey_pools: Arc::new(HashMap::new()),
            },
        };
        let mut metrics = ();
        let mut pending_db_writes = DbWriteBatch::new();
        let mut ctx = StateHandlerContext {
            services: &mut services,
            metrics: &mut metrics,
            pending_db_writes: &mut pending_db_writes,
        };

        let partition_id = partition.id;
        let outcome = IBPartitionStateHandler::default()
            .handle_object_state(&partition_id, &mut partition, &controller_state, &mut ctx)
            .await;

        (manager.build_count(), outcome)
    }

    #[derive(Clone, Copy)]
    enum MissingPkeyCase {
        Ready,
        Deleting,
    }

    #[derive(Debug, PartialEq)]
    struct MissingPkeyObservation {
        builds: usize,
        cause: String,
        level: tracing::Level,
        message: String,
        event_name: Option<String>,
        metric_name: Option<String>,
        controller_state: Option<String>,
        counter_delta: f64,
    }

    fn observe_missing_pkey(case: MissingPkeyCase) -> MissingPkeyObservation {
        const METRIC: &str = "carbide_ib_partition_pkey_missing_total";

        let (controller_state, label) = match case {
            MissingPkeyCase::Ready => (IBPartitionControllerState::Ready, "ready"),
            MissingPkeyCase::Deleting => (IBPartitionControllerState::Deleting, "deleting"),
        };
        let partition = make_partition(None, false);
        let partition_id = partition.id.to_string();
        let metrics = MetricsCapture::start();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("test runtime");
        let mut handler_result = None;
        let logs = capture_logs(|| {
            handler_result = Some(runtime.block_on(run_handler(controller_state, partition)));
        });

        let (builds, outcome) = handler_result.expect("handler ran");
        let cause = match outcome.expect("handler succeeds") {
            StateHandlerOutcome::Transition {
                next_state: IBPartitionControllerState::Error { cause },
                ..
            } => cause,
            _ => panic!("expected transition to Error"),
        };
        let event = logs
            .iter()
            .find(|log| log.metadata_name == "ib_partition_pkey_missing")
            .expect("missing-pkey Event logged");
        assert_eq!(event.field("ib_partition_id"), Some(partition_id.as_str()));

        let other_label = match case {
            MissingPkeyCase::Ready => "deleting",
            MissingPkeyCase::Deleting => "ready",
        };
        assert_eq!(
            metrics.counter_delta(METRIC, &[("controller_state", other_label)]),
            0.0,
            "only the current transition is counted"
        );

        MissingPkeyObservation {
            builds,
            cause,
            level: event.level,
            message: event.message.clone(),
            event_name: event.field("event_name").map(str::to_string),
            metric_name: event.field("metric_name").map(str::to_string),
            controller_state: event.field("controller_state").map(str::to_string),
            counter_delta: metrics.counter_delta(METRIC, &[("controller_state", label)]),
        }
    }

    #[test]
    fn missing_pkey_transitions_log_and_count_by_controller_state() {
        const METRIC: &str = "carbide_ib_partition_pkey_missing_total";

        value_scenarios!(observe_missing_pkey:
            "partition is ready without a pkey" {
                MissingPkeyCase::Ready => MissingPkeyObservation {
                    builds: 0,
                    cause: "The pkey is None when IBPartition is ready".to_string(),
                    level: tracing::Level::ERROR,
                    message: "IB partition has no pkey while ready".to_string(),
                    event_name: Some("ib_partition_pkey_missing".to_string()),
                    metric_name: Some(METRIC.to_string()),
                    controller_state: Some("ready".to_string()),
                    counter_delta: 1.0,
                },
            }

            "partition is deleting without a pkey" {
                MissingPkeyCase::Deleting => MissingPkeyObservation {
                    builds: 0,
                    cause: "The pkey is None when deleting an IBPartition.".to_string(),
                    level: tracing::Level::ERROR,
                    message: "IB partition has no pkey while deleting".to_string(),
                    event_name: Some("ib_partition_pkey_missing".to_string()),
                    metric_name: Some(METRIC.to_string()),
                    controller_state: Some("deleting".to_string()),
                    counter_delta: 1.0,
                },
            }
        );
    }

    #[tokio::test]
    async fn provisioning_builds_no_ufm_client() {
        let (builds, outcome) = run_handler(
            IBPartitionControllerState::Provisioning,
            make_partition(Some(0x101), false),
        )
        .await;

        assert!(matches!(
            outcome,
            Ok(StateHandlerOutcome::Transition {
                next_state: IBPartitionControllerState::Ready,
                ..
            })
        ));
        assert_eq!(builds, 0, "arm resolves without touching UFM");
    }

    #[tokio::test]
    async fn error_state_builds_no_ufm_client() {
        let (builds, outcome) = run_handler(
            IBPartitionControllerState::Error {
                cause: "some earlier failure".to_string(),
            },
            make_partition(Some(0x101), false),
        )
        .await;

        assert!(matches!(outcome, Ok(StateHandlerOutcome::DoNothing { .. })));
        assert_eq!(builds, 0, "arm resolves without touching UFM");
    }

    #[tokio::test]
    async fn ready_marked_deleted_transitions_without_ufm_client() {
        let (builds, outcome) = run_handler(
            IBPartitionControllerState::Ready,
            make_partition(Some(0x101), true),
        )
        .await;

        assert!(matches!(
            outcome,
            Ok(StateHandlerOutcome::Transition {
                next_state: IBPartitionControllerState::Deleting,
                ..
            })
        ));
        assert_eq!(builds, 0, "arm resolves without touching UFM");
    }

    #[tokio::test]
    async fn deleting_with_live_network_builds_one_ufm_client() {
        let (builds, outcome) = run_handler(
            IBPartitionControllerState::Deleting,
            make_partition(Some(0x101), true),
        )
        .await;

        assert!(matches!(outcome, Ok(StateHandlerOutcome::Wait { .. })));
        assert_eq!(builds, 1, "the arm that queries UFM builds one client");
    }
}
