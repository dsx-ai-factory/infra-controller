// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use std::fmt::Debug;
use std::time::Duration;

use async_trait::async_trait;
use k8s_openapi::api::core::v1::{Node, Secret};
use kube::api::ListParams;
use kube::config::{
    AuthInfo, Cluster, Context, KubeConfigOptions, Kubeconfig, NamedAuthInfo, NamedCluster,
    NamedContext,
};
use kube::core::ObjectList;
use kube::{Api, Client, Config};
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};

use super::{DpuClusterRepository, KubeRepository, ServiceVpcClusterRepository};
use crate::DpfError;
use crate::crds::dpuservicecredentialrequests_generated::{
    DPUServiceCredentialRequest, DpuServiceCredentialRequestType,
};
use crate::crds::servicechains_generated::ServiceChain;
use crate::crds::servicechainsets_generated::ServiceChainSet;
use crate::types::DpuClusterIdentity;

// Match the management SDK's ten-second attempt budget. Credential lookup and client construction
// share one budget; each complete list has its own budget including all pages and client waits.
// No operation retries internally, so a failed page cannot become a partial successful list.
const OBSERVER_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const OBSERVER_ACCOUNT: &str = "nico-service-vpc-observer";

/// Reads remote Nodes and service chains through one DPF-issued observer client.
///
/// Lists are direct API reads with resourceVersion unset, rather than watcher snapshots. Every
/// list includes all pages from one resourceVersion within ten seconds. Callers must list chains
/// after observing the required ServiceChainSet acknowledgement, then revalidate identities.
pub struct ServiceVpcClusterReader {
    client: Client,
}

impl ServiceVpcClusterReader {
    /// Connects SDK tests to a supplied client so they can verify remote observation ordering.
    #[cfg(test)]
    pub(crate) fn with_client(client: Client) -> Self {
        Self { client }
    }

    /// Lists every remote Node so callers can detect missing or ambiguous DPU identity.
    pub async fn list_nodes(&self) -> Result<ObjectList<Node>, DpfError> {
        list_all(Api::all(self.client.clone())).await
    }

    /// Lists all ServiceChainSets in the namespace for one batched generation and identity check.
    pub async fn list_service_chain_sets(
        &self,
        namespace: &str,
    ) -> Result<ObjectList<ServiceChainSet>, DpfError> {
        list_all(Api::namespaced(self.client.clone(), namespace)).await
    }

    /// Lists all ServiceChains, including deleting or mislabeled leftovers needed for isolation.
    pub async fn list_service_chains(
        &self,
        namespace: &str,
    ) -> Result<ObjectList<ServiceChain>, DpfError> {
        // DPF 26.10 does not support a spec.node field selector; retain all children for indexing.
        list_all(Api::namespaced(self.client.clone(), namespace)).await
    }
}

