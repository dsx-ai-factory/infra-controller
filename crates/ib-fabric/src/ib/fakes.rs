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

//! Canned fakes for tests that exercise fabric-facing code without a UFM
//! server: an [`IBFabric`] whose operations succeed with empty data, an
//! [`IBFabricManager`] that counts how many clients it builds, and an
//! `IBPartition` factory. Unlike the stateful `MockIBFabric` behind
//! `IBFabricManagerType::Mock`, these fakes hold no fabric state; they exist
//! to observe interaction counts and drive handler/monitor control flow.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use config_version::{ConfigVersion, Versioned};
use model::ib::{IBNetwork, IBPort, IBQosConf};
use model::ib_partition::{
    IBPartition, IBPartitionConfig, IBPartitionControllerState, IBPartitionStatus, PartitionKey,
};
use model::metadata::Metadata;
use model::tenant::TenantOrganizationId;

use super::{
    Filter, GetPartitionOptions, IBFabric, IBFabricConfig, IBFabricManager, IBFabricManagerConfig,
    IBFabricRawResponse, IBFabricVersions,
};
use crate::errors::IbError;

/// An [`IBFabric`] whose operations all succeed with empty data, so tests can
/// drive fabric interactions without a UFM server.
#[derive(Debug, Default)]
pub struct StubIBFabric;

#[async_trait]
impl IBFabric for StubIBFabric {
    async fn get_fabric_config(&self) -> Result<IBFabricConfig, IbError> {
        Ok(IBFabricConfig::default())
    }

    async fn update_partition_qos_conf(
        &self,
        _pkey: u16,
        _qos_conf: &IBQosConf,
    ) -> Result<(), IbError> {
        Ok(())
    }

    async fn get_ib_networks(
        &self,
        _options: GetPartitionOptions,
    ) -> Result<HashMap<u16, IBNetwork>, IbError> {
        Ok(HashMap::new())
    }

    async fn get_ib_network(
        &self,
        pkey: u16,
        _options: GetPartitionOptions,
    ) -> Result<IBNetwork, IbError> {
        Ok(IBNetwork {
            name: "stub".to_string(),
            pkey,
            ipoib: false,
            qos_conf: None,
            associated_guids: Some(HashSet::new()),
            membership: None,
        })
    }

    async fn bind_ib_ports(
        &self,
        _ibnetwork: IBNetwork,
        _ports: Vec<String>,
    ) -> Result<(), IbError> {
        Ok(())
    }

    async fn unbind_ib_ports(&self, _pkey: u16, _id: Vec<String>) -> Result<(), IbError> {
        Ok(())
    }

    async fn find_ib_port(&self, _filter: Option<Filter>) -> Result<Vec<IBPort>, IbError> {
        Ok(Vec::new())
    }

    async fn versions(&self) -> Result<IBFabricVersions, IbError> {
        Ok(IBFabricVersions {
            ufm_version: "stub".to_string(),
        })
    }

    async fn raw_get(&self, _path: &str) -> Result<IBFabricRawResponse, IbError> {
        Ok(IBFabricRawResponse {
            body: String::new(),
            code: 200,
            headers: http::HeaderMap::new(),
        })
    }
}

/// An [`IBFabricManager`] that counts how many [`StubIBFabric`] clients it
/// builds. Each `new_client` call stands in for the real manager's
/// secret-manager fetch + TLS + HTTP client construction, so tests can assert
/// how often that cost is paid.
#[derive(Debug, Default)]
pub struct CountingFabricManager {
    builds: AtomicUsize,
}

impl CountingFabricManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of clients built so far.
    pub fn build_count(&self) -> usize {
        self.builds.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl IBFabricManager for CountingFabricManager {
    async fn new_client(&self, _fabric_name: &str) -> Result<Arc<dyn IBFabric>, IbError> {
        self.builds.fetch_add(1, Ordering::SeqCst);
        Ok(Arc::new(StubIBFabric))
    }

    fn get_config(&self) -> IBFabricManagerConfig {
        IBFabricManagerConfig::default()
    }
}

/// A minimal `IBPartition` for handler and monitor tests: `pkey` lands in both
/// the config and the status, and `deleted` marks the partition as deleted.
pub fn make_partition(pkey: Option<u16>, deleted: bool) -> IBPartition {
    let pkey = pkey.map(|p| PartitionKey::try_from(p).expect("valid pkey"));
    IBPartition {
        id: carbide_uuid::infiniband::IBPartitionId::new(),
        version: ConfigVersion::initial(),
        config: IBPartitionConfig {
            name: "partition-under-test".to_string(),
            pkey,
            tenant_organization_id: TenantOrganizationId::try_from("tenant-1".to_string())
                .expect("valid tenant org id"),
            mtu: None,
            rate_limit: None,
            service_level: None,
        },
        status: Some(IBPartitionStatus {
            partition: None,
            mtu: None,
            rate_limit: None,
            service_level: None,
            pkey,
        }),
        deleted: deleted.then(chrono::Utc::now),
        controller_state: Versioned::new(
            IBPartitionControllerState::Provisioning,
            ConfigVersion::initial(),
        ),
        controller_state_outcome: None,
        metadata: Metadata {
            name: "partition-under-test".to_string(),
            description: String::new(),
            labels: HashMap::new(),
        },
    }
}
