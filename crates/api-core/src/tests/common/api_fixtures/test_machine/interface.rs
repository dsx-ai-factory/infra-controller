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

use carbide_uuid::machine::MachineInterfaceId;
use rpc::forge::forge_server::Forge;
use rpc::forge::{MachineArchitecture, PxeInstructions};

use crate::tests::common::api_fixtures::Api;

pub(in crate::tests) struct TestMachineInterface {
    id: MachineInterfaceId,
    api: Arc<Api>,
}

impl TestMachineInterface {
    pub(in crate::tests) fn new(id: MachineInterfaceId, api: Arc<Api>) -> Self {
        Self { id, api }
    }

    pub(in crate::tests) async fn get_pxe_instructions(
        &self,
        arch: MachineArchitecture,
    ) -> PxeInstructions {
        let mut txn = self.api.txn_begin().await.unwrap();
        let iface = db::machine_interface::find_one(txn.as_pgconn(), self.id)
            .await
            .unwrap();
        txn.commit().await.unwrap();
        let client_ip = iface
            .addresses
            .first()
            .expect("interface must have at least one address to PXE boot")
            .to_string();

        self.api
            .get_pxe_instructions(tonic::Request::new(rpc::forge::PxeInstructionRequest {
                arch: arch as i32,
                product: None,
                client_ip: Some(client_ip),
                ..Default::default()
            }))
            .await
            .unwrap()
            .into_inner()
    }
}
