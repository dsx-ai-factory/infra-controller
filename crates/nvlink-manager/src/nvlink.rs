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

#[cfg(any(test, feature = "test-support"))]
pub mod test_support {
    use std::collections::HashSet;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use libnmxc::nmxc_model::{
        self, GetComputeNodeCountResponse, GetComputeNodeInfoListResponse, GetGpuInfoListResponse,
        GetPartitionCountResponse, GetPartitionIdListResponse, GetPartitionInfoListResponse,
        GetSwitchNodeCountResponse, GetSwitchNodeInfoListResponse,
    };
    use libnmxc::{Endpoint, Nmxc, NmxcClientPool, NmxcClientPoolBuilder, NmxcError, NmxcPool};

    use crate::config::{NmxCTlsConfigError, NvLinkConfig};

    /// Simulator endpoint URL and pool builder for `url` under `nvlink`'s NMX-C TLS settings.
    ///
    /// The TLS settings from [`crate::config::NvLinkConfig::nmx_c_tls_config`] are always
    /// attached. When TLS settings are present, an `http://` URL is upgraded to `https://`
    /// according to the production HTTPS policy. Without TLS settings, the plain simulator
    /// fallback is preserved; other URLs are kept as given.
    fn simulator_connection(
        url: &str,
        nvlink: &NvLinkConfig,
    ) -> Result<(String, NmxcClientPoolBuilder), NmxCTlsConfigError> {
        let tls = nvlink.nmx_c_tls_config()?;
        let endpoint_url = match url.strip_prefix("http://") {
            Some(rest) if tls.is_some() && nvlink.nmx_c_uses_https() => format!("https://{rest}"),
            _ => url.to_string(),
        };
        let mut builder = NmxcClientPool::builder();
        if let Some(tls) = tls {
            builder = builder.tls(tls);
        }
        Ok((endpoint_url, builder))
    }

    #[derive(Debug)]
    struct SimPartition {
        partition_id: u32,
        name: String,
        gpu_uids: Vec<u64>,
    }

    /// In-memory NMX-C gRPC API mock, mirroring [`NmxmSimClient`] partition presets for tests.
    #[derive(Debug)]
    pub struct NmxcSimClient {
        _partitions: Arc<Mutex<Vec<SimPartition>>>,
        _next_partition_id: Arc<Mutex<u32>>,
        _fail_after_n_creates: Option<Arc<Mutex<usize>>>,
        _grpc_pool: Option<NmxcClientPool>,
        _simulator_endpoint: Option<Endpoint>,
        /// `get_partition_info_list` calls across this client and the clients it created.
        _partition_info_list_calls: Arc<AtomicUsize>,
        /// GPUs served by `get_gpu_info_list`.
        _gpu_info_list: Arc<Vec<nmxc_model::GpuInfo>>,
        /// While set, `hello` fails on this client and the clients it created, simulating an
        /// NMX-C that cannot be reached.
        _hello_fails: Arc<AtomicBool>,
    }

    impl Default for NmxcSimClient {
        fn default() -> Self {
            NmxcSimClient {
                _partitions: Arc::new(Mutex::new(Vec::new())),
                _next_partition_id: Arc::new(Mutex::new(1)),
                _fail_after_n_creates: None,
                _grpc_pool: None,
                _simulator_endpoint: None,
                _partition_info_list_calls: Arc::new(AtomicUsize::new(0)),
                _gpu_info_list: Arc::new(Vec::new()),
                _hello_fails: Arc::new(AtomicBool::new(false)),
            }
        }
    }

    impl NmxcSimClient {
        /// Default simulator URL: plain gRPC on port 9601 (`http://localhost:9601`).
        pub const SIMULATOR_URL: &'static str = "http://localhost:9601";

        /// Default simulator URL when configured TLS settings require HTTPS, including mTLS.
        pub const SIMULATOR_URL_MTLS: &'static str = "https://localhost:9601";

        /// Creates a pool that proxies to the NMX-C gRPC simulator.
        pub fn simulator() -> Self {
            Self::with_simulator_url(Self::SIMULATOR_URL)
        }

