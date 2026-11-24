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

use carbide_api_core::test_support::Api;
use carbide_uuid::machine::{DpuMachineId, MachineId};
use mac_address::MacAddress;
use model::hardware_info::HardwareInfo;
use model::test_support::DpuConfig;

use crate::machine::{TestMachine, TestMachinePrivate};
use crate::network::segment::TestNetworkSegment;
use crate::rpc::forge::forge_server::Forge;
use crate::rpc::forge::{DhcpDiscovery, DhcpRecord};

#[derive(Clone)]
pub struct TestDpuMachine {
    pub id: DpuMachineId,
    pub bmc_mac: MacAddress,
    pub api: Arc<Api>,
    hardware_info: HardwareInfo,
    oob_mac: MacAddress,
}

impl TestDpuMachine {
    pub(crate) fn new(id: DpuMachineId, api: Arc<Api>, config: &DpuConfig) -> Self {
        Self {
            id,
            bmc_mac: config.bmc_mac_address,
            api,
            hardware_info: HardwareInfo::from(config),
            oob_mac: config.oob_mac_address,
        }
    }

    pub fn oob_mac(&self) -> MacAddress {
        self.oob_mac
    }

    pub async fn dhcp_discover_oob_iface(&self, segment: TestNetworkSegment) -> DhcpRecord {
        self.api
            .discover_dhcp(
                DhcpDiscovery::builder(self.oob_mac(), segment.relay_address)
                    .vendor_string("SomeVendor")
                    .tonic_request(),
            )
            .await
            .expect("DPU OOB interface DHCP discovery should succeed")
            .into_inner()
    }

    pub async fn discover_oob_iface(&self, segment: TestNetworkSegment) {
        let dhcp_record = self.dhcp_discover_oob_iface(segment).await;
        let discovered_id = self
            .discover_machine(&dhcp_record, self.hardware_info.clone())
            .await
            .machine_id
            .expect("DPU discovery should return a machine id");
        assert_eq!(
            discovered_id,
            self.id.into(),
            "DPU discovery should resolve to the expected test machine"
        );
    }

    /// Starts building a status acknowledgement for this DPU's current desired network config.
    pub fn network_status(&self) -> crate::DpuNetworkStatusBuilder<'_> {
        crate::DpuNetworkStatusBuilder::new(&self.api, self.id)
    }
}

impl TestMachine for TestDpuMachine {
    fn id(&self) -> MachineId {
        self.id.into()
    }

    fn api(&self) -> &Api {
        &self.api
    }
}

impl TestMachinePrivate for TestDpuMachine {}
