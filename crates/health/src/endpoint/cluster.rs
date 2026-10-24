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

use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex};

use arc_swap::ArcSwap;
use carbide_uuid::rack::RackId;
use mac_address::MacAddress;
use nv_redfish::bmc_http::reqwest::Client as ReqwestClient;
use reqwest::Client as HttpClient;
use serde::Deserialize;
use serde_json::json;
use url::Url;

use crate::HealthError;
use crate::bmc::{
    BmcClient, BmcLatencyInstrumentation, CredentialProvider, bmc_latency_endpoint_labels,
};
use crate::config::ClusterEndpointSourceConfig;
use crate::endpoint::{BmcAddr, BmcCredentials, BmcEndpoint, BoxFuture, EndpointSource};
use crate::metrics::BmcLatencyMetrics;

// ── Inventory file shape ──────────────────────────────────────────────────────

#[derive(Deserialize)]
struct FileInventory {
    default_credentials: FileCredentials,
    nodes: Vec<FileNode>,
}

#[derive(Deserialize)]
struct FileCredentials {
    username: String,
    password: Option<String>,
}

#[derive(Debug, Deserialize)]
struct FileNode {
    hostname: String,
    bmc_ip: IpAddr,
    rack: String,
}

// ── Canonical internal node shape (both paths produce this) ──────────────────

struct ClusterNode {
    hostname: String,
    bmc_ip: IpAddr,
    rack: Option<String>,
    credentials: ClusterCredentials,
}

#[derive(Clone)]
enum ClusterCredentials {
    Discovered(BmcCredentials),

    // Fallback credentials may initialize a provider but cannot replace its retained snapshot.
    Fallback(BmcCredentials),
}

// ── Cluster Manager JSON RPC ────────────────────────────────────────────────
//
// The cluster manager exposes a JSON RPC API at /json/ for inventory and credential queries.
// Two calls are made:
//
//   1. cmdevice.getDevices — inventory: one entry per PhysicalNode with hostname
//      and BMC interface IP.
//
//   2. cmpart.getPartition("<partition>") — credentials: the bmcsettings sub-object
//      holds the cluster-wide BMC username and password (stored by the cluster manager daemon).
//      Default partition is "base".
//
// Exact call names and response field paths need verification against the live
// head node:
//   GET  https://<head-node>:8081/api           — lists available services + calls
//   POST https://<head-node>:8081/json/          — JSON RPC endpoint
//
// When Joab confirms the real call names, update MANAGER_DEVICE_CALL and
// MANAGER_PARTITION_CALL below, and the field extractors.

const MANAGER_DEVICE_CALL: &str = "getDevices";
const MANAGER_PARTITION_CALL: &str = "getPartition";

fn build_http() -> Result<HttpClient, HealthError> {
    HttpClient::builder().build().map_err(|e| {
        HealthError::GenericError(format!("Cluster manager HTTP client build failed: {e}"))
    })
}

async fn manager_rpc(
    http: &HttpClient,
    json_rpc_url: &Url,
    service: &str,
    call: &str,
    arg: serde_json::Value,
) -> Result<serde_json::Value, HealthError> {
    let body = json!({ "service": service, "call": call, "arg": arg });

    tracing::debug!(service, call, url = %json_rpc_url, "cluster manager JSON RPC call");

    let response = http
        .post(json_rpc_url.as_str())
        .json(&body)
        .send()
        .await
        .map_err(|e| {
            HealthError::GenericError(format!("cluster manager RPC {service}.{call} failed: {e}"))
        })?;

    if !response.status().is_success() {
        return Err(HealthError::GenericError(format!(
            "cluster manager RPC {service}.{call} returned HTTP {}",
            response.status()
        )));
    }

    response.json().await.map_err(|e| {
        HealthError::GenericError(format!(
            "cluster manager RPC {service}.{call} parse failed: {e}"
        ))
    })
}