        /// Like [`Self::simulator`], with the same NMX-C TLS settings as production. Uses
        /// [`Self::SIMULATOR_URL_MTLS`] when configured TLS settings require HTTPS.
        /// Returns an error if only one half of the client identity is configured.
        pub fn simulator_for_nvlink_config(
            nvlink: &NvLinkConfig,
        ) -> Result<Self, NmxCTlsConfigError> {
            Self::with_simulator_url_for_nvlink(Self::SIMULATOR_URL, nvlink)
        }

        /// Creates a pool that proxies to an NMX-C gRPC simulator URL.
        pub fn with_simulator_url(url: impl Into<String>) -> Self {
            NmxcSimClient {
                _grpc_pool: Some(
                    NmxcClientPool::builder()
                        .build()
                        .expect("NmxcClientPool::builder default"),
                ),
                _simulator_endpoint: Some(Endpoint::new(url.into()).expect("simulator URL")),
                ..Self::default()
            }
        }

        /// Like [`Self::with_simulator_url`], with `nvlink`'s NMX-C TLS settings attached. An
        /// `http://` URL is upgraded to `https://` when TLS settings are present and either
        /// `allow_insecure` is false or a client identity is configured. Other URLs are kept
        /// as given. Returns an error if only one half of the client identity is configured.
        pub fn with_simulator_url_for_nvlink(
            url: impl Into<String>,
            nvlink: &NvLinkConfig,
        ) -> Result<Self, NmxCTlsConfigError> {
            let (endpoint_url, builder) = simulator_connection(&url.into(), nvlink)?;
            Ok(NmxcSimClient {
                _grpc_pool: Some(builder.build().expect("NmxcClientPool for the simulator")),
                _simulator_endpoint: Some(
                    Endpoint::new(endpoint_url).expect("simulator endpoint URL"),
                ),
                ..Self::default()
            })
        }

        /// After n successful [`Nmxc::create_partition`] calls, further creates fail.
        pub fn with_fail_after_n_creates(n: usize) -> Self {
            NmxcSimClient {
                _fail_after_n_creates: Some(Arc::new(Mutex::new(n))),
                ..Self::default()
            }
        }

        /// Serves `gpus` from [`Nmxc::get_gpu_info_list`].
        pub fn with_gpu_info_list(gpus: Vec<nmxc_model::GpuInfo>) -> Self {
            NmxcSimClient {
                _gpu_info_list: Arc::new(gpus),
                ..Self::default()
            }
        }

        pub fn with_unknown_partition() -> Self {
            let client = Self::default();
            client.push_partition(12345, "unknown-partition", Self::default_gpu_uids());
            client
        }

        pub fn with_default_partition() -> Self {
            let client = Self::default();
            client.push_partition(32766, "default-partition", Self::default_gpu_uids());
            client
        }

        /// Makes `hello` fail (`true`) or succeed (`false`) for this client and every client
        /// it has created or will create, so a test can take NMX-C down and bring it back.
        pub fn set_hello_fails(&self, fails: bool) {
            self._hello_fails.store(fails, Ordering::SeqCst);
        }

        /// Serves one partition named `name` holding `gpu_uids`.
        pub fn with_partition(
            partition_id: u32,
            name: impl Into<String>,
            gpu_uids: Vec<u64>,
        ) -> Self {
            let client = Self::default();
            client.push_partition(partition_id, name, gpu_uids);
            client
        }

        /// The name and members of each partition currently held, in creation order.
        pub fn partitions(&self) -> Vec<(String, Vec<u64>)> {
            self._partitions
                .lock()
                .unwrap()
                .iter()
                .map(|p| (p.name.clone(), p.gpu_uids.clone()))
                .collect()
        }

        /// Number of `get_partition_info_list` calls served in memory so far.
        pub fn partition_info_list_calls(&self) -> usize {
            self._partition_info_list_calls.load(Ordering::SeqCst)
        }

