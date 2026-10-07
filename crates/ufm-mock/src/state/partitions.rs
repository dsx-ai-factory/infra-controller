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

use std::collections::{BTreeMap, HashMap};

use super::types::{
    BindRequest, DEFAULT_PARTITION_KEY, Fabric, FabricError, FabricRead, Partition, PartitionData,
    PartitionKey, PartitionQos, PortConfig, PortConfigData, PortMembership, QosRequest,
    UnbindRequest,
};

impl Fabric {
    pub(crate) fn bind(&self, request: BindRequest) -> Result<(), FabricError> {
        let key = request.pkey;
        let guids = request.guids;
        let mut state = self.inner.write().expect("fabric lock poisoned");
        for guid in &guids {
            if !state.ports.contains_key(guid) {
                return Err(FabricError::PortNotFound(*guid));
            }
        }
        let partition = state.partitions.entry(key).or_insert_with(|| Partition {
            name: format!("api_pkey_{key}").into(),
            ip_over_ib: request.ip_over_ib,
            qos: PartitionQos::default(),
            members: HashMap::new(),
        });
        partition.ip_over_ib = request.ip_over_ib;
        for guid in guids {
            partition.members.insert(
                guid,
                PortConfig {
                    membership: request.membership,
                    index0: request.index0,
                },
            );
        }
        Ok(())
    }

    pub(crate) fn unbind(&self, request: UnbindRequest) {
        let key = request.pkey;
        let guids = request.guids;
        let mut state = self.inner.write().expect("fabric lock poisoned");
        let Some(partition) = state.partitions.get_mut(&key) else {
            return;
        };
        for guid in guids {
            partition.members.remove(&guid);
        }
        if key != DEFAULT_PARTITION_KEY && partition.members.is_empty() {
            state.partitions.remove(&key);
        }
    }

    pub(crate) fn update_qos(&self, request: QosRequest) -> Result<(), FabricError> {
        let key = request.pkey;
        let mut state = self.inner.write().expect("fabric lock poisoned");
        let partition = state
            .partitions
            .get_mut(&key)
            .ok_or(FabricError::PartitionNotFound(key))?;
        partition.qos = PartitionQos {
            mtu_limit: request.mtu_limit,
            service_level: request.service_level,
            rate_limit: request.rate_limit,
        };
        Ok(())
    }
}

impl FabricRead<'_> {
    pub(crate) fn partitions(
        &self,
        include_guids: bool,
        include_qos: bool,
    ) -> BTreeMap<&PartitionKey, PartitionData<'_>> {
        self.state
            .partitions
            .iter()
            .map(|(key, partition)| {
                (
                    key,
                    partition_data(*key, partition, include_guids, include_qos),
                )
            })
            .collect()
    }

    pub(crate) fn partition(
        &self,
        key: PartitionKey,
        include_guids: bool,
        include_qos: bool,
    ) -> Result<Option<PartitionData<'_>>, FabricError> {
        Ok(self
            .state
            .partitions
            .get(&key)
            .map(|partition| partition_data(key, partition, include_guids, include_qos)))
    }
}

fn partition_data(
    key: PartitionKey,
    partition: &Partition,
    include_guids: bool,
    include_qos: bool,
) -> PartitionData<'_> {
    static DEFAULT_MEMBERSHIP: PortMembership = PortMembership::Limited;

    let mut members = partition.members.iter().collect::<Vec<_>>();
    members.sort_by_key(|(guid, _)| *guid);
    PartitionData {
        partition: partition.name.as_ref(),
        ip_over_ib: &partition.ip_over_ib,
        qos_conf: include_qos.then_some(&partition.qos),
        guids: include_guids.then(|| {
            members
                .into_iter()
                .map(|(guid, config)| PortConfigData {
                    guid,
                    membership: &config.membership,
                    index0: &config.index0,
                })
                .collect()
        }),
        membership: (key == DEFAULT_PARTITION_KEY).then_some(&DEFAULT_MEMBERSHIP),
    }
}
