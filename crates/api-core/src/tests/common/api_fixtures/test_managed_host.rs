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

use std::collections::HashMap;
use std::sync::Arc;

use carbide_uuid::instance::InstanceId;
use carbide_uuid::machine::{DpuMachineId, HostMachineId};
// `PgPoolReader` and `LoadSnapshotOptions` are only used by the `#[cfg(test)]`
// `TestManagedHostSnapshots` trait below, so gate them out of test-support-only builds.
#[cfg(test)]
use db::db_read::PgPoolReader;
#[cfg(test)]
use model::machine::LoadSnapshotOptions;
use model::machine::{
    InstanceState, Machine, ManagedHostState, ManagedHostStateSnapshot, ReprovisionState,
};
use rpc::forge::forge_agent_control_response::LegacyAction;
use rpc::forge::forge_server::Forge;
use rpc::forge_agent_control_response::Action;
use tonic::Request;

use crate::tests::common::api_fixtures::instance::TestInstanceBuilder;
use crate::tests::common::api_fixtures::{Api, TestEnv, TestMachine};

pub(in crate::tests) struct TestManagedHost {
    pub(in crate::tests) id: HostMachineId,
    pub(in crate::tests) dpu_ids: Vec<DpuMachineId>,
    pub(in crate::tests) api: Arc<Api>,
}

impl From<TestManagedHost> for (HostMachineId, DpuMachineId) {
    fn from(mut v: TestManagedHost) -> Self {
        (v.id, v.dpu_ids.remove(0))
    }
}

type Txn<'a> = sqlx::Transaction<'a, sqlx::Postgres>;

impl TestManagedHost {
    // from_rpc_machine() will first try to read from m.status.interfaces
    // and fall back to the deprecated m.interfaces only if status is absent
    // or if status.interfaces yields zero DPU ids
    #[allow(deprecated)]
    pub(in crate::tests) fn from_rpc_machine(m: &rpc::Machine, api: Arc<Api>) -> Self {
        TestManagedHost {
            id: m
                .id
                .unwrap()
                .try_into()
                .expect("MachineId should be a HostMachineId"),
            dpu_ids: m
                .status
                .as_ref()
                .map(|s| {
                    s.interfaces
                        .iter()
                        .filter_map(|i| i.attached_dpu_machine_id)
                        .collect::<Vec<_>>()
                })
                .filter(|ids| !ids.is_empty())
                .unwrap_or_else(|| {
                    m.interfaces
                        .iter()
                        .filter_map(|i| i.attached_dpu_machine_id)
                        .collect()
                })
                .into_iter()
                .map(TryInto::try_into)
                .collect::<Result<Vec<DpuMachineId>, _>>()
                .expect("attachec_dpu_machine_id should be a valid DpuMachineId"),
            api,
        }
    }

    pub(in crate::tests) fn dpu(&self) -> TestMachine<DpuMachineId> {
        TestMachine::new(self.dpu_ids[0], self.api.clone())
    }

    pub(in crate::tests) fn dpu_n(&self, n: usize) -> TestMachine<DpuMachineId> {
        assert!(n < self.dpu_ids.len());
        TestMachine::new(self.dpu_ids[n], self.api.clone())
    }

    pub(in crate::tests) fn host(&self) -> TestMachine<HostMachineId> {
        TestMachine::new(self.id, self.api.clone())
    }

    pub(in crate::tests) async fn snapshot(&self, txn: &mut Txn<'_>) -> ManagedHostStateSnapshot {
        db::managed_host::load_snapshot(txn.as_mut(), &self.id, Default::default())
            .await
            .unwrap()
            .unwrap()
    }

    pub(in crate::tests) async fn dpu_db_machines(&self, txn: &mut Txn<'_>) -> Vec<Machine> {
        db::machine::find_dpus_by_host_machine_id(txn, &self.id)
            .await
            .unwrap()
    }

    pub(in crate::tests) fn new_dpu_reprovision_state(
        &self,
        state: ReprovisionState,
    ) -> ManagedHostState {
        ManagedHostState::DPUReprovision {
            dpu_states: model::machine::DpuReprovisionStates {
                states: HashMap::from([(self.dpu_ids[0], state)]),
            },
        }
    }

    pub(in crate::tests) fn new_dpus_reprovision_state(
        &self,
        states: &[&ReprovisionState],
    ) -> ManagedHostState {
        assert_eq!(states.len(), self.dpu_ids.len());
        ManagedHostState::DPUReprovision {
            dpu_states: model::machine::DpuReprovisionStates {
                states: self
                    .dpu_ids
                    .iter()
                    .zip(states.iter())
                    .map(|(id, state)| (*id, (*state).clone()))
                    .collect(),
            },
        }
    }

    pub(in crate::tests) fn new_dpu_assigned_reprovision_state(
        &self,
        state: ReprovisionState,
    ) -> ManagedHostState {
        ManagedHostState::Assigned {
            instance_state: InstanceState::DPUReprovision {
                dpu_states: model::machine::DpuReprovisionStates {
                    states: HashMap::from([(self.dpu_ids[0], state)]),
                },
            },
        }
    }

    pub(in crate::tests) async fn network_configured(&self, test_env: &TestEnv) {
        crate::tests::common::api_fixtures::network_configured(test_env, &self.dpu_ids).await
    }

    pub(in crate::tests) async fn machine_validation_completed(&self) {
        let response = self.host().forge_agent_control().await;
        let Some(Action::MachineValidation(machine_validation)) = response.action else {
            panic!("expected typed machine validation action");
        };
        assert_eq!(
            response.legacy_action,
            LegacyAction::MachineValidation as i32
        );
        let validation_id = machine_validation
            .validation_id
            .expect("machine validation action missing validation_id");
        self.api
            .machine_validation_completed(Request::new(
                rpc::forge::MachineValidationCompletedRequest {
                    machine_id: Some(self.id.into()),
                    machine_validation_error: None,
                    validation_id: Some(validation_id),
                },
            ))
            .await
            .unwrap()
            .into_inner();
    }

    pub(in crate::tests) fn instance_builer<'a, 'b>(
        &'b self,
        test_env: &'a TestEnv,
    ) -> TestInstanceBuilder<'a, 'b> {
        TestInstanceBuilder::new(test_env, self)
    }

    pub(in crate::tests) async fn delete_instance(&self, env: &TestEnv, instance_id: InstanceId) {
        crate::tests::common::api_fixtures::instance::delete_instance(env, instance_id, self).await
    }
}

// Only consumed by this crate's own `#[cfg(test)]` test cases (not by the `test-support` consumers),
// so gate it out of test-support-only builds to keep dead-code detection working.
#[cfg(test)]
pub(in crate::tests) trait TestManagedHostSnapshots {
    async fn snapshots(
        &self,
        txn: &mut PgPoolReader,
        load_options: LoadSnapshotOptions,
    ) -> HashMap<HostMachineId, ManagedHostStateSnapshot>;
}

#[cfg(test)]
impl TestManagedHostSnapshots for Vec<TestManagedHost> {
    async fn snapshots(
        &self,
        txn: &mut PgPoolReader,
        load_options: LoadSnapshotOptions,
    ) -> HashMap<HostMachineId, ManagedHostStateSnapshot> {
        let machine_ids = self.iter().map(|machine| machine.id).collect::<Vec<_>>();
        db::managed_host::load_by_machine_ids(txn, &machine_ids, load_options)
            .await
            .unwrap()
    }
}