        fn default_gpu_uids() -> Vec<u64> {
            vec![
                0xdb488cb17978480,
                0xdb488cb17978481,
                0xdb488cb17978482,
                0xdb488cb17978483,
                0xeb488cb17978480,
                0xeb488cb17978481,
                0xeb488cb17978482,
                0xeb488cb17978483,
                0xfb488cb17978480,
                0xfb488cb17978481,
                0xfb488cb17978482,
                0xfb488cb17978483,
            ]
        }

        fn push_partition(&self, partition_id: u32, name: impl Into<String>, gpu_uids: Vec<u64>) {
            self._partitions.lock().unwrap().push(SimPartition {
                partition_id,
                name: name.into(),
                gpu_uids,
            });
        }

        fn to_partition_info(p: &SimPartition) -> nmxc_model::PartitionInfo {
            nmxc_model::PartitionInfo {
                partition_id: Some(nmxc_model::PartitionId {
                    partition_id: p.partition_id,
                }),
                name: p.name.clone(),
                num_gpus: p.gpu_uids.len() as u32,
                gpu_location_list: vec![],
                gpu_uid_list: p.gpu_uids.clone(),
                health: nmxc_model::PartitionHealth::NmxPartitionHealthHealthy as i32,
                partition_type: nmxc_model::PartitionType::NmxPartitionTypeGpuuidBased as i32,
                num_allocated_multicast_groups: 0,
                attr: None,
            }
        }

        fn uids_from_resource_ids(ids: &[nmxc_model::GpuResourceId]) -> Vec<u64> {
            ids.iter()
                .filter_map(|r| match &r.resource_id {
                    Some(nmxc_model::gpu_resource_id::ResourceId::GpuUid(uid)) => Some(*uid),
                    _ => None,
                })
                .collect()
        }

        fn success_server_header() -> nmxc_model::ServerHeader {
            nmxc_model::ServerHeader {
                return_code: nmxc_model::StReturnCode::NmxStSuccess as i32,
                ..Default::default()
            }
        }

        /// True when `endpoint` carries a URL from `nvlink_nmxc_endpoints` (non-empty host).
        fn nvlink_nmxc_endpoint_is_set(endpoint: &Endpoint) -> bool {
            endpoint.uri.host().is_some_and(|host| !host.is_empty())
        }
    }

    #[::async_trait::async_trait]
    impl Nmxc for NmxcSimClient {
        async fn hello(&mut self, _gateway_id: &str) -> Result<nmxc_model::ServerHello, NmxcError> {
            if self._hello_fails.load(Ordering::SeqCst) {
                return Err(NmxcError::invalid_response("simulated NMX-C outage"));
            }
            Ok(nmxc_model::ServerHello {
                server_header: Some({
                    let mut header = Self::success_server_header();
                    header.domain_uuid = "ffffffff-ffff-ffff-ffff-ffffffffffff".to_string();
                    header
                }),
                components_ver: vec![],
                capabilities: vec![],
                host_os_details: String::new(),
                major_version: nmxc_model::ProtoMsgMajorVersion::ProtoMsgMajorVersion as i32,
                minor_version: nmxc_model::ProtoMsgMinorVersion::ProtoMsgMinorVersion as i32,
            })
        }

        #[allow(deprecated)]
        async fn get_domain_properties(
            &mut self,
            _context: Option<nmxc_model::Context>,
            _gateway_id: &str,
        ) -> Result<nmxc_model::DomainProperties, NmxcError> {
            Ok(nmxc_model::DomainProperties {
                server_header: Some(Self::success_server_header()),
                context: None,
                max_compute_nodes: 0,
                max_compute_nodes_per_chassis: 0,
                max_gpus_per_compute_node: 0,
                max_gpu_nv_links: 0,
                line_rate_mbps: 0,
                max_switch_nodes: 0,
                max_switch_nodes_per_chassis: 0,
                max_switches_per_switch_node: 0,
                max_switch_nv_links: 0,
                min_gpus_per_partition: 0,
                max_num_partitions: 0,
                max_num_alids: 0,
                max_multicast_groups: 0,
                max_num_ports: 0,
            })
        }