async fn fetch_manager_credentials(
    http: &HttpClient,
    json_rpc_url: &Url,
    cfg: &ClusterEndpointSourceConfig,
) -> ClusterCredentials {
    // cmsh equivalent: partition use <partition> → bmcsettings → get username / get password
    // Password stored by the cluster manager daemon at partition level.
    let result = manager_rpc(
        http,
        json_rpc_url,
        "cmpart",
        MANAGER_PARTITION_CALL,
        json!(cfg.cluster_manager_partition),
    )
    .await;

    let raw = match result {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(
                error = %e,
                partition = %cfg.cluster_manager_partition,
                "Failed to fetch cluster manager partition credentials; retained endpoint credentials will not change"
            );

            return ClusterCredentials::Fallback(BmcCredentials::UsernamePassword {
                username: cfg.default_username.clone(),
                password: cfg.default_password.clone(),
            });
        }
    };

    tracing::debug!("Raw cluster manager partition response received");

    // Navigate to bmcsettings — try likely paths.
    // Update once live /api confirms the exact field layout.
    let bmc_settings = raw
        .pointer("/bmcSettings")
        .or_else(|| raw.pointer("/result/bmcSettings"))
        .or_else(|| raw.pointer("/data/bmcSettings"));

    let username = bmc_settings
        .and_then(|s| s.get("username").or_else(|| s.get("userName")))
        .and_then(|v| v.as_str())
        .map(str::to_string);

    let password = bmc_settings
        .and_then(|s| s.get("password").or_else(|| s.get("bmcPassword")))
        .and_then(|v| v.as_str())
        .map(str::to_string);

    match (username, password) {
        (Some(username), Some(password)) => {
            ClusterCredentials::Discovered(BmcCredentials::UsernamePassword {
                username,
                password: Some(password),
            })
        }
        (username, password) => {
            tracing::debug!(
                "Cluster manager partition response missing BMC credential fields; retained endpoint credentials will not change."
            );

            ClusterCredentials::Fallback(BmcCredentials::UsernamePassword {
                username: username.unwrap_or_else(|| cfg.default_username.clone()),
                password: password.or_else(|| cfg.default_password.clone()),
            })
        }
    }
}

fn extract_manager_devices(
    raw: &serde_json::Value,
    credentials: ClusterCredentials,
) -> Result<(Vec<ClusterNode>, bool), HealthError> {
    // The device list may be a top-level array or wrapped in result/data/items.
    // Update once live /api confirms the exact response shape.
    let items: Vec<&serde_json::Value> = if let Some(arr) = raw.as_array() {
        arr.iter().collect()
    } else {
        match ["result", "data", "items", "devices"]
            .iter()
            .find_map(|key| raw.get(key).and_then(|v| v.as_array()))
        {
            Some(arr) => arr.iter().collect(),
            None => {
                return Err(HealthError::GenericError(
                    "Cluster manager getDevices response has no recognized shape; \
                     probe /api on head node and update extract_manager_devices in cluster.rs"
                        .to_string(),
                ));
            }
        }
    };

    let item_count = items.len();
    let mut nodes = Vec::new();
    for item in items {
        let hostname = item
            .get("hostname")
            .or_else(|| item.get("name"))
            .and_then(|v| v.as_str())
            .map(str::to_string);

        // BMC IP lives in the BMC interface, not at the top level of the device.
        // cmsh: device → interfaces → use ipmi0/ilo0/rf0 → get ip
        // Try common field paths; update once confirmed.
        let bmc_ip_str = item
            .pointer("/interfaces/ipmi0/ip")
            .or_else(|| item.pointer("/interfaces/bmc/ip"))
            .or_else(|| item.pointer("/interfaces/ilo0/ip"))
            .or_else(|| item.pointer("/interfaces/rf0/ip"))
            .or_else(|| item.get("bmcAddress"))
            .or_else(|| item.get("bmcIp"))
            .or_else(|| item.get("ipmiAddress"))
            .and_then(|v| v.as_str());

        // Cluster manager category maps to our rack identifier.
        let rack = item
            .get("category")
            .or_else(|| item.get("partition"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string);

        let (Some(hostname), Some(bmc_ip_str)) = (hostname, bmc_ip_str) else {
            tracing::debug!(
                item_keys = ?item.as_object().map(|o| o.keys().cloned().collect::<Vec<_>>()),
                rack_id = rack.as_deref().map(tracing::field::display),
                "Cluster manager device entry missing hostname or BMC IP; skipping. \
                 Update field paths in extract_manager_devices once head node is probed."
            );
            continue;
        };

        let bmc_ip: IpAddr = match bmc_ip_str.parse() {
            Ok(ip) => ip,
            Err(e) => {
                tracing::warn!(
                    hostname,
                    bmc_ip_address = bmc_ip_str,
                    rack_id = rack.as_deref().map(tracing::field::display),
                    error = %e,
                    "Invalid BMC IP; skipping"
                );
                continue;
            }
        };

        nodes.push(ClusterNode {
            hostname,
            bmc_ip,
            rack,
            credentials: credentials.clone(),
        });
    }

    if nodes.is_empty() && item_count > 0 {
        return Err(HealthError::GenericError(
            "Cluster manager getDevices returned entries but none had a recognized hostname or BMC IP; \
             probe /api on head node and update extract_manager_devices in cluster.rs"
                .to_string(),
        ));
    }

    let complete = nodes.len() == item_count;

    Ok((nodes, complete))
}

// ── Source implementations ────────────────────────────────────────────────────

// Retained clients share one complete credential snapshot for each source endpoint.
struct ClusterCredentialProvider {
    credentials: ArcSwap<BmcCredentials>,
}

impl ClusterCredentialProvider {
    fn new(credentials: BmcCredentials) -> Self {
        Self {
            credentials: ArcSwap::from_pointee(credentials),
        }
    }

    fn replace(&self, credentials: BmcCredentials) {
        self.credentials.store(Arc::new(credentials));
    }
}

impl CredentialProvider for ClusterCredentialProvider {
    fn fetch_credentials<'a>(
        &'a self,
        _endpoint: &'a BmcAddr,
    ) -> BoxFuture<'a, Result<BmcCredentials, HealthError>> {
        let credentials = self.credentials.load_full();
        Box::pin(async move { Ok((*credentials).clone()) })
    }
}

