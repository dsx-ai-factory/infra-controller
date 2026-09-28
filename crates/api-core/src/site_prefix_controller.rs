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

//! Makes tenant SitePrefixes usable after the required DPUs apply protection.

use carbide_uuid::site_prefix::SitePrefixId;
use chrono::{DateTime, Utc};
use config_version::{ConfigVersion, Versioned};
use db::{ConditionalWrite, ControllerStateNotCurrent, DatabaseError};
use model::StateSla;
use model::controller_outcome::PersistentStateHandlerOutcome;
use model::site_prefix::{
    SitePrefix, SitePrefixAuthority, SitePrefixLifecycleState, SitePrefixSearchFilter,
};
use sqlx::{PgConnection, PgPool};
use state_controller::CheckApplied;
use state_controller::io::StateControllerIO;
use state_controller::metrics::NoopMetricsEmitter;
use state_controller::state_handler::{
    StateHandler, StateHandlerContext, StateHandlerContextObjects, StateHandlerError,
    StateHandlerOutcome,
};

use crate::cfg::file::VpcIsolationBehaviorType;

#[derive(Debug, Default)]
pub(crate) struct SitePrefixReadiness {
    pub(crate) vpc_isolation_behavior: VpcIsolationBehaviorType,
}

impl SitePrefixReadiness {
    async fn wait_for_isolation(
        &self,
        txn: &mut PgConnection,
        site_prefix_id: SitePrefixId,
        requested_at: DateTime<Utc>,
    ) -> Result<Option<StateHandlerOutcome<SitePrefixLifecycleState>>, DatabaseError> {
        let isolation_required = matches!(
            self.vpc_isolation_behavior,
            VpcIsolationBehaviorType::MutualIsolation
        );
        for host in db::site_prefix::find_isolation_hosts(txn, isolation_required).await? {
            if !host.managed_host_network_config_version_synced() {
                tracing::info!(
                    %site_prefix_id,
                    host_machine_id = %host.host_snapshot.id,
                    network_config_version = %host.host_snapshot.network_config.version,
                    isolation_requested_at = %requested_at,
                    "Waiting for SitePrefix DPU acknowledgements",
                );
                return Ok(Some(StateHandlerOutcome::wait(format!(
                    "waiting for all DPUs on host {} to acknowledge network version {}; isolation requested at {}",
                    host.host_snapshot.id,
                    host.host_snapshot.network_config.version,
                    requested_at.to_rfc3339(),
                ))));
            }
        }
        Ok(None)
    }
}

impl StateHandlerContextObjects for SitePrefixReadiness {
    type Services = PgPool;
    type ObjectMetrics = ();
}

#[async_trait::async_trait]
impl StateControllerIO for SitePrefixReadiness {
    type ObjectId = SitePrefixId;
    type State = SitePrefix;
    type ControllerState = SitePrefixLifecycleState;
    type MetricsEmitter = NoopMetricsEmitter;
    type ContextObjects = Self;

    const DB_ITERATION_ID_TABLE_NAME: &'static str = "site_prefixes_controller_iteration_ids";
    const DB_QUEUED_OBJECTS_TABLE_NAME: &'static str = "site_prefixes_controller_queued_objects";
    const LOG_SPAN_CONTROLLER_NAME: &'static str = "site_prefix_controller";

    async fn list_objects(
        &self,
        txn: &mut PgConnection,
    ) -> Result<Vec<SitePrefixId>, DatabaseError> {
        db::site_prefix::find_ids(
            txn,
            SitePrefixSearchFilter {
                authority: Some(SitePrefixAuthority::TenantManaged),
                lifecycle_state: Some(SitePrefixLifecycleState::Provisioning),
                ..Default::default()
            },
        )
        .await
    }

    async fn load_object_state(
        &self,
        txn: &mut PgConnection,
        object_id: &SitePrefixId,
    ) -> Result<Option<SitePrefix>, DatabaseError> {
        Ok(db::site_prefix::find_by_ids(txn, &[*object_id])
            .await?
            .pop())
    }

    async fn load_controller_state(
        &self,
        _txn: &mut PgConnection,
        _object_id: &SitePrefixId,
        state: &SitePrefix,
    ) -> Result<Versioned<SitePrefixLifecycleState>, DatabaseError> {
        Ok(Versioned::new(state.status.lifecycle_state, state.version))
    }