        async fn get_domain_state_info(
            &mut self,
            _context: Option<nmxc_model::Context>,
            _gateway_id: &str,
        ) -> Result<nmxc_model::DomainStateInfo, NmxcError> {
            Ok(nmxc_model::DomainStateInfo {
                server_header: Some(Self::success_server_header()),
                context: None,
                control_plane_state: 0,
                available_multicast_groups: 0,
                config_status_description: String::new(),
                nmx_controller_health: 0,
            })
        }

        async fn get_topology_info(
            &mut self,
            _context: Option<nmxc_model::Context>,
            _gateway_id: &str,
        ) -> Result<nmxc_model::FmTopologyInfo, NmxcError> {
            Ok(nmxc_model::FmTopologyInfo {
                server_header: Some(Self::success_server_header()),
                context: None,
                device_topo_info: vec![],
            })
        }

        async fn get_compute_node_count(
            &mut self,
            _req: nmxc_model::GetComputeNodeCountRequest,
        ) -> Result<GetComputeNodeCountResponse, NmxcError> {
            Ok(GetComputeNodeCountResponse {
                server_header: Some(Self::success_server_header()),
                context: None,
                num_nodes: 0,
            })
        }

        async fn get_compute_node_info_list(
            &mut self,
            _req: nmxc_model::GetComputeNodeInfoListRequest,
        ) -> Result<GetComputeNodeInfoListResponse, NmxcError> {
            Ok(GetComputeNodeInfoListResponse {
                server_header: Some(Self::success_server_header()),
                context: None,
                node_info_list: vec![],
            })
        }

        async fn get_gpu_info_list(
            &mut self,
            _req: nmxc_model::GetGpuInfoListRequest,
        ) -> Result<GetGpuInfoListResponse, NmxcError> {
            Ok(GetGpuInfoListResponse {
                server_header: Some(Self::success_server_header()),
                context: None,
                gpu_info_list: self._gpu_info_list.as_ref().clone(),
            })
        }

        async fn get_switch_node_count(
            &mut self,
            _req: nmxc_model::GetSwitchNodeCountRequest,
        ) -> Result<GetSwitchNodeCountResponse, NmxcError> {
            Ok(GetSwitchNodeCountResponse {
                server_header: Some(Self::success_server_header()),
                context: None,
                num_nodes: 0,
            })
        }

        async fn get_switch_node_info_list(
            &mut self,
            _req: nmxc_model::GetSwitchNodeInfoListRequest,
        ) -> Result<GetSwitchNodeInfoListResponse, NmxcError> {
            Ok(GetSwitchNodeInfoListResponse {
                server_header: Some(Self::success_server_header()),
                context: None,
                node_info_list: vec![],
            })
        }

        async fn get_partition_count(
            &mut self,
            _req: nmxc_model::GetPartitionCountRequest,
        ) -> Result<GetPartitionCountResponse, NmxcError> {
            let n = self._partitions.lock().unwrap().len() as u32;
            Ok(GetPartitionCountResponse {
                server_header: Some(Self::success_server_header()),
                context: None,
                num_partitions: n,
            })
        }

        async fn get_partition_id_list(
            &mut self,
            _req: nmxc_model::GetPartitionIdListRequest,
        ) -> Result<GetPartitionIdListResponse, NmxcError> {
            let parts = self._partitions.lock().unwrap();
            let partition_list = parts
                .iter()
                .map(|p| nmxc_model::Partition {
                    partition_id: Some(nmxc_model::PartitionId {
                        partition_id: p.partition_id,
                    }),
                    num_gpus: p.gpu_uids.len() as u32,
                })
                .collect();
            Ok(GetPartitionIdListResponse {
                server_header: Some(Self::success_server_header()),
                context: None,
                partition_list,
            })
        }