/// Discovers cluster BMCs and shares credential updates with retained clients.
///
/// A manager response must supply both credential strings to refresh an existing
/// provider. Configured fallback credentials remain available for initial discovery.
pub struct ClusterEndpointSource {
    cfg: ClusterEndpointSourceConfig,
    providers: Mutex<HashMap<String, Arc<ClusterCredentialProvider>>>,
    reqwest: ReqwestClient,
    proxy_url: Option<Url>,
    cache_size: usize,
    bmc_request_concurrency: NonZeroUsize,
    bmc_latency_metrics: Option<Arc<BmcLatencyMetrics>>,
}

impl ClusterEndpointSource {
    pub fn from_config(
        cfg: ClusterEndpointSourceConfig,
        reqwest: &ReqwestClient,
        proxy_url: Option<&Url>,
        cache_size: usize,
        bmc_latency_metrics: Option<Arc<BmcLatencyMetrics>>,
    ) -> Self {
        Self::from_config_with_request_concurrency(
            cfg,
            reqwest,
            proxy_url,
            cache_size,
            NonZeroUsize::MIN,
            bmc_latency_metrics,
        )
    }

    pub(crate) fn from_config_with_request_concurrency(
        cfg: ClusterEndpointSourceConfig,
        reqwest: &ReqwestClient,
        proxy_url: Option<&Url>,
        cache_size: usize,
        bmc_request_concurrency: NonZeroUsize,
        bmc_latency_metrics: Option<Arc<BmcLatencyMetrics>>,
    ) -> Self {
        Self {
            cfg,
            providers: Mutex::new(HashMap::new()),
            reqwest: reqwest.clone(),
            proxy_url: proxy_url.cloned(),
            cache_size,
            bmc_request_concurrency,
            bmc_latency_metrics,
        }
    }

