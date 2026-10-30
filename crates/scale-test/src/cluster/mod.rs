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

//! The few Kubernetes reads and writes the runner needs, behind a small
//! trait. Leases, ConfigMaps and MAT Services are in the run's own namespace.

#[cfg(any(test, feature = "test-support"))]
pub mod fake;
mod kube;

use std::collections::BTreeMap;
use std::future::Future;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub use self::kube::KubeCluster;

/// A Lease as the site lock sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaseObject {
    pub name: String,
    pub resource_version: Option<String>,
    /// Set on every Lease the API returns. A replace sends it, so the API
    /// refuses the write once the Lease was deleted or made again.
    pub uid: Option<String>,
    pub holder_identity: Option<String>,
    pub lease_duration_seconds: i32,
    pub acquire_time: Option<DateTime<Utc>>,
    pub renew_time: Option<DateTime<Utc>>,
    pub annotations: BTreeMap<String, String>,
}

/// What a Lease replace did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeaseReplaced {
    /// The Lease was there and is now this one.
    Replaced(LeaseObject),
    /// No Lease was there, and the replace made this one: the API answered
    /// 201. The Lease the write meant was gone.
    Created(LeaseObject),
    /// As [`Created`](Self::Created), but the Lease made did not arrive
    /// whole in the reply.
    CreatedUnread,
}

impl LeaseReplaced {
    /// The stored Lease, when the reply carried it.
    pub fn into_lease(self) -> Option<LeaseObject> {
        match self {
            Self::Replaced(lease) | Self::Created(lease) => Some(lease),
            Self::CreatedUnread => None,
        }
    }
}

/// A Service the MAT controller made for one simulated device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatService {
    pub name: String,
    pub machine_type: Option<String>,
    /// Label `nvidia-infra-controller/mat-machine-id`. Missing on per-pod Services.
    pub machine_id: Option<String>,
    pub mat_id: Option<String>,
    /// Annotation `mat-bmc-ip`.
    pub bmc_ip: Option<IpAddr>,
    pub hardware_type: Option<String>,
    pub external_ips: Vec<IpAddr>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PodInfo {
    pub name: String,
    pub labels: BTreeMap<String, String>,
    pub annotations: BTreeMap<String, String>,
    pub phase: String,
    pub ready: bool,
    /// `metadata.creationTimestamp`.
    pub created_at: DateTime<Utc>,
    /// Sum over containers.
    pub restarts: u32,
    /// `status.podIP`.
    pub ip: Option<String>,
}