#[async_trait]
impl ServiceVpcClusterRepository for KubeRepository {
    /// Reloads the DPUCluster's dedicated DPF credentials so each reader consumes token rotation.
    async fn service_vpc_cluster_reader(
        &self,
        cluster: &DpuClusterIdentity,
    ) -> Result<ServiceVpcClusterReader, DpfError> {
        tokio::time::timeout(OBSERVER_REQUEST_TIMEOUT, async {
            // Verify the actual DPUCluster before reading credentials for its namespaced name.
            if cluster.namespace.is_empty() || cluster.name.is_empty() || cluster.uid.is_empty() {
                return Err(DpfError::InvalidState(
                    "incomplete DPUCluster identity".to_owned(),
                ));
            }
            let current = DpuClusterRepository::get(self, &cluster.name, &cluster.namespace)
                .await?
                .ok_or_else(|| DpfError::not_found("DPUCluster", &cluster.name))?;
            if current.metadata.uid.as_deref() != Some(&cluster.uid)
                || current.metadata.deletion_timestamp.is_some()
            {
                return Err(DpfError::InvalidState(format!(
                    "DPUCluster {}/{} is deleting or has changed UID",
                    cluster.namespace, cluster.name
                )));
            }

            let name = observer_credential_name(cluster);
            let requests: Api<DPUServiceCredentialRequest> =
                Api::namespaced(self.client().clone(), &cluster.namespace);
            let request = requests.get(&name).await?;
            let invalid_credentials = || {
                DpfError::InvalidState(format!(
                    "observer credentials for DPUCluster {}/{} are incomplete or stale",
                    cluster.namespace, cluster.name
                ))
            };

            // DPF records the cluster UID after issuing the observer ServiceAccount's credentials.
            // A current condition for each step is required; top-level Ready alone is insufficient.
            let status = request.status.as_ref().ok_or_else(invalid_credentials)?;
            let generation = request
                .metadata
                .generation
                .filter(|value| *value > 0)
                .ok_or_else(invalid_credentials)?;
            let target = request
                .spec
                .target_cluster
                .as_ref()
                .ok_or_else(invalid_credentials)?;
            let expected_account = format!("{}/{}", cluster.namespace, OBSERVER_ACCOUNT);
            let expected_secret = format!("{}/{}", cluster.namespace, name);
            let expected_cluster = format!("{}/{}", cluster.namespace, cluster.name);
            if request.metadata.deletion_timestamp.is_some()
                || request.metadata.uid.as_deref().is_none_or(str::is_empty)
                || request
                    .metadata
                    .resource_version
                    .as_deref()
                    .is_none_or(str::is_empty)
                || !matches!(
                    request.spec.r#type,
                    DpuServiceCredentialRequestType::Kubeconfig
                )
                || target.name != cluster.name
                || target.namespace.as_deref() != Some(&cluster.namespace)
                || request.spec.service_account.name != OBSERVER_ACCOUNT
                || request.spec.service_account.namespace.as_deref() != Some(&cluster.namespace)
                || request.spec.secret.name != name
                || request.spec.secret.namespace.as_deref() != Some(&cluster.namespace)
                || status.target_cluster_uid.as_deref() != Some(&cluster.uid)
                || status.target_cluster.as_deref() != Some(&expected_cluster)
                || status.service_account.as_deref() != Some(&expected_account)
                || status.secret.as_deref() != Some(&expected_secret)
                || status
                    .expiration_timestamp
                    .as_deref()
                    .and_then(|time| chrono::DateTime::parse_from_rfc3339(time).ok())
                    .is_none_or(|time| time <= chrono::Utc::now())
                || !["ServiceAccountReconciled", "SecretReconciled"]
                    .iter()
                    .all(|expected| {
                        status
                            .conditions
                            .as_deref()
                            .unwrap_or_default()
                            .iter()
                            .any(|condition| {
                                condition.type_ == *expected
                                    && condition.status == "True"
                                    && condition.observed_generation == Some(generation)
                            })
                    })
            {
                return Err(invalid_credentials());
            }

            // Read only this request's Secret; DPUCluster.spec.kubeconfig contains admin credentials.
            let secrets: Api<Secret> = Api::namespaced(self.client().clone(), &cluster.namespace);
            let secret = secrets.get(&name).await?;
            let labels = secret
                .metadata
                .labels
                .as_ref()
                .ok_or_else(invalid_credentials)?;
            if secret.metadata.deletion_timestamp.is_some()
                || secret.metadata.uid.as_deref().is_none_or(str::is_empty)
                || secret
                    .metadata
                    .resource_version
                    .as_deref()
                    .is_none_or(str::is_empty)
                || labels
                    .get("svc.dpu.nvidia.com/managed-by-controller")
                    .map(String::as_str)
                    != Some("dpuservicecredentialrequestcontroller")
                || labels.get("svc.dpu.nvidia.com/credential-request-name") != Some(&name)
                || labels.get("svc.dpu.nvidia.com/credential-request-namespace")
                    != Some(&cluster.namespace)
            {
                return Err(invalid_credentials());
            }
            let data = secret
                .data
                .as_ref()
                .and_then(|data| data.get("kubeconfig"))
                .ok_or_else(invalid_credentials)?;
            let kubeconfig = observer_kubeconfig(&data.0, &cluster.namespace)?;
            let config = Config::from_custom_kubeconfig(kubeconfig, &KubeConfigOptions::default())
                .await
                .map_err(DpfError::ObserverKubeconfig)?;
            Ok(ServiceVpcClusterReader {
                client: Client::try_from(config)?,
            })
        })
        .await
        .map_err(|_| {
            DpfError::timeout(
                "open service-VPC observer client",
                "ten-second credential lookup deadline exceeded",
            )
        })?
    }
}

/// Matches the Helm credential naming rule while keeping names stable across DPUCluster recreation.
fn observer_credential_name(cluster: &DpuClusterIdentity) -> String {
    let digest = hex::encode(Sha256::digest(format!(
        "{}/{}",
        cluster.namespace, cluster.name
    )));
    format!("{OBSERVER_ACCOUNT}-{}", &digest[..16])
}