    async fn load_endpoints(&self) -> Result<Vec<Arc<BmcEndpoint>>, HealthError> {
        let (nodes, complete) = if let Some(ref cluster_manager_url) = self.cfg.cluster_manager_url
        {
            fetch_from_manager(&self.cfg, cluster_manager_url).await?
        } else {
            (read_from_file(&self.cfg)?, true)
        };

        let mut providers = self.providers.lock().map_err(|_| {
            HealthError::GenericError("Cluster credential provider mutex poisoned".to_string())
        })?;

        let endpoints = build_endpoints(self, nodes, &mut providers);

        // A partial manager inventory cannot establish that an unseen endpoint was removed.
        if complete {
            let keys: HashSet<_> = endpoints.iter().map(|endpoint| endpoint.key()).collect();
            providers.retain(|key, _| keys.contains(key));
        }

        tracing::info!(endpoint_count = endpoints.len(), "Loaded cluster endpoints");
        Ok(endpoints)
    }
}

async fn fetch_from_manager(
    cfg: &ClusterEndpointSourceConfig,
    cluster_manager_url: &Url,
) -> Result<(Vec<ClusterNode>, bool), HealthError> {
    let http = build_http()?;
    let json_rpc_url = cluster_manager_url
        .join("/json/")
        .map_err(|e| HealthError::GenericError(format!("Invalid cluster manager URL: {e}")))?;

    tracing::info!(url = %json_rpc_url, partition = %cfg.cluster_manager_partition, "Fetching cluster inventory from cluster manager");

    // Call 1: partition-level BMC credentials
    let credentials = fetch_manager_credentials(&http, &json_rpc_url, cfg).await;

    // Call 2: device inventory
    let raw = manager_rpc(
        &http,
        &json_rpc_url,
        "cmdevice",
        MANAGER_DEVICE_CALL,
        json!({"type": "PhysicalNode"}),
    )
    .await?;

    tracing::debug!(
        response_keys = ?raw.as_object().map(|o| o.keys().cloned().collect::<Vec<_>>()),
        response_is_array = raw.is_array(),
        "Raw cluster manager device response received"
    );

    let (nodes, complete) = extract_manager_devices(&raw, credentials)?;

    tracing::info!(
        loaded_node_count = nodes.len(),
        "Cluster manager device fetch complete"
    );

    Ok((nodes, complete))
}

fn read_from_file(cfg: &ClusterEndpointSourceConfig) -> Result<Vec<ClusterNode>, HealthError> {
    let contents = std::fs::read_to_string(&cfg.inventory_path).map_err(|e| {
        HealthError::GenericError(format!(
            "Failed to read cluster inventory {}: {e}",
            cfg.inventory_path.display()
        ))
    })?;

    let inventory: FileInventory = serde_json::from_str(&contents)?;

    let credentials = ClusterCredentials::Discovered(BmcCredentials::UsernamePassword {
        username: inventory.default_credentials.username,
        password: inventory.default_credentials.password,
    });

    Ok(inventory
        .nodes
        .into_iter()
        .map(|n| ClusterNode {
            hostname: n.hostname,
            bmc_ip: n.bmc_ip,
            rack: Some(n.rack).filter(|s| !s.is_empty()),
            credentials: credentials.clone(),
        })
        .collect())
}