/// A ConfigMap with what a compare-and-swap write needs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConfigMapObject {
    pub name: String,
    /// `None` to create.
    pub resource_version: Option<String>,
    pub uid: Option<String>,
    pub labels: BTreeMap<String, String>,
    pub annotations: BTreeMap<String, String>,
    pub data: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamedConfigMap {
    pub name: String,
    pub labels: BTreeMap<String, String>,
    pub data: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ClusterError {
    #[error("conflict: the object changed since it was read")]
    Conflict,
    #[error("the object already exists")]
    AlreadyExists,
    #[error("forbidden: {0}")]
    Forbidden(String),
    #[error("kubernetes API error {code}: {message}")]
    Api { code: u16, message: String },
    /// No answer came: the request may or may not have reached the API.
    /// The same call may work when tried again.
    #[error("kubernetes API unreachable: {0}")]
    Transport(String),
    /// The call was refused before it was sent, or its answer is not a
    /// valid one. Trying again does not help. A write answered this way may
    /// have landed.
    #[error("kubernetes API call invalid: {0}")]
    Invalid(String),
    #[error("kubernetes API call timed out after {0} s")]
    Timeout(u64),
}

/// Every call is bounded by the implementation: callers such as the site
/// lock add no timeout of their own.
#[async_trait]
pub trait Cluster: Send + Sync {
    async fn get_lease(&self, name: &str) -> Result<Option<LeaseObject>, ClusterError>;
    /// `AlreadyExists` when another process created it first.
    async fn create_lease(&self, lease: &LeaseObject) -> Result<LeaseObject, ClusterError>;
    /// `Conflict` on a stale `resource_version`, or on a `uid` the stored
    /// Lease does not have, also when no Lease is stored. The API server
    /// makes a missing Lease on a replace with no UID: a replace that made
    /// the Lease answers [`LeaseReplaced::Created`].
    async fn replace_lease(&self, lease: &LeaseObject) -> Result<LeaseReplaced, ClusterError>;
    async fn get_config_map(
        &self,
        name: &str,
    ) -> Result<Option<BTreeMap<String, String>>, ClusterError>;
    /// Creates the ConfigMap when missing.
    async fn put_config_map_key(
        &self,
        name: &str,
        key: &str,
        value: &str,
    ) -> Result<(), ClusterError>;
    /// The ConfigMap with its resource version, UID and metadata.
    async fn get_config_map_object(
        &self,
        name: &str,
    ) -> Result<Option<ConfigMapObject>, ClusterError>;
    /// `AlreadyExists` when another process created it first.
    async fn create_config_map(
        &self,
        config_map: &ConfigMapObject,
    ) -> Result<ConfigMapObject, ClusterError>;
    /// A compare-and-swap: `Conflict` when the ConfigMap changed since
    /// `resource_version` was read. The UID, when set, must be the same.
    async fn replace_config_map(
        &self,
        config_map: &ConfigMapObject,
    ) -> Result<ConfigMapObject, ClusterError>;
    /// Services labeled `app.kubernetes.io/managed-by=mat-k8s-controller`.
    async fn list_mat_services(&self) -> Result<Vec<MatService>, ClusterError>;
    async fn list_pods(
        &self,
        namespace: &str,
        label_selector: &str,
    ) -> Result<Vec<PodInfo>, ClusterError>;
}

/// The longest any single Kubernetes call may take before it fails with
/// [`ClusterError::Timeout`].
pub const CALL_TIMEOUT: Duration = Duration::from_secs(10);

/// Bounds `call` by `limit`. A call that never answers fails, so no caller
/// waits forever on the API.
pub async fn bounded<T>(
    limit: Duration,
    call: impl Future<Output = Result<T, ClusterError>>,
) -> Result<T, ClusterError> {
    match tokio::time::timeout(limit, call).await {
        Ok(result) => result,
        Err(_) => Err(ClusterError::Timeout(limit.as_secs())),
    }
}

/// A [`Cluster`] whose every call is [`bounded`].
pub struct BoundedCluster {
    inner: Arc<dyn Cluster>,
    limit: Duration,
}

impl BoundedCluster {
    pub fn new(inner: Arc<dyn Cluster>, limit: Duration) -> Self {
        Self { inner, limit }
    }
}

#[async_trait]
impl Cluster for BoundedCluster {
    async fn get_lease(&self, name: &str) -> Result<Option<LeaseObject>, ClusterError> {
        bounded(self.limit, self.inner.get_lease(name)).await
    }

    async fn create_lease(&self, lease: &LeaseObject) -> Result<LeaseObject, ClusterError> {
        bounded(self.limit, self.inner.create_lease(lease)).await
    }

    async fn replace_lease(&self, lease: &LeaseObject) -> Result<LeaseReplaced, ClusterError> {
        bounded(self.limit, self.inner.replace_lease(lease)).await
    }

    async fn get_config_map(
        &self,
        name: &str,
    ) -> Result<Option<BTreeMap<String, String>>, ClusterError> {
        bounded(self.limit, self.inner.get_config_map(name)).await
    }

    async fn put_config_map_key(
        &self,
        name: &str,
        key: &str,
        value: &str,
    ) -> Result<(), ClusterError> {
        bounded(self.limit, self.inner.put_config_map_key(name, key, value)).await
    }

    async fn get_config_map_object(
        &self,
        name: &str,
    ) -> Result<Option<ConfigMapObject>, ClusterError> {
        bounded(self.limit, self.inner.get_config_map_object(name)).await
    }

    async fn create_config_map(
        &self,
        config_map: &ConfigMapObject,
    ) -> Result<ConfigMapObject, ClusterError> {
        bounded(self.limit, self.inner.create_config_map(config_map)).await
    }

    async fn replace_config_map(
        &self,
        config_map: &ConfigMapObject,
    ) -> Result<ConfigMapObject, ClusterError> {
        bounded(self.limit, self.inner.replace_config_map(config_map)).await
    }

    async fn list_mat_services(&self) -> Result<Vec<MatService>, ClusterError> {
        bounded(self.limit, self.inner.list_mat_services()).await
    }

    async fn list_pods(
        &self,
        namespace: &str,
        label_selector: &str,
    ) -> Result<Vec<PodInfo>, ClusterError> {
        bounded(self.limit, self.inner.list_pods(namespace, label_selector)).await
    }
}

#[cfg(test)]
mod tests {
    use super::fake::{ClusterOp, FakeCluster};
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn bounded_cluster_fails_a_call_that_never_answers() {
        let fake = Arc::new(FakeCluster::new("nico-mat"));
        fake.delay(ClusterOp::GetConfigMap, Duration::MAX);
        let cluster = BoundedCluster::new(fake.clone(), CALL_TIMEOUT);
        let started = tokio::time::Instant::now();
        assert_eq!(
            cluster.get_config_map("x").await,
            Err(ClusterError::Timeout(10))
        );
        assert_eq!(started.elapsed(), CALL_TIMEOUT);
        assert_eq!(
            cluster.get_lease("y").await,
            Ok(None),
            "other calls still answer"
        );
    }
}
