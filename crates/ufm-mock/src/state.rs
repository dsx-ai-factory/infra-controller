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

mod partitions;
mod ports;
mod reconciliation;
#[cfg(test)]
mod tests;
mod types;

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

pub(crate) use types::{
    BindRequest, Fabric, FabricError, FabricRead, PartitionKey, QosRequest, UnbindRequest,
};
use types::{DEFAULT_PARTITION_KEY, FabricState, Partition, PartitionQos};
#[cfg(test)]
use types::{PortMembership, ReconcileOutcome};

use crate::config::FabricConfig;

impl Fabric {
    pub(crate) fn new(config: FabricConfig) -> Self {
        let default_partition = Partition {
            name: "management".into(),
            ip_over_ib: true,
            qos: PartitionQos::default(),
            members: HashMap::new(),
        };
        let inner = FabricState {
            candidates: HashMap::new(),
            ports: HashMap::new(),
            sources: HashMap::new(),
            partitions: HashMap::from([(DEFAULT_PARTITION_KEY, default_partition)]),
            next_lid: 1,
        };
        Self {
            config: Arc::new(config),
            inner: Arc::new(RwLock::new(inner)),
        }
    }

    pub(crate) fn version(&self) -> &str {
        &self.config.ufm_version
    }

    pub(crate) fn sm_config(&self) -> &crate::config::SmConfig {
        &self.config.sm_config
    }

    pub(crate) fn read(&self) -> FabricRead<'_> {
        FabricRead {
            state: self.inner.read().expect("fabric lock poisoned"),
        }
    }
}