fn build_endpoints(
    source: &ClusterEndpointSource,
    nodes: Vec<ClusterNode>,
    providers: &mut HashMap<String, Arc<ClusterCredentialProvider>>,
) -> Vec<Arc<BmcEndpoint>> {
    let mut endpoints = Vec::with_capacity(nodes.len());
    let mut observed_keys = HashSet::new();

    for node in nodes {
        let rack_id = node.rack.as_deref().map(RackId::new);

        let mac = match node.bmc_ip {
            IpAddr::V4(v4) => {
                // Retain the synthetic MAC already used in IPv4 logs and metric labels.
                let [o1, o2, o3, o4] = v4.octets();
                Some(MacAddress::new([0x02, 0x00, o1, o2, o3, o4]))
            }
            // IPv6 needs the full address; a synthetic MAC cannot distinguish all IPv6 BMCs.
            IpAddr::V6(_) => None,
        };

        let addr = BmcAddr {
            ip: node.bmc_ip,
            port: source.cfg.port,
            mac,
        };

        let key = addr
            .mac
            .map_or_else(|| format!("ip:{}", addr.ip), |mac| mac.to_string());

        let first_observation = observed_keys.insert(key.clone());

        let bmc_latency_instrumentation = source.bmc_latency_metrics.clone().map(|metrics| {
            BmcLatencyInstrumentation::new(
                metrics,
                bmc_latency_endpoint_labels(None, rack_id.as_ref()),
            )
        });

        let provider = match providers.entry(key) {
            std::collections::hash_map::Entry::Occupied(entry) => {
                let provider = entry.get();

                if first_observation
                    && let ClusterCredentials::Discovered(credentials) = node.credentials
                {
                    provider.replace(credentials);
                }

                Arc::clone(provider)
            }
            std::collections::hash_map::Entry::Vacant(entry) => {
                let (ClusterCredentials::Discovered(credentials)
                | ClusterCredentials::Fallback(credentials)) = node.credentials;

                Arc::clone(entry.insert(Arc::new(ClusterCredentialProvider::new(credentials))))
            }
        };

        let bmc = match BmcClient::new(
            source.reqwest.clone(),
            addr.clone(),
            provider,
            source.proxy_url.clone(),
            source.cache_size,
            source.bmc_request_concurrency,
            bmc_latency_instrumentation,
        ) {
            Ok(c) => Arc::new(c),
            Err(e) => {
                tracing::warn!(
                    error = ?e,
                    hostname = %node.hostname,
                    rack_id = rack_id.as_ref().map(tracing::field::display),
                    "Failed to construct BmcClient for cluster endpoint; skipping"
                );
                continue;
            }
        };
        endpoints.push(Arc::new(BmcEndpoint {
            addr,
            metadata: None,
            rack_id,
            labels: Default::default(),
            bmc,
        }));
    }
    endpoints
}