    async fn persist_controller_state(
        &self,
        txn: &mut PgConnection,
        object_id: &SitePrefixId,
        old_version: ConfigVersion,
        new_version: ConfigVersion,
        new_state: &SitePrefixLifecycleState,
    ) -> Result<ConditionalWrite<(), ControllerStateNotCurrent>, DatabaseError> {
        if *new_state != SitePrefixLifecycleState::Ready {
            return Err(DatabaseError::FailedPrecondition(
                "SitePrefix readiness can only transition to Ready".to_string(),
            ));
        }
        db::site_prefix::try_mark_ready(txn, *object_id, old_version, new_version).await
    }

    async fn persist_state_history(
        &self,
        txn: &mut PgConnection,
        object_id: &SitePrefixId,
        new_version: ConfigVersion,
        new_state: &SitePrefixLifecycleState,
    ) -> Result<(), DatabaseError> {
        db::state_history::persist(
            txn,
            db::state_history::StateHistoryTableId::SitePrefix,
            object_id,
            new_state,
            new_version,
        )
        .await?;
        Ok(())
    }

    async fn persist_outcome(
        &self,
        txn: &mut PgConnection,
        object_id: &SitePrefixId,
        outcome: PersistentStateHandlerOutcome,
    ) -> Result<(), DatabaseError> {
        db::site_prefix::update_controller_state_outcome(txn, *object_id, outcome).await
    }

    fn metric_state_names(state: &SitePrefixLifecycleState) -> (&'static str, &'static str) {
        match state {
            SitePrefixLifecycleState::Provisioning => ("provisioning", ""),
            SitePrefixLifecycleState::Ready => ("ready", ""),
            SitePrefixLifecycleState::Deleting => ("deleting", ""),
            SitePrefixLifecycleState::Error => ("error", ""),
        }
    }

    fn state_sla(
        &self,
        _state: &Versioned<SitePrefixLifecycleState>,
        _object_state: &SitePrefix,
    ) -> StateSla {
        StateSla::no_sla()
    }
}

#[async_trait::async_trait]
impl StateHandler for SitePrefixReadiness {
    type ObjectId = SitePrefixId;
    type State = SitePrefix;
    type ControllerState = SitePrefixLifecycleState;
    type ContextObjects = Self;

    async fn handle_object_state(
        &self,
        object_id: &SitePrefixId,
        state: &mut SitePrefix,
        _controller_state: &SitePrefixLifecycleState,
        ctx: &mut StateHandlerContext<Self>,
    ) -> Result<StateHandlerOutcome<SitePrefixLifecycleState>, StateHandlerError> {
        let mut txn = ctx.services.begin().await?;
        let requested_at = db::site_prefix::isolation_requested_at(&mut txn, *object_id).await?;
        if let Some(requested_at) = requested_at {
            // A stale negative only delays readiness. Avoid holding the routing
            // lock through this scan: a queued writer also delays later readers.
            // An apparently complete set still needs the locked check below.
            if let Some(wait) = self
                .wait_for_isolation(&mut txn, *object_id, requested_at)
                .await?
            {
                return Ok(wait.with_txn(txn));
            }
            db::tenant_prefix_overlap::lock_config(&mut txn).await?;
        } else {
            db::tenant_prefix_overlap::lock_checks(&mut txn).await?;
        }
        let Some(current) = db::site_prefix::find_by_id_for_update(&mut txn, *object_id).await?
        else {
            return Ok(StateHandlerOutcome::deleted().with_txn(txn));
        };
        if current.status.authority != SitePrefixAuthority::TenantManaged
            || current.status.lifecycle_state != SitePrefixLifecycleState::Provisioning
        {
            return Ok(StateHandlerOutcome::do_nothing().with_txn(txn));
        }
        if current.version != state.version {
            return Err(StateHandlerError::IterationInvalidated {
                source_ref: std::panic::Location::caller(),
            });
        }

        let isolation_required = matches!(
            self.vpc_isolation_behavior,
            VpcIsolationBehaviorType::MutualIsolation
        );
        let requested_at = match requested_at {
            Some(requested_at) => requested_at,
            None => db::site_prefix::request_isolation(&mut txn, &current, isolation_required)
                .await?
                .check_applied()?,
        };
        if let Some(wait) = self
            .wait_for_isolation(&mut txn, *object_id, requested_at)
            .await?
        {
            return Ok(wait.with_txn(txn));
        }

        // Keep the routing and root locks through the framework's transition.
        // A later assignment gets the complete prefix set; a concurrent delete
        // cannot be overwritten by this readiness decision.
        Ok(StateHandlerOutcome::transition(SitePrefixLifecycleState::Ready).with_txn(txn))
    }
}
