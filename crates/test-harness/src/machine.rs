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

use std::future::Future;

use carbide_api_core::test_support::Api;
use carbide_uuid::machine::MachineId;
use model::hardware_info::HardwareInfo;
use model::machine::AnyMachine;
use model::machine::machine_search_config::MachineSearchConfig;
use tonic::IntoRequest;

use crate::rpc::forge::forge_server::Forge;
use crate::rpc::forge::{DhcpRecord, MachineDiscoveryResult};
use crate::rpc::{DiscoveryData, DiscoveryInfo};

pub trait TestMachine {
    fn id(&self) -> MachineId;

    fn api(&self) -> &Api;

    fn rpc_machine(&self) -> impl Future<Output = crate::rpc::Machine> + '_ {
        async move {
            self.api()
                .find_machines_by_ids(
                    crate::rpc::forge::MachinesByIdsRequest {
                        machine_ids: vec![self.id()],
                        include_history: true,
                    }
                    .into_request(),
                )
                .await
                .expect("machine lookup by id should succeed")
                .into_inner()
                .machines
                .remove(0)
        }
    }

    fn db_machine<'a, 'txn>(
        &'a self,
        txn: &'a mut sqlx::PgTransaction<'txn>,
    ) -> impl Future<Output = AnyMachine> + 'a {
        async move {
            db::machine::find_one(txn.as_mut(), &self.id(), MachineSearchConfig::default())
                .await
                .expect("machine lookup should succeed")
                .expect("machine should exist")
        }
    }

    fn machine(&self) -> impl Future<Output = AnyMachine> + '_ {
        async move {
            let mut txn = self
                .api()
                .database_connection
                .begin()
                .await
                .expect("database transaction should start");
            let machine = self.db_machine(&mut txn).await;
            txn.commit()
                .await
                .expect("database transaction should commit");
            machine
        }
    }
}

pub(crate) trait TestMachinePrivate: TestMachine {
    async fn discover_machine(
        &self,
        dhcp_record: &DhcpRecord,
        hardware_info: HardwareInfo,
    ) -> MachineDiscoveryResult {
        self.api()
            .discover_machine(
                crate::rpc::forge::MachineDiscoveryInfo {
                    machine_interface_id: Some(
                        *dhcp_record
                            .machine_interface_id
                            .as_ref()
                            .expect("DHCP record should include a machine interface id"),
                    ),
                    create_machine: true,
                    discovery_data: Some(DiscoveryData::Info(
                        DiscoveryInfo::try_from(hardware_info)
                            .expect("hardware info should convert to discovery info"),
                    )),
                    ..Default::default()
                }
                .into_request(),
            )
            .await
            .expect("machine discovery should succeed")
            .into_inner()
    }
}