impl EndpointSource for ClusterEndpointSource {
    fn fetch_bmc_hosts<'a>(&'a self) -> BoxFuture<'a, Result<Vec<Arc<BmcEndpoint>>, HealthError>> {
        Box::pin(self.load_endpoints())
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::{Check, check_values};

    use super::*;
    use crate::endpoint::test_support::reqwest;
    use crate::sink::EventContext;

    #[tokio::test]
    async fn cluster_rotation_reaches_retained_client() {
        use std::sync::Mutex;

        use axum::Router;
        use axum::http::{HeaderMap, StatusCode};
        use axum::routing::get;
        use base64::Engine;
        use bmc_mock::test_support::serve_https;
        use nv_redfish::core::{Bmc, ODataId};
        use nv_redfish::schema::service_root::ServiceRoot;

        let pairs = [("reader-a", "synthetic-a"), ("reader-b", "synthetic-b")];

        let authorizations: Vec<_> = pairs
            .iter()
            .map(|(user, password)| {
                format!(
                    "Basic {}",
                    base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}"))
                )
            })
            .collect();

        let accepted = Arc::new(Mutex::new(0_usize));
        let observed = Arc::new(Mutex::new(Vec::new()));
        let accepted_by_server = Arc::clone(&accepted);
        let observed_by_server = Arc::clone(&observed);

        let app = Router::new().route("/{*path}", get(move |headers: HeaderMap| {
            let supplied = headers.get("authorization").and_then(|value| value.to_str().ok());
            let pair = authorizations.iter().position(|value| Some(value.as_str()) == supplied);
            let expected = *accepted_by_server.lock().expect("accepted pair lock");

            observed_by_server.lock().expect("observed pairs lock").push(pair);

            async move {
                if pair != Some(expected) {
                    return (StatusCode::UNAUTHORIZED, "{}");
                }

                (StatusCode::OK, r##"{"@odata.id":"/redfish/v1","@odata.type":"#ServiceRoot.v1_15_0.ServiceRoot","Id":"RootService","Name":"Root Service","Links":{"Sessions":{"@odata.id":"/redfish/v1/SessionService/Sessions"}},"RedfishVersion":"1.15.0"}"##)
            }
        }));
        let (mut server, base_url) = serve_https("health-cluster-rotation", app);
        let inventory = tempfile::NamedTempFile::new().expect("inventory file");

        std::fs::write(
            inventory.path(),
            json!({
                "default_credentials": {"username": pairs[0].0, "password": pairs[0].1},
                "nodes": [{"hostname":"host-a","bmc_ip":"10.2.3.4","rack":"rack-a"}],
            })
            .to_string(),
        )
        .expect("initial inventory");

        let source = ClusterEndpointSource::from_config(
            ClusterEndpointSourceConfig {
                inventory_path: inventory.path().to_path_buf(),
                ..Default::default()
            },
            &reqwest(),
            Some(&base_url),
            0,
            None,
        );

        let initial = source.fetch_bmc_hosts().await.expect("initial discovery");
        let retained = initial[0].bmc();
        retained
            .get::<ServiceRoot>(&ODataId::from("/initial".to_string()))
            .await
            .expect("initial authentication");

        std::fs::write(inventory.path(), "{malformed").expect("malformed inventory");

        assert!(source.fetch_bmc_hosts().await.is_err());
        retained
            .get::<ServiceRoot>(&ODataId::from("/after-malformed".to_string()))
            .await
            .expect("previous credentials survive malformed inventory");

        *accepted.lock().expect("accepted pair lock") = 1;

        std::fs::write(
            inventory.path(),
            json!({
                "default_credentials": {"username": pairs[1].0, "password": pairs[1].1},
                "nodes": [{"hostname":"host-a","bmc_ip":"10.2.3.4","rack":"rack-a"}],
            })
            .to_string(),
        )
        .expect("rotated inventory");

        source.fetch_bmc_hosts().await.expect("rotated discovery");

        let result = retained
            .get::<ServiceRoot>(&ODataId::from("/rotated".to_string()))
            .await;

        server.stop().await.expect("BMC server shutdown");

        assert!(
            result.is_ok(),
            "retained client authenticates with the rotated pair"
        );

        assert_eq!(
            *observed.lock().expect("observed pairs lock"),
            vec![Some(0), Some(0), Some(0), Some(1)]
        );
    }

    #[tokio::test]
    async fn manager_rotation_preserves_complete_pair_after_fallback() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        use axum::http::StatusCode;
        use axum::routing::post;
        use axum::{Json, Router};

        let phase = Arc::new(AtomicUsize::new(0));
        let server_phase = Arc::clone(&phase);

        let app = Router::new().route("/json/", post(move |Json(body): Json<serde_json::Value>| {
            let phase = server_phase.load(Ordering::SeqCst);

            async move {
                if body["service"] == "cmdevice" {
                    let devices = match phase {
                        5 => json!([{"hostname":"host-b","bmcIp":"10.2.3.5"}, {"hostname":"host-a"}]),
                        6 => json!([]),
                        _ => json!([{"hostname":"host-a","bmcIp":"10.2.3.4"}]),
                    };

                    return (StatusCode::OK, Json(devices));
                }

                match phase {
                    0 => (StatusCode::OK, Json(json!({"bmcSettings":{"username":"reader-a","password":"synthetic-a"}}))),
                    1 => (StatusCode::OK, Json(json!({"bmcSettings":{"username":"reader-b","password":"synthetic-b"}}))),
                    2 => (StatusCode::SERVICE_UNAVAILABLE, Json(json!({}))),
                    3 => (StatusCode::OK, Json(json!({"bmcSettings":{"username":"reader-c"}}))),
                    4 => (StatusCode::OK, Json(json!({"bmcSettings":{"password":"synthetic-c"}}))),
                    _ => (StatusCode::OK, Json(json!({"bmcSettings":{"username":"reader-c","password":"synthetic-c"}}))),
                }
            }
        }));

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("manager listener");

        let manager_url = Url::parse(&format!(
            "http://{}",
            listener.local_addr().expect("manager address")
        ))
        .expect("manager URL");

        let mut server = tokio::task::JoinSet::new();
        server.spawn(async move { axum::serve(listener, app).await });

        let source = ClusterEndpointSource::from_config(
            ClusterEndpointSourceConfig {
                cluster_manager_url: Some(manager_url),
                default_username: "fallback-reader".to_string(),
                default_password: Some("synthetic-fallback".to_string()),
                ..Default::default()
            },
            &reqwest(),
            None,
            0,
            None,
        );

        let initial = source.fetch_bmc_hosts().await.expect("initial inventory");
        let retained = initial[0].bmc().credential_provider();

        for (next_phase, startup_pair) in [
            (1, ("reader-b", "synthetic-b")),
            (2, ("fallback-reader", "synthetic-fallback")),
            (3, ("reader-c", "synthetic-fallback")),
            (4, ("fallback-reader", "synthetic-c")),
        ] {
            phase.store(next_phase, Ordering::SeqCst);

            let refreshed = source
                .fetch_bmc_hosts()
                .await
                .expect("valid device inventory");

            let startup_source =
                ClusterEndpointSource::from_config(source.cfg.clone(), &reqwest(), None, 0, None);

            let startup = startup_source
                .fetch_bmc_hosts()
                .await
                .expect("startup fallback inventory");

            let startup_credentials = startup[0]
                .bmc()
                .credential_provider()
                .fetch_credentials(&startup[0].addr)
                .await
                .expect("startup credential snapshot");

            assert!(
                matches!(startup_credentials, BmcCredentials::UsernamePassword { username, password } if username == startup_pair.0 && password.as_deref() == Some(startup_pair.1)),
                "phase {next_phase}: initial fallback behavior"
            );

            for provider in [retained.clone(), refreshed[0].bmc().credential_provider()] {
                let credentials = provider
                    .fetch_credentials(&initial[0].addr)
                    .await
                    .expect("provider snapshot");

                let BmcCredentials::UsernamePassword { username, password } = credentials else {
                    panic!("expected username/password")
                };

                assert!(
                    username == "reader-b" && password.as_deref() == Some("synthetic-b"),
                    "phase {next_phase}: retain the last complete manager pair"
                );
            }
        }

        phase.store(5, Ordering::SeqCst);

        let partial = source
            .fetch_bmc_hosts()
            .await
            .expect("partial device observation");

        assert_eq!(partial.len(), 1);

        assert!(
            source
                .providers
                .lock()
                .expect("provider map lock")
                .contains_key(&initial[0].key())
        );

        phase.store(6, Ordering::SeqCst);

        let removed = source
            .fetch_bmc_hosts()
            .await
            .expect("complete empty inventory");

        assert!(removed.is_empty());

        assert!(
            source
                .providers
                .lock()
                .expect("provider map lock")
                .is_empty()
        );

        server.abort_all();
        server
            .join_next()
            .await
            .expect("manager server task")
            .expect_err("manager server cancelled");
    }

    #[tokio::test]
    async fn removed_cluster_provider_remains_valid_after_source_prunes_ownership() {
        let inventory = tempfile::NamedTempFile::new().expect("inventory file");

        let source = ClusterEndpointSource::from_config(
            ClusterEndpointSourceConfig {
                inventory_path: inventory.path().to_path_buf(),
                ..Default::default()
            },
            &reqwest(),
            None,
            0,
            None,
        );

        let mut retained: Option<(Arc<dyn CredentialProvider>, BmcAddr)> = None;

        for (username, password, nodes) in [
            (
                "reader-a",
                "synthetic-a",
                json!([{"hostname":"host-a","bmc_ip":"10.2.3.4","rack":"rack-a"}]),
            ),
            ("reader-a", "synthetic-a", json!([])),
            (
                "reader-b",
                "synthetic-b",
                json!([{"hostname":"host-a","bmc_ip":"10.2.3.4","rack":"rack-a"}]),
            ),
        ] {
            std::fs::write(inventory.path(), json!({"default_credentials":{"username":username,"password":password},"nodes":nodes}).to_string()).expect("inventory observation");
            let endpoints = source.fetch_bmc_hosts().await.expect("inventory load");

            if endpoints.is_empty() {
                assert!(
                    source
                        .providers
                        .lock()
                        .expect("provider map lock")
                        .is_empty()
                );

                continue;
            }

            let endpoint = &endpoints[0];
            let provider = endpoint.bmc().credential_provider();

            if let Some((old_provider, old_addr)) = &retained {
                assert!(!Arc::ptr_eq(old_provider, &provider));

                let old_credentials = old_provider
                    .fetch_credentials(old_addr)
                    .await
                    .expect("retained provider remains valid");

                let new_credentials = provider
                    .fetch_credentials(&endpoint.addr)
                    .await
                    .expect("new provider snapshot");

                assert!(
                    matches!(old_credentials, BmcCredentials::UsernamePassword { username, password } if username == "reader-a" && password.as_deref() == Some("synthetic-a"))
                );

                assert!(
                    matches!(new_credentials, BmcCredentials::UsernamePassword { username, password } if username == "reader-b" && password.as_deref() == Some("synthetic-b"))
                );
            } else {
                retained = Some((provider, endpoint.addr.clone()));
            }
        }
    }

    #[tokio::test]
    async fn file_inventory_preserves_ipv4_keys_and_distinguishes_ipv6_addresses() {
        let cases = [
            Check {
                scenario: "IPv4 retains its synthetic MAC identity",
                input: "10.2.3.4",
                expect: (
                    Some(MacAddress::new([0x02, 0x00, 10, 2, 3, 4])),
                    "02:00:0A:02:03:04".to_string(),
                ),
            },
            Check {
                scenario: "duplicate IPv4 entries retain their source observations",
                input: "10.2.3.4",
                expect: (
                    Some(MacAddress::new([0x02, 0x00, 10, 2, 3, 4])),
                    "02:00:0A:02:03:04".to_string(),
                ),
            },
            Check {
                scenario: "IPv6 uses its full normalized address",
                input: "2001:0db8:0001:0000:0000:0000:0000:0001",
                expect: (None, "ip:2001:db8:1::1".to_string()),
            },
            Check {
                scenario: "IPv6 with the same suffix remains distinct",
                input: "2001:db8:2::1",
                expect: (None, "ip:2001:db8:2::1".to_string()),
            },
        ];
        let inventory = tempfile::NamedTempFile::new().expect("create inventory file");
        let nodes: Vec<_> = cases
            .iter()
            .enumerate()
            .map(|(index, case)| {
                json!({"hostname": format!("host-{index}"), "bmc_ip": case.input, "rack": "rack-a"})
            })
            .collect();
        std::fs::write(
            inventory.path(),
            json!({
                "default_credentials": {"username": "reader", "password": "test-password"},
                "nodes": nodes,
            })
            .to_string(),
        )
        .expect("write inventory file");
        let source = ClusterEndpointSource::from_config(
            ClusterEndpointSourceConfig {
                inventory_path: inventory.path().to_path_buf(),
                port: Some(9443),
                ..Default::default()
            },
            &reqwest(),
            None,
            10,
            None,
        );

        let endpoints = source.fetch_bmc_hosts().await.expect("load inventory");
        assert_eq!(endpoints.len(), cases.len());
        check_values(cases, |ip| {
            let ip: IpAddr = ip.parse().expect("valid inventory IP");
            let endpoint = endpoints
                .iter()
                .find(|endpoint| endpoint.addr.ip == ip)
                .expect("inventory endpoint is retained");
            (endpoint.addr.mac, endpoint.key())
        });

        for endpoint in &endpoints {
            assert_eq!(endpoint.addr.port, Some(9443));
            assert_eq!(endpoint.rack_id, Some(RackId::new("rack-a")));
            assert_eq!(endpoint.log_identity(), endpoint.key());
            assert_eq!(
                EventContext::from_endpoint(endpoint, "test").endpoint_key(),
                endpoint.key(),
            );
            let credentials = endpoint
                .bmc()
                .credential_provider()
                .fetch_credentials(&endpoint.addr)
                .await
                .expect("inventory credentials");
            let BmcCredentials::UsernamePassword { username, password } = credentials else {
                panic!("expected inventory username and password");
            };
            assert_eq!(username, "reader");
            assert_eq!(password.as_deref(), Some("test-password"));
        }
    }
}