        async fn get_partition_info_list(
            &mut self,
            req: nmxc_model::GetPartitionInfoListRequest,
        ) -> Result<GetPartitionInfoListResponse, NmxcError> {
            self._partition_info_list_calls
                .fetch_add(1, Ordering::SeqCst);
            let parts = self._partitions.lock().unwrap();
            let partition_info_list: Vec<nmxc_model::PartitionInfo> =
                if req.partition_id_list.is_empty() {
                    parts.iter().map(Self::to_partition_info).collect()
                } else {
                    let wanted: HashSet<u32> = req
                        .partition_id_list
                        .iter()
                        .map(|p| p.partition_id)
                        .collect();
                    parts
                        .iter()
                        .filter(|p| wanted.contains(&p.partition_id))
                        .map(Self::to_partition_info)
                        .collect()
                };
            Ok(GetPartitionInfoListResponse {
                server_header: Some(Self::success_server_header()),
                context: None,
                partition_info_list,
            })
        }

        async fn create_partition(
            &mut self,
            req: nmxc_model::CreatePartitionRequest,
        ) -> Result<nmxc_model::CreatePartitionResponse, NmxcError> {
            if let Some(fail_counter) = &self._fail_after_n_creates {
                let mut fail_counter = fail_counter.lock().unwrap();
                if *fail_counter == 0 {
                    return Err(NmxcError::invalid_response("fail after n creates"));
                }
                *fail_counter -= 1;
            }
            let gpu_uids = Self::uids_from_resource_ids(&req.gpu_resource_id);
            let partition_id = if let Some(ref pid) = req.partition_id {
                pid.partition_id
            } else {
                let mut next = self._next_partition_id.lock().unwrap();
                let id = *next;
                *next = next.saturating_add(1);
                id
            };
            self._partitions.lock().unwrap().push(SimPartition {
                partition_id,
                name: req.name,
                gpu_uids,
            });
            Ok(nmxc_model::CreatePartitionResponse {
                server_header: Some(Self::success_server_header()),
                context: None,
                partition_id: Some(nmxc_model::PartitionId { partition_id }),
            })
        }

        async fn delete_partition(
            &mut self,
            req: nmxc_model::DeletePartitionRequest,
        ) -> Result<nmxc_model::DeletePartitionResponse, NmxcError> {
            let pid = req.partition_id.map(|p| p.partition_id).unwrap_or_default();
            self._partitions
                .lock()
                .unwrap()
                .retain(|p| p.partition_id != pid);
            Ok(nmxc_model::DeletePartitionResponse {
                server_header: Some(Self::success_server_header()),
                context: None,
                partition_id: Some(nmxc_model::PartitionId { partition_id: pid }),
            })
        }

        async fn add_gpus_to_partition(
            &mut self,
            req: nmxc_model::UpdatePartitionRequest,
        ) -> Result<nmxc_model::UpdatePartitionResponse, NmxcError> {
            let pid = req
                .partition_id
                .as_ref()
                .ok_or_else(|| NmxcError::invalid_response("partition_id"))?
                .partition_id;
            let mut parts = self._partitions.lock().unwrap();
            let partition = parts
                .iter_mut()
                .find(|p| p.partition_id == pid)
                .ok_or_else(|| NmxcError::invalid_response("partition not found"))?;
            for u in &req.gpu_uid {
                if !partition.gpu_uids.contains(u) {
                    partition.gpu_uids.push(*u);
                }
            }
            Ok(nmxc_model::UpdatePartitionResponse {
                server_header: Some(Self::success_server_header()),
                context: None,
                partition_id: Some(nmxc_model::PartitionId { partition_id: pid }),
            })
        }