/// Selects DPF's inline CA and bearer token without loading files or executing kubeconfig plugins.
fn observer_kubeconfig(data: &[u8], namespace: &str) -> Result<Kubeconfig, DpfError> {
    let invalid = || {
        DpfError::InvalidState(
            "observer kubeconfig does not identify the DPF-issued ServiceAccount".to_owned(),
        )
    };
    let text = std::str::from_utf8(data).map_err(|_| invalid())?;
    let supplied = Kubeconfig::from_yaml(text).map_err(DpfError::ObserverKubeconfig)?;
    let ([context], [cluster], [account]) = (
        supplied.contexts.as_slice(),
        supplied.clusters.as_slice(),
        supplied.auth_infos.as_slice(),
    ) else {
        return Err(invalid());
    };
    let connection = cluster.cluster.as_ref().ok_or_else(invalid)?;
    let authentication = account.auth_info.as_ref().ok_or_else(invalid)?;
    let selected = context.context.as_ref().ok_or_else(invalid)?;
    if supplied.current_context.as_deref() != Some(OBSERVER_ACCOUNT)
        || context.name != OBSERVER_ACCOUNT
        || account.name != OBSERVER_ACCOUNT
        || selected.user.as_deref() != Some(OBSERVER_ACCOUNT)
        || selected.namespace.as_deref() != Some(namespace)
        || selected.cluster != cluster.name
        || !connection
            .server
            .as_deref()
            .is_some_and(|server| server.starts_with("https://"))
        || connection
            .certificate_authority_data
            .as_deref()
            .is_none_or(str::is_empty)
        || authentication.token.is_none()
    {
        return Err(invalid());
    }

    // Rebuild only the fields DPF emits, excluding impersonation, client certificates and plugins.
    Ok(Kubeconfig {
        api_version: Some("v1".to_owned()),
        kind: Some("Config".to_owned()),
        current_context: Some(OBSERVER_ACCOUNT.to_owned()),
        clusters: vec![NamedCluster {
            name: cluster.name.clone(),
            cluster: Some(Cluster {
                server: connection.server.clone(),
                certificate_authority_data: connection.certificate_authority_data.clone(),
                ..Default::default()
            }),
        }],
        auth_infos: vec![NamedAuthInfo {
            name: OBSERVER_ACCOUNT.to_owned(),
            auth_info: Some(AuthInfo {
                token: authentication.token.clone(),
                ..Default::default()
            }),
        }],
        contexts: vec![NamedContext {
            name: OBSERVER_ACCOUNT.to_owned(),
            context: Some(Context {
                cluster: cluster.name.clone(),
                user: Some(OBSERVER_ACCOUNT.to_owned()),
                namespace: Some(namespace.to_owned()),
                ..Default::default()
            }),
        }],
        ..Default::default()
    })
}

/// Collects one authoritative API snapshot, rejecting incomplete or inconsistent paginated lists.
async fn list_all<K>(api: Api<K>) -> Result<ObjectList<K>, DpfError>
where
    K: Clone + Debug + DeserializeOwned,
{
    tokio::time::timeout(OBSERVER_REQUEST_TIMEOUT, async {
        // Leave resourceVersion unset for a most-recent API read; "0" permits older cached data.
        let mut params = ListParams::default().limit(500);
        let mut result = api.list(&params).await?;
        if result
            .metadata
            .resource_version
            .as_deref()
            .is_none_or(str::is_empty)
        {
            return Err(DpfError::InvalidState(
                "observer list has no resourceVersion".to_owned(),
            ));
        }
        while let Some(token) = result
            .metadata
            .continue_
            .take()
            .filter(|value| !value.is_empty())
        {
            params.continue_token = Some(token);
            let page = api.list(&params).await?;
            if page.metadata.resource_version != result.metadata.resource_version {
                return Err(DpfError::InvalidState(
                    "observer list changed resourceVersion between pages".to_owned(),
                ));
            }
            result.items.extend(page.items);
            result.metadata.continue_ = page.metadata.continue_;
        }
        result.metadata.remaining_item_count = None;
        Ok(result)
    })
    .await
    .map_err(|_| {
        DpfError::timeout(
            "list service-VPC observer resources",
            "ten-second complete-list deadline exceeded",
        )
    })?
}

#[cfg(test)]
mod tests;
