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
use std::net::IpAddr;
use std::str::FromStr;
use std::sync::Arc;

use carbide_utils::redfish::BmcAccessInfo;
use carbide_uuid::machine::{MachineId, MachineIdSubtypeTrait};
use model::machine::{Machine, ManagedHostState};
use rpc::forge::forge_server::Forge;
use tonic::Request;

use crate::tests::common::api_fixtures::{Api, TestEnv};

pub(in crate::tests) mod interface;

pub(in crate::tests) type TestMachineInterface = interface::TestMachineInterface;

pub(in crate::tests) struct TestMachine<ID: MachineIdSubtypeTrait> {
    pub(in crate::tests) id: ID,
    api: Arc<Api>,
}

type Txn<'a> = sqlx::Transaction<'a, sqlx::Postgres>;

impl<ID> TestMachine<ID>
where
    ID: MachineIdSubtypeTrait,
    ID: TryFrom<MachineId>,
    db::DatabaseError: From<<ID as TryFrom<MachineId>>::Error>,
{
    pub(in crate::tests) fn new(id: ID, api: Arc<Api>) -> Self {
        Self { id, api }
    }

    pub(in crate::tests) async fn rpc_machine(&self) -> rpc::Machine {
        self.api
            .find_machines_by_ids(tonic::Request::new(rpc::forge::MachinesByIdsRequest {
                machine_ids: vec![self.id.into()],
                include_history: true,
            }))
            .await
            .unwrap()
            .into_inner()
            .machines
            .remove(0)
    }

    pub(in crate::tests) async fn next_iteration_machine(&self, env: &TestEnv) -> Machine<ID> {
        env.run_machine_state_controller_iteration().await;
        let mut txn = env.pool.begin().await.unwrap();
        let dpu = self.db_machine(&mut txn).await;
        txn.commit().await.unwrap();
        dpu
    }

    pub(in crate::tests) async fn db_machine(&self, txn: &mut Txn<'_>) -> Machine<ID> {
        db::machine::find_one(txn.as_mut(), &self.id, Default::default())
            .await
            .unwrap()
            .unwrap()
    }

    pub(in crate::tests) async fn bmc_access(&self, txn: &mut Txn<'_>) -> BmcAccessInfo {
        let addr = self.db_machine(txn).await.bmc_addr().unwrap();
        db::machine_interface::lookup_bmc_access_info(txn.as_mut(), addr.ip(), Some(addr.port()))
            .await
            .unwrap()
    }

    pub(in crate::tests) async fn first_interface(
        &self,
        txn: &mut Txn<'_>,
    ) -> TestMachineInterface {
        TestMachineInterface::new(
            db::machine_interface::find_by_machine_ids(txn, &[self.id])
                .await
                .unwrap()
                .get(&self.id)
                .unwrap()[0]
                .id,
            self.api.clone(),
        )
    }

    pub(in crate::tests) async fn reboot_completed(
        &self,
    ) -> rpc::forge::MachineRebootCompletedResponse {
        let response = self
            .api
            .reboot_completed(Request::new(rpc::forge::MachineRebootCompletedRequest {
                machine_id: Some(self.id.into()),
            }))
            .await
            .unwrap()
            .into_inner();
        tracing::info!(
            machine_id = %self.id,
            "Machine rebooted",
        );
        response
    }

    pub(in crate::tests) async fn forge_agent_control(
        &self,
    ) -> rpc::forge::ForgeAgentControlResponse {
        self.reboot_completed().await;
        self.api
            .forge_agent_control(Request::new(rpc::forge::ForgeAgentControlRequest {
                machine_id: Some(self.id.into()),
            }))
            .await
            .unwrap()
            .into_inner()
    }

    pub(in crate::tests) async fn discovery_completed(&self) {
        self.api
            .discovery_completed(Request::new(rpc::forge::MachineDiscoveryCompletedRequest {
                machine_id: Some(self.id.into()),
            }))
            .await
            .unwrap()
            .into_inner();
    }

    pub(in crate::tests) async fn trigger_dpu_reprovisioning(
        &self,
        mode: rpc::forge::dpu_reprovisioning_request::Mode,
        update_firmware: bool,
    ) {
        self.api
            .trigger_dpu_reprovisioning(tonic::Request::new(
                ::rpc::forge::DpuReprovisioningRequest {
                    dpu_id: None,
                    machine_id: Some(self.id.into()),
                    mode: mode as i32,
                    initiator: ::rpc::forge::UpdateInitiator::AdminCli as i32,
                    update_firmware,
                },
            ))
            .await
            .unwrap();
    }

    pub(in crate::tests) async fn bmc_ip(&self, txn: &mut Txn<'_>) -> Option<IpAddr> {
        let machine = self.db_machine(txn).await;
        machine.bmc_addr().map(|addr| addr.ip())
    }

    /// Replaces the model in this machine's persisted Site Explorer report.
    pub(in crate::tests) async fn set_exploration_model(&self, txn: &mut Txn<'_>, model: &str) {
        let bmc_ip = self
            .bmc_ip(txn)
            .await
            .expect("fixture machine should have a BMC IP");
        let endpoint = db::explored_endpoints::find_by_ips(txn.as_mut(), vec![bmc_ip])
            .await
            .unwrap()
            .pop()
            .expect("fixture machine should have a Site Explorer report");
        let old_version = endpoint.report_version;
        let waiting_for_explorer_refresh = endpoint.waiting_for_explorer_refresh;
        let mut report = endpoint.report;
        report
            .systems
            .first_mut()
            .expect("fixture report should contain a Redfish system")
            .model = Some(model.to_string());
        report.model = report.model();

        let updated = db::explored_endpoints::try_update(
            bmc_ip,
            old_version,
            &report,
            waiting_for_explorer_refresh,
            txn.as_mut(),
        )
        .await
        .unwrap();
        assert!(updated, "fixture Site Explorer report should be updated");
    }

    pub(in crate::tests) async fn json_history(
        &self,
        limit: Option<usize>,
    ) -> Vec<serde_json::Value> {
        let machine = self.rpc_machine().await;
        let mut states: Vec<serde_json::Value> = machine
            .events
            .into_iter()
            .map(|e| serde_json::Value::from_str(&e.event).unwrap())
            .collect();
        if let Some(limit) = limit {
            if states.len() >= limit {
                states.split_off(states.len() - limit)
            } else {
                states
            }
        } else {
            states
        }
    }

    pub(in crate::tests) async fn parsed_history(
        &self,
        limit: Option<usize>,
    ) -> Vec<ManagedHostState> {
        let json_states = self.json_history(limit).await;
        json_states
            .into_iter()
            .map(|s| serde_json::from_value::<ManagedHostState>(s).unwrap())
            .collect()
    }
}