        async fn remove_gpus_from_partition(
            &mut self,
            req: nmxc_model::UpdatePartitionRequest,
        ) -> Result<nmxc_model::UpdatePartitionResponse, NmxcError> {
            let pid = req
                .partition_id
                .as_ref()
                .ok_or_else(|| NmxcError::invalid_response("partition_id"))?
                .partition_id;
            let remove: HashSet<u64> = req.gpu_uid.iter().copied().collect();
            let mut parts = self._partitions.lock().unwrap();
            let partition = parts
                .iter_mut()
                .find(|p| p.partition_id == pid)
                .ok_or_else(|| NmxcError::invalid_response("partition not found"))?;
            partition.gpu_uids.retain(|u| !remove.contains(u));
            Ok(nmxc_model::UpdatePartitionResponse {
                server_header: Some(Self::success_server_header()),
                context: None,
                partition_id: Some(nmxc_model::PartitionId { partition_id: pid }),
            })
        }
    }

    #[::async_trait::async_trait]
    impl NmxcPool for NmxcSimClient {
        async fn create_client(&self, endpoint: Endpoint) -> Result<Box<dyn Nmxc>, NmxcError> {
            if let Some(pool) = &self._grpc_pool {
                let ep = if Self::nvlink_nmxc_endpoint_is_set(&endpoint) {
                    endpoint
                } else {
                    self._simulator_endpoint
                        .clone()
                        .expect("simulator mode must set _simulator_endpoint")
                };
                return pool.create_client(ep).await;
            }
            Ok(Box::new(NmxcSimClient {
                _partitions: self._partitions.clone(),
                _next_partition_id: self._next_partition_id.clone(),
                _fail_after_n_creates: self._fail_after_n_creates.clone(),
                _grpc_pool: self._grpc_pool.clone(),
                _simulator_endpoint: self._simulator_endpoint.clone(),
                _partition_info_list_calls: self._partition_info_list_calls.clone(),
                _gpu_info_list: self._gpu_info_list.clone(),
                _hello_fails: self._hello_fails.clone(),
            }))
        }
    }
    #[cfg(test)]
    mod nmxc_sim_client_tests {
        use std::sync::Arc;

        use libnmxc::NmxcPool;

        use super::NmxcSimClient;

        #[test]
        fn default_simulator_url_is_localhost_9601() {
            let s = NmxcSimClient::simulator();
            assert_eq!(NmxcSimClient::SIMULATOR_URL, "http://localhost:9601");
            assert_eq!(
                s._simulator_endpoint
                    .as_ref()
                    .expect("simulator endpoint should be set")
                    .uri,
                NmxcSimClient::SIMULATOR_URL.parse::<http::Uri>().unwrap(),
            );
        }

        #[test]
        fn with_simulator_url_overrides_endpoint() {
            let s = NmxcSimClient::with_simulator_url("http://127.0.0.1:19999");
            assert_eq!(
                s._simulator_endpoint
                    .as_ref()
                    .expect("simulator endpoint should be set")
                    .uri,
                "http://127.0.0.1:19999".parse::<http::Uri>().unwrap(),
            );
        }

        #[test]
        fn simulator_for_nvlink_with_mtls_uses_https_default() {
            let cfg = crate::config::NvLinkConfig {
                nmx_c_tls_client_cert_path: Some("/tmp/client.pem".to_string()),
                nmx_c_tls_client_key_path: Some("/tmp/client-key.pem".to_string()),
                ..Default::default()
            };
            let s = NmxcSimClient::simulator_for_nvlink_config(&cfg).unwrap();
            assert_eq!(
                s._simulator_endpoint
                    .as_ref()
                    .expect("simulator endpoint should be set")
                    .uri,
                NmxcSimClient::SIMULATOR_URL_MTLS
                    .parse::<http::Uri>()
                    .unwrap(),
            );
        }

        #[test]
        fn with_simulator_url_for_nvlink_upgrades_http_to_https() {
            let cfg = crate::config::NvLinkConfig {
                nmx_c_tls_client_cert_path: Some("/c".to_string()),
                nmx_c_tls_client_key_path: Some("/k".to_string()),
                ..Default::default()
            };
            let s = NmxcSimClient::with_simulator_url_for_nvlink("http://127.0.0.1:19999", &cfg)
                .unwrap();
            assert_eq!(
                s._simulator_endpoint
                    .as_ref()
                    .expect("simulator endpoint should be set")
                    .uri,
                "https://127.0.0.1:19999".parse::<http::Uri>().unwrap(),
            );
        }

        #[test]
        fn implements_nmxc_pool() {
            let _pool: Arc<dyn NmxcPool> = Arc::new(NmxcSimClient::simulator());
        }

        #[test]
        fn simulator_for_nvlink_config_propagates_invalid_tls_configuration() {
            let cfg = crate::config::NvLinkConfig {
                nmx_c_tls_client_cert_path: Some("/tls/tls.crt".to_string()),
                ..Default::default()
            };

            assert_eq!(
                NmxcSimClient::simulator_for_nvlink_config(&cfg).unwrap_err(),
                crate::config::NmxCTlsConfigError::CertificateWithoutKey,
            );
        }

        /// The simulator pool receives every configured TLS setting, including CA-only and
        /// authority-only settings without a client identity. Configured TLS settings use the
        /// production HTTPS policy, while the default simulator remains plain HTTP.
        #[test]
        fn simulator_connection_attaches_nmx_c_tls_settings() {
            use std::path::PathBuf;

            use carbide_test_support::{Check, check_values};
            use libnmxc::{NmxcClientIdentity, NmxcTlsConfig};

            use crate::config::NvLinkConfig;

            let server_only = NvLinkConfig {
                nmx_c_tls_ca_cert_path: Some("/tls/ca.crt".to_string()),
                nmx_c_tls_authority: Some("nmxc.example".to_string()),
                ..NvLinkConfig::default()
            };
            let server_only_tls = NmxcTlsConfig {
                ca_cert_path: Some(PathBuf::from("/tls/ca.crt")),
                client_identity: None,
                authority: Some("nmxc.example".to_string()),
            };
            let mutual = NvLinkConfig {
                nmx_c_tls_client_cert_path: Some("/tls/tls.crt".to_string()),
                nmx_c_tls_client_key_path: Some("/tls/tls.key".to_string()),
                allow_insecure: true,
                ..server_only.clone()
            };
            let mutual_tls = NmxcTlsConfig {
                client_identity: Some(NmxcClientIdentity {
                    cert_path: PathBuf::from("/tls/tls.crt"),
                    key_path: PathBuf::from("/tls/tls.key"),
                }),
                ..server_only_tls.clone()
            };

            check_values(
                [
                    Check {
                        scenario: "explicit HTTPS keeps the CA and authority without a client identity",
                        input: ("https://127.0.0.1:19999", server_only.clone()),
                        expect: (
                            "https://127.0.0.1:19999".to_string(),
                            Some(server_only_tls.clone()),
                        ),
                    },
                    Check {
                        scenario: "server-only TLS upgrades HTTP when insecure transport is disabled",
                        input: ("http://127.0.0.1:19999", server_only.clone()),
                        expect: (
                            "https://127.0.0.1:19999".to_string(),
                            Some(server_only_tls.clone()),
                        ),
                    },
                    Check {
                        scenario: "server-only TLS preserves HTTP when insecure transport is allowed",
                        input: (
                            "http://127.0.0.1:19999",
                            NvLinkConfig {
                                allow_insecure: true,
                                ..server_only
                            },
                        ),
                        expect: ("http://127.0.0.1:19999".to_string(), Some(server_only_tls)),
                    },
                    Check {
                        scenario: "a client identity upgrades HTTP even when insecure transport is allowed",
                        input: ("http://127.0.0.1:19999", mutual),
                        expect: ("https://127.0.0.1:19999".to_string(), Some(mutual_tls)),
                    },
                    Check {
                        scenario: "no NMX-C TLS setting leaves the pool without TLS",
                        input: ("http://127.0.0.1:19999", NvLinkConfig::default()),
                        expect: ("http://127.0.0.1:19999".to_string(), None),
                    },
                ],
                |(url, nvlink)| {
                    let (endpoint_url, builder) =
                        super::simulator_connection(url, &nvlink).unwrap();
                    (endpoint_url, builder.tls)
                },
            );
        }
    }
}
