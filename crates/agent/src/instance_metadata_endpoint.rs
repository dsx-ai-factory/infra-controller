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

use ::rpc::forge_tls_client::ForgeClientConfig;
use arc_swap::ArcSwapOption;
use async_trait::async_trait;
use axum::Router;
use axum::extract::{Path, State};
use axum::http::header::HeaderMap;
use axum::http::{StatusCode, Uri};
use axum::response::Response;
use axum::routing::{get, post};
use carbide_host_support::agent_config::MachineIdentityConfig;
use carbide_uuid::machine::DpuMachineId;
use eyre::eyre;
use forge_dpu_agent_utils::utils::create_forge_client;
use forge_dpu_fmds_shared::machine_identity::{
    self, MetaDataIdentityOutcome, MetaDataIdentitySigner, forward_sign_proxy_if_ready,
    sign_machine_identity_with_forge, wait_identity_rate_limit_permit,
};
use governor::middleware::NoOpMiddleware;
use governor::state::{InMemoryState, NotKeyed};
use governor::{Quota, RateLimiter, clock};
use mockall::automock;
use nonzero_ext::nonzero;
use rpc::forge::{self, ManagedHostNetworkConfigResponse};

use crate::periodic_config_fetcher::InstanceMetadata;
use crate::util::phone_home;

const PUBLIC_IPV4_CATEGORY: &str = "public-ipv4";
const PUBLIC_IPV6_CATEGORY: &str = "public-ipv6";
const HOSTNAME_CATEGORY: &str = "hostname";
const INSTANCE_NAME_CATEGORY: &str = "instance-name";
const SITENAME_CATEGORY: &str = "sitename";
const USER_DATA_CATEGORY: &str = "user-data";
const META_DATA_CATEGORY: &str = "meta-data";
const GUID: &str = "guid";
const IB_PARTITION: &str = "partition";
const LID: &str = "lid";
const PHONE_HOME_RATE_LIMIT: Quota = Quota::per_minute(nonzero!(10u32));
const DEVICES_CATEGORY: &str = "devices";
const INFINIBAND_CATEGORY: &str = "infiniband";
const MACHINE_ID_CATEGORY: &str = "machine-id";
const INSTANCE_ID_CATEGORY: &str = "instance-id";
const PHONE_HOME_CATEGORY: &str = "phone_home";
const ASN_CATEGORY: &str = "asn";

#[automock]
#[async_trait]
pub(super) trait InstanceMetadataRouterState: Sync + Send {
    fn read(
        &self,
    ) -> (
        Option<Arc<InstanceMetadata>>,
        Option<Arc<ManagedHostNetworkConfigResponse>>,
    );
    async fn phone_home(&self) -> Result<(), eyre::Error>;

    /// Calls Carbide `SignMachineIdentity` gRPC using the agent's DPU TLS client identity.
    /// Carbide maps the DPU SPIFFE ID to the owning host when resolving the active instance and
    /// tenant identity config; the issued JWT `sub` still reflects the caller's machine ID.
    ///
    /// **Note:** `GET …/meta-data/identity` does not use this trait method directly; it goes through
    /// [`Self::serve_meta_data_identity`], which enforces the identity rate limit before Carbide or
    /// before invoking this method indirectly.
    async fn sign_machine_identity(
        &self,
        audiences: Vec<String>,
    ) -> Result<forge::MachineIdentityResponse, tonic::Status>;

    /// SPIFFE machine-identity over IMDS-style HTTP (`GET …/meta-data/identity`).
    async fn serve_meta_data_identity(&self, uri: Uri, headers: HeaderMap) -> Response;
}

pub(super) struct InstanceMetadataRouterStateImpl {
    latest_instance_data: ArcSwapOption<InstanceMetadata>,
    latest_network_config: ArcSwapOption<ManagedHostNetworkConfigResponse>,
    machine_id: DpuMachineId,
    forge_api: String,
    forge_client_config: Arc<ForgeClientConfig>,
    outbound_governor:
        Arc<RateLimiter<NotKeyed, InMemoryState, clock::DefaultClock, NoOpMiddleware>>,
    /// Rate limits, Forge/sign-proxy timeouts, and optional HTTP sign-proxy client for
    /// `GET …/meta-data/identity`.
    identity_serving: Arc<machine_identity::MachineIdentityServing>,
}

#[async_trait]
impl InstanceMetadataRouterState for InstanceMetadataRouterStateImpl {
    /// Reads the latest desired instance metadata obtained from the Forge
    /// Site controller
    fn read(
        &self,
    ) -> (
        Option<Arc<InstanceMetadata>>,
        Option<Arc<ManagedHostNetworkConfigResponse>>,
    ) {
        (
            self.latest_instance_data.load_full(),
            self.latest_network_config.load_full(),
        )
    }

    // Phones home to the site controller.
    async fn phone_home(&self) -> Result<(), eyre::Error> {
        match self.outbound_governor.clone().check() {
            Ok(_) => {}
            Err(e) => return Err(eyre!("rate limit exceeded for phone_home; {}\n", e)),
        };

        let mut client = create_forge_client(&self.forge_api, &self.forge_client_config).await?;

        let timestamp = phone_home(&mut client, &self.machine_id).await?;

        tracing::info!(
            machine_id = %self.machine_id,
            %timestamp,
            "Successfully phoned home"
        );

        Ok(())
    }

    async fn sign_machine_identity(
        &self,
        audiences: Vec<String>,
    ) -> Result<forge::MachineIdentityResponse, tonic::Status> {
        sign_machine_identity_with_forge(
            &self.forge_api,
            &self.forge_client_config,
            self.identity_serving.forge_call_timeout,
            audiences,
        )
        .await
    }

    async fn serve_meta_data_identity(&self, uri: Uri, headers: HeaderMap) -> Response {
        machine_identity::serve_meta_data_identity(self, uri, headers).await
    }
}

#[async_trait]
impl MetaDataIdentitySigner for InstanceMetadataRouterStateImpl {
    async fn wait_identity_permit(&self) -> Result<(), tonic::Status> {
        self.wait_identity_governor().await
    }

    async fn machine_identity_response(
        &self,
        uri: &Uri,
        headers: &HeaderMap,
        audiences: Vec<String>,
    ) -> Result<MetaDataIdentityOutcome, tonic::Status> {
        if let Some(resp) = forward_sign_proxy_if_ready(
            self.identity_serving.sign_proxy_base.as_deref(),
            self.identity_serving.sign_proxy_http_client.as_ref(),
            uri,
            headers,
        )
        .await
        {
            return Ok(MetaDataIdentityOutcome::HttpProxy(resp));
        }

        let resp = InstanceMetadataRouterState::sign_machine_identity(self, audiences).await?;
        Ok(MetaDataIdentityOutcome::Forge(resp))
    }
}

impl InstanceMetadataRouterStateImpl {
    /// Wait for a `meta-data/identity` rate-limit permit (governor).
    async fn wait_identity_governor(&self) -> Result<(), tonic::Status> {
        wait_identity_rate_limit_permit(
            &self.identity_serving.governor,
            self.identity_serving.wait_timeout,
        )
        .await
        .map_err(|_| {
            tonic::Status::resource_exhausted(
                "timed out waiting for machine-identity rate limit capacity (machine-identity.wait-timeout-secs)",
            )
        })
    }

    pub(super) fn new(
        machine_id: DpuMachineId,
        forge_api: String,
        forge_client_config: Arc<ForgeClientConfig>,
        machine_identity: MachineIdentityConfig,
    ) -> Result<Self, String> {
        let params = machine_identity::MachineIdentityParams::try_from_limits(
            machine_identity.requests_per_second,
            machine_identity.burst,
            machine_identity.wait_timeout_secs,
            machine_identity.sign_timeout_secs,
            machine_identity.sign_proxy_url.as_deref(),
            machine_identity.sign_proxy_tls_root_ca.as_deref(),
        )?;
        let serving = Arc::new(machine_identity::MachineIdentityServing::try_from_params(
            params,
        )?);
        Ok(Self {
            latest_instance_data: ArcSwapOption::new(None),
            latest_network_config: ArcSwapOption::new(None),
            machine_id,
            forge_api,
            forge_client_config,
            outbound_governor: Arc::new(RateLimiter::direct(PHONE_HOME_RATE_LIMIT)),
            identity_serving: serving,
        })
    }

    /// Updates the instance metadata that should be served by FMDS
    pub(super) fn update_instance_data(&self, instance_data: Option<Arc<InstanceMetadata>>) {
        self.latest_instance_data.store(instance_data);
    }

    pub(super) fn update_network_configuration(
        &self,
        network_config: Option<Arc<ManagedHostNetworkConfigResponse>>,
    ) {
        self.latest_network_config.store(network_config);
    }
}

pub(super) fn get_fmds_router(
    metadata_router_state: Arc<dyn InstanceMetadataRouterState>,
) -> Router {
    let user_data_router =
        Router::new().route(&format!("/{USER_DATA_CATEGORY}"), get(get_userdata));

    // TODO add handling for non-supported URIs
    let ib_router = Router::new()
        .route(&format!("/{DEVICES_CATEGORY}"), get(get_devices))
        .route(
            &format!("/{DEVICES_CATEGORY}/{{device}}"),
            get(get_instances),
        )
        .nest(
            &format!("/{DEVICES_CATEGORY}/{{device}}"),
            Router::new()
                .route("/instances", get(get_instances))
                .route("/instances/{instance}", get(get_instance_attributes))
                .route(
                    "/instances/{instance}/{attribute}",
                    get(get_instance_attribute),
                ),
        );

    let service_router = Router::new()
        .nest(&format!("/{INFINIBAND_CATEGORY}"), ib_router)
        .route(&format!("/{PHONE_HOME_CATEGORY}"), post(post_phone_home))
        .route(&format!("/{INSTANCE_ID_CATEGORY}"), get(get_instance_id))
        .route(&format!("/{MACHINE_ID_CATEGORY}"), get(get_machine_id))
        .route(
            &format!("/{}", machine_identity::META_DATA_IDENTITY_CATEGORY),
            get(get_metadata_identity),
        )
        .route("/{category}", get(get_metadata_parameter));

    let metadata_router = Router::new()
        // The additional ending slash is a cloud init issue as found when looking at the cloud init src
        // https://bugs.launchpad.net/cloud-init/+bug/1356855
        .route(&format!("/{META_DATA_CATEGORY}/"), get(get_metadata_params))
        .route(&format!("/{META_DATA_CATEGORY}"), get(get_metadata_params))
        .nest(&format!("/{META_DATA_CATEGORY}"), service_router);

    Router::new()
        .merge(metadata_router)
        .merge(user_data_router)
        .with_state(metadata_router_state)
}

async fn get_metadata_identity(
    State(state): State<Arc<dyn InstanceMetadataRouterState>>,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    state.serve_meta_data_identity(uri, headers).await
}

async fn get_metadata_parameter(
    State(state): State<Arc<dyn InstanceMetadataRouterState>>,
    Path(category): Path<String>,
) -> (StatusCode, String) {
    extract_metadata(category, state)
}

async fn get_userdata(
    State(state): State<Arc<dyn InstanceMetadataRouterState>>,
) -> (StatusCode, String) {
    extract_metadata(USER_DATA_CATEGORY.to_string(), state)
}

fn extract_metadata(
    category: String,
    state: Arc<dyn InstanceMetadataRouterState>,
) -> (StatusCode, String) {
    let (instance_meta, network_config) = state.read();
    if let (Some(metadata), Some(network_config)) =
        (instance_meta.as_ref(), network_config.as_ref())
    {
        match category.as_str() {
            PUBLIC_IPV4_CATEGORY => (StatusCode::OK, metadata.public_addresses.ipv4_string()),
            PUBLIC_IPV6_CATEGORY => (StatusCode::OK, metadata.public_addresses.ipv6_string()),
            HOSTNAME_CATEGORY => (StatusCode::OK, metadata.hostname.clone()),
            INSTANCE_NAME_CATEGORY => match &metadata.instance_name {
                Some(instance_name) if !instance_name.is_empty() => {
                    (StatusCode::OK, instance_name.clone())
                }
                _ => (
                    StatusCode::NOT_FOUND,
                    "instance name not available".to_string(),
                ),
            },
            SITENAME_CATEGORY => (
                StatusCode::OK,
                metadata.sitename.clone().unwrap_or(String::new()),
            ),
            USER_DATA_CATEGORY => (StatusCode::OK, metadata.user_data.clone()),
            ASN_CATEGORY => (StatusCode::OK, network_config.asn.to_string()),
            _ => (
                StatusCode::NOT_FOUND,
                format!("metadata category not found: {category}"),
            ),
        }
    } else {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "metadata currently unavailable".to_string(),
        )
    }
}

async fn get_machine_id(
    State(state): State<Arc<dyn InstanceMetadataRouterState>>,
) -> (StatusCode, String) {
    let metadata = match state.read().0 {
        Some(metadata) => metadata,
        None => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "metadata currently unavailable".to_string(),
            );
        }
    };

    if let Some(machine_id) = &metadata.machine_id {
        (StatusCode::OK, machine_id.to_string())
    } else {
        (
            StatusCode::NOT_FOUND,
            "machine id not available".to_string(),
        )
    }
}

async fn get_instance_id(
    State(state): State<Arc<dyn InstanceMetadataRouterState>>,
) -> (StatusCode, String) {
    let metadata = match state.read().0 {
        Some(metadata) => metadata,
        None => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "metadata currently unavailable".to_string(),
            );
        }
    };

    if let Some(instance_id) = &metadata.instance_id {
        (StatusCode::OK, instance_id.to_string())
    } else {
        (
            StatusCode::NOT_FOUND,
            "instance id not available".to_string(),
        )
    }
}

async fn get_metadata_params(
    State(_state): State<Arc<dyn InstanceMetadataRouterState>>,
) -> (StatusCode, String) {
    (
        StatusCode::OK,
        [
            HOSTNAME_CATEGORY,
            INSTANCE_NAME_CATEGORY,
            SITENAME_CATEGORY,
            MACHINE_ID_CATEGORY,
            INSTANCE_ID_CATEGORY,
            ASN_CATEGORY,
            PUBLIC_IPV4_CATEGORY,
            PUBLIC_IPV6_CATEGORY,
        ]
        .join("\n"),
    )
}

async fn get_devices(
    State(state): State<Arc<dyn InstanceMetadataRouterState>>,
) -> (StatusCode, String) {
    let metadata = match state.read().0 {
        Some(metadata) => metadata,
        None => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "metadata currently unavailable".to_string(),
            );
        }
    };

    let mut response = String::new();
    if let Some(devices) = &metadata.ib_devices {
        for (index, device) in devices.iter().enumerate() {
            response.push_str(&format!("{}={}\n", index, device.pf_guid));
        }

        (StatusCode::OK, response)
    } else {
        (StatusCode::NOT_FOUND, "devices not available".to_string())
    }
}

async fn get_instances(
    State(state): State<Arc<dyn InstanceMetadataRouterState>>,
    Path(device_index): Path<usize>,
) -> (StatusCode, String) {
    let metadata = match state.read().0 {
        Some(metadata) => metadata,
        None => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "metadata currently unavailable".to_string(),
            );
        }
    };

    if let Some(devices) = &metadata.ib_devices {
        if devices.len() <= device_index {
            return (
                StatusCode::NOT_FOUND,
                format!("no device at index: {device_index}"),
            );
        }
        let dev = &devices[device_index];

        let mut response = String::new();
        for (index, instance) in dev.instances.iter().enumerate() {
            match &instance.ib_guid {
                Some(guid) => response.push_str(&format!("{index}={guid}\n")),
                None => continue,
            }
        }

        (StatusCode::OK, response)
    } else {
        (StatusCode::NOT_FOUND, "devices not available".to_string())
    }
}

async fn get_instance_attributes(
    State(state): State<Arc<dyn InstanceMetadataRouterState>>,
    Path((device_index, instance_index)): Path<(usize, usize)>,
) -> (StatusCode, String) {
    let read_guard = state.read();
    let metadata = match read_guard.0.as_ref() {
        Some(metadata) => metadata,
        None => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "metadata currently unavailable".to_string(),
            );
        }
    };

    if let Some(devices) = &metadata.ib_devices {
        if devices.len() <= device_index {
            return (
                StatusCode::NOT_FOUND,
                format!("no device at index: {device_index}"),
            );
        }

        let dev = &devices[device_index];

        if dev.instances.len() <= instance_index {
            return (
                StatusCode::NOT_FOUND,
                format!("no instance at index: {instance_index}"),
            );
        }
        let inst = &dev.instances[instance_index];

        let mut response = String::new();

        if let Some(_ib_guid) = &inst.ib_guid {
            response += &(GUID.to_owned() + "\n")
        }
        if let Some(_ib_partition_id) = &inst.ib_partition_id {
            response += &(IB_PARTITION.to_owned() + "\n")
        }
        response.push_str(LID);

        (StatusCode::OK, response)
    } else {
        (StatusCode::NOT_FOUND, "devices not available".to_string())
    }
}

async fn get_instance_attribute(
    State(state): State<Arc<dyn InstanceMetadataRouterState>>,
    Path((device_index, instance_index, attribute)): Path<(usize, usize, String)>,
) -> (StatusCode, String) {
    let read_guard = state.read();
    let metadata = match read_guard.0.as_ref() {
        Some(metadata) => metadata,
        None => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "metadata currently unavailable".to_string(),
            );
        }
    };

    if let Some(devices) = &metadata.ib_devices {
        if devices.len() <= device_index {
            return (
                StatusCode::NOT_FOUND,
                format!("no device at index: {device_index}"),
            );
        }
        let dev = &devices[device_index];

        if dev.instances.len() <= instance_index {
            return (
                StatusCode::NOT_FOUND,
                format!("no instance at index: {instance_index}"),
            );
        }
        let inst = &dev.instances[instance_index];

        match attribute.as_str() {
            GUID => match &inst.ib_guid {
                Some(guid) => (StatusCode::OK, guid.clone()),
                None => (
                    StatusCode::NOT_FOUND,
                    format!("guid not found at index: {instance_index}"),
                ),
            },
            IB_PARTITION => match &inst.ib_partition_id {
                Some(ib_partition_id) => (StatusCode::OK, ib_partition_id.to_string()),
                None => (
                    StatusCode::NOT_FOUND,
                    format!("ib partition not found at index: {instance_index}"),
                ),
            },
            LID => (StatusCode::OK, inst.lid.to_string()),
            _ => (StatusCode::NOT_FOUND, "no such attribute".to_string()),
        }
    } else {
        (StatusCode::NOT_FOUND, "devices not available".to_string())
    }
}

async fn post_phone_home(
    State(state): State<Arc<dyn InstanceMetadataRouterState>>,
) -> (StatusCode, String) {
    match state.phone_home().await {
        Ok(()) => (StatusCode::OK, "successfully phoned home\n".to_string()),
        Err(err) => (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, Ipv6Addr};

    use axum::http;
    use http_body_util::{BodyExt, Full};
    use hyper::body::Bytes;
    use hyper_util::rt::TokioExecutor;
    use uuid::uuid;

    use super::*;
    use crate::periodic_config_fetcher::{
        IBDeviceConfig, IBInstanceConfig, InstanceMetadata, PublicAddresses,
    };

    async fn setup_server(
        metadata: Option<InstanceMetadata>,
        network_config: Option<ManagedHostNetworkConfigResponse>,
    ) -> (tokio::task::JoinHandle<()>, u16) {
        let metadata = metadata.map(Arc::new);
        let network_config = network_config.map(Arc::new);
        let mut mock_router_state = MockInstanceMetadataRouterState::new();
        mock_router_state
            .expect_read()
            .return_const((metadata.clone(), network_config.clone()));

        let arc_mock_router_state = Arc::new(mock_router_state);

        let router = get_fmds_router(arc_mock_router_state);

        let addr = std::net::SocketAddr::from(([127, 0, 0, 1], 0));
        let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
        let server_port = listener.local_addr().unwrap().port();
        let std_listener = listener.into_std().unwrap();

        let server = tokio::spawn(async move {
            axum_server::from_tcp(std_listener)
                // Safety: This only fails if the socket is blocking, but it started as a tokio
                // TcpListener which sets non-blocking by default.
                .expect("BUG: Could not bind to listener")
                .serve(router.into_make_service())
                .await
                .unwrap();
        });

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        (server, server_port)
    }

    async fn send_request_and_check_response(
        port: u16,
        path: &str,
        expected_body: &str,
        expected_code: http::StatusCode,
    ) {
        let client = hyper_util::client::legacy::Client::builder(TokioExecutor::new()).build_http();
        let request: hyper::Request<Full<Bytes>> = hyper::Request::builder()
            .method(hyper::Method::GET)
            .uri(format!("http://127.0.0.1:{port}/{path}"))
            .body("".into())
            .unwrap();

        let response = client.request(request).await.unwrap();

        assert_eq!(response.status(), expected_code);

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let body_str = std::str::from_utf8(&body).unwrap();

        assert_eq!(body_str, expected_body);
    }

    #[tokio::test]
    async fn test_get_metadata_parameter_public_ip_categories() {
        let metadata = InstanceMetadata {
            instance_id: Some(uuid!("67e55044-10b1-426f-9247-bb680e5fe0c8").into()),
            machine_id: Some(
                "fm100ht6n80e7do39u8gmt7cvhm89pb32st9ngevgdolu542l1nfa4an0rg"
                    .parse()
                    .unwrap(),
            ),
            public_addresses: PublicAddresses {
                ipv4: Some(Ipv4Addr::LOCALHOST),
                ipv6: Some(Ipv6Addr::LOCALHOST),
            },
            hostname: "localhost".to_string(),
            instance_name: Some("test-instance".to_string()),
            user_data: "\"userData\": {\"data\": 0}".to_string(),
            ib_devices: None,
            config_version: "V2-T1666644937962267".parse().unwrap(),
            network_config_version: "V1-T1666644937952267".parse().unwrap(),
            sitename: Some("testsite".to_string()),
            extension_service_version: "V1-T1666644937952267".parse().unwrap(),
        };

        let (server, server_port) = setup_server(
            Some(metadata.clone()),
            Some(ManagedHostNetworkConfigResponse::default()),
        )
        .await;
        send_request_and_check_response(
            server_port,
            "meta-data/public-ipv4",
            "127.0.0.1",
            StatusCode::OK,
        )
        .await;
        send_request_and_check_response(
            server_port,
            "meta-data/public-ipv6",
            "::1",
            StatusCode::OK,
        )
        .await;
        server.abort();

        let metadata = InstanceMetadata {
            public_addresses: PublicAddresses::default(),
            ..metadata
        };
        let (server, server_port) = setup_server(
            Some(metadata),
            Some(ManagedHostNetworkConfigResponse::default()),
        )
        .await;
        send_request_and_check_response(server_port, "meta-data/public-ipv4", "", StatusCode::OK)
            .await;
        send_request_and_check_response(server_port, "meta-data/public-ipv6", "", StatusCode::OK)
            .await;
        server.abort();
    }

    #[tokio::test]
    async fn test_get_metadata_parameter_hostname_and_instance_name_categories() {
        let mut metadata = InstanceMetadata {
            instance_id: None,
            machine_id: Some(
                "fm100ht6n80e7do39u8gmt7cvhm89pb32st9ngevgdolu542l1nfa4an0rg"
                    .parse()
                    .unwrap(),
            ),
            public_addresses: PublicAddresses {
                ipv4: Some(Ipv4Addr::LOCALHOST),
                ipv6: None,
            },
            hostname: "localhost".to_string(),
            instance_name: Some("test-instance".to_string()),
            user_data: "\"userData\": {\"data\": 0}".to_string(),
            ib_devices: None,
            config_version: "V2-T1666644937962267".parse().unwrap(),
            network_config_version: "V1-T1666644937952267".parse().unwrap(),
            sitename: Some("testsite".to_string()),
            extension_service_version: "V1-T1666644937952267".parse().unwrap(),
        };

        let (server, server_port) = setup_server(
            Some(metadata.clone()),
            Some(ManagedHostNetworkConfigResponse::default()),
        )
        .await;
        send_request_and_check_response(
            server_port,
            "meta-data/hostname",
            &metadata.hostname,
            StatusCode::OK,
        )
        .await;
        server.abort();

        for (instance_name, expected_body, expected_status) in [
            (Some("test-instance"), "test-instance", StatusCode::OK),
            (None, "instance name not available", StatusCode::NOT_FOUND),
            (
                Some(""),
                "instance name not available",
                StatusCode::NOT_FOUND,
            ),
        ] {
            metadata.instance_name = instance_name.map(str::to_owned);
            let (server, server_port) = setup_server(
                Some(metadata.clone()),
                Some(ManagedHostNetworkConfigResponse::default()),
            )
            .await;
            send_request_and_check_response(
                server_port,
                "meta-data/instance-name",
                expected_body,
                expected_status,
            )
            .await;
            server.abort();
        }
    }

    #[tokio::test]
    async fn test_get_metadata_listing() {
        let metadata = InstanceMetadata {
            instance_id: None,
            machine_id: Some(
                "fm100ht6n80e7do39u8gmt7cvhm89pb32st9ngevgdolu542l1nfa4an0rg"
                    .parse()
                    .unwrap(),
            ),
            public_addresses: PublicAddresses {
                ipv4: Some(Ipv4Addr::LOCALHOST),
                ipv6: None,
            },
            hostname: "localhost".to_string(),
            instance_name: Some("test-instance".to_string()),
            user_data: "\"userData\": {\"data\": 0}".to_string(),
            ib_devices: None,
            config_version: "V2-T1666644937962267".parse().unwrap(),
            network_config_version: "V1-T1666644937952267".parse().unwrap(),
            sitename: Some("testsite".to_string()),
            extension_service_version: "V1-T1666644937952267".parse().unwrap(),
        };

        let expected_output = [
            HOSTNAME_CATEGORY,
            INSTANCE_NAME_CATEGORY,
            SITENAME_CATEGORY,
            MACHINE_ID_CATEGORY,
            INSTANCE_ID_CATEGORY,
            ASN_CATEGORY,
            PUBLIC_IPV4_CATEGORY,
            PUBLIC_IPV6_CATEGORY,
        ]
        .join("\n");

        let (server, server_port) = setup_server(
            Some(metadata.clone()),
            Some(ManagedHostNetworkConfigResponse::default()),
        )
        .await;
        send_request_and_check_response(server_port, "meta-data", &expected_output, StatusCode::OK)
            .await;
        // Also check the metadata url with the end slash is valid
        send_request_and_check_response(
            server_port,
            "meta-data/",
            &expected_output,
            StatusCode::OK,
        )
        .await;
        server.abort();
    }

    #[tokio::test]
    async fn test_get_metadata_parameter_user_data_category() {
        let metadata = InstanceMetadata {
            instance_id: Some(uuid!("67e55044-10b1-426f-9247-bb680e5fe0c8").into()),
            machine_id: Some(
                "fm100ht6n80e7do39u8gmt7cvhm89pb32st9ngevgdolu542l1nfa4an0rg"
                    .parse()
                    .unwrap(),
            ),
            public_addresses: PublicAddresses {
                ipv4: Some(Ipv4Addr::LOCALHOST),
                ipv6: None,
            },
            hostname: "localhost".to_string(),
            instance_name: Some("test-instance".to_string()),
            user_data: "\"userData\": {\"data\": 0}".to_string(),
            ib_devices: None,
            config_version: "V2-T1666644937962267".parse().unwrap(),
            network_config_version: "V1-T1666644937952267".parse().unwrap(),
            sitename: Some("testsite".to_string()),
            extension_service_version: "V1-T1666644937952267".parse().unwrap(),
        };

        let (server, server_port) = setup_server(
            Some(metadata.clone()),
            Some(ManagedHostNetworkConfigResponse::default()),
        )
        .await;
        send_request_and_check_response(
            server_port,
            "user-data",
            &metadata.user_data,
            StatusCode::OK,
        )
        .await;
        server.abort();
    }

    #[tokio::test]
    async fn test_get_server_error_on_empty_metadata() {
        let (server, server_port) = setup_server(None, None).await;
        send_request_and_check_response(
            server_port,
            "meta-data/hostname",
            "metadata currently unavailable",
            StatusCode::INTERNAL_SERVER_ERROR,
        )
        .await;
        server.abort();
    }

    #[tokio::test]
    async fn test_get_ib_devices() {
        let metadata = InstanceMetadata {
            instance_id: Some(uuid!("67e55044-10b1-426f-9247-bb680e5fe0c8").into()),
            machine_id: Some(
                "fm100ht6n80e7do39u8gmt7cvhm89pb32st9ngevgdolu542l1nfa4an0rg"
                    .parse()
                    .unwrap(),
            ),
            public_addresses: PublicAddresses {
                ipv4: Some(Ipv4Addr::LOCALHOST),
                ipv6: None,
            },
            hostname: "localhost".to_string(),
            instance_name: Some("test-instance".to_string()),
            user_data: "\"userData\": {\"data\": 0}".to_string(),
            ib_devices: Some(vec![
                IBDeviceConfig {
                    pf_guid: "pfguid1".to_string(),
                    instances: vec![IBInstanceConfig {
                        ib_partition_id: None,
                        ib_guid: Some("test-guid1".to_string()),
                        lid: 0,
                    }],
                },
                IBDeviceConfig {
                    pf_guid: "pfguid2".to_string(),
                    instances: vec![IBInstanceConfig {
                        ib_partition_id: None,
                        ib_guid: Some("test-guid2".to_string()),
                        lid: 1,
                    }],
                },
            ]),
            config_version: "V2-T1666644937962267".parse().unwrap(),
            network_config_version: "V1-T1666644937952267".parse().unwrap(),
            sitename: Some("testsite".to_string()),
            extension_service_version: "V1-T1666644937952267".parse().unwrap(),
        };

        let (server, server_port) = setup_server(
            Some(metadata.clone()),
            Some(ManagedHostNetworkConfigResponse::default()),
        )
        .await;
        send_request_and_check_response(
            server_port,
            "meta-data/infiniband/devices",
            "0=pfguid1\n1=pfguid2\n",
            StatusCode::OK,
        )
        .await;
        server.abort();
    }

    #[tokio::test]
    async fn test_get_incorrect_ib_device() {
        let metadata = InstanceMetadata {
            instance_id: Some(uuid!("67e55044-10b1-426f-9247-bb680e5fe0c8").into()),
            machine_id: Some(
                "fm100ht6n80e7do39u8gmt7cvhm89pb32st9ngevgdolu542l1nfa4an0rg"
                    .parse()
                    .unwrap(),
            ),
            public_addresses: PublicAddresses {
                ipv4: Some(Ipv4Addr::LOCALHOST),
                ipv6: None,
            },
            hostname: "localhost".to_string(),
            instance_name: Some("test-instance".to_string()),
            user_data: "\"userData\": {\"data\": 0}".to_string(),
            ib_devices: Some(vec![IBDeviceConfig {
                pf_guid: "pfguid1".to_string(),
                instances: vec![IBInstanceConfig {
                    ib_partition_id: None,
                    ib_guid: Some("test-guid1".to_string()),
                    lid: 0,
                }],
            }]),
            config_version: "V2-T1666644937962267".parse().unwrap(),
            network_config_version: "V1-T1666644937952267".parse().unwrap(),
            sitename: Some("testsite".to_string()),
            extension_service_version: "V1-T1666644937952267".parse().unwrap(),
        };

        let (server, server_port) = setup_server(
            Some(metadata.clone()),
            Some(ManagedHostNetworkConfigResponse::default()),
        )
        .await;
        send_request_and_check_response(
            server_port,
            "meta-data/infiniband/devices/2",
            "no device at index: 2",
            StatusCode::NOT_FOUND,
        )
        .await;
        server.abort();
    }

    #[tokio::test]
    async fn test_get_ib_instances() {
        let metadata = InstanceMetadata {
            instance_id: Some(uuid!("67e55044-10b1-426f-9247-bb680e5fe0c8").into()),
            machine_id: Some(
                "fm100ht6n80e7do39u8gmt7cvhm89pb32st9ngevgdolu542l1nfa4an0rg"
                    .parse()
                    .unwrap(),
            ),
            public_addresses: PublicAddresses {
                ipv4: Some(Ipv4Addr::LOCALHOST),
                ipv6: None,
            },
            hostname: "localhost".to_string(),
            instance_name: Some("test-instance".to_string()),
            user_data: "\"userData\": {\"data\": 0}".to_string(),
            ib_devices: Some(vec![IBDeviceConfig {
                pf_guid: "guid".to_string(),
                instances: vec![
                    IBInstanceConfig {
                        ib_partition_id: None,
                        ib_guid: Some("test-guid1".to_string()),
                        lid: 0,
                    },
                    IBInstanceConfig {
                        ib_partition_id: None,
                        ib_guid: Some("test-guid2".to_string()),
                        lid: 1,
                    },
                ],
            }]),
            config_version: "V2-T1666644937962267".parse().unwrap(),
            network_config_version: "V1-T1666644937952267".parse().unwrap(),
            sitename: Some("testsite".to_string()),
            extension_service_version: "V1-T1666644937952267".parse().unwrap(),
        };

        let (server, server_port) = setup_server(
            Some(metadata.clone()),
            Some(ManagedHostNetworkConfigResponse::default()),
        )
        .await;
        send_request_and_check_response(
            server_port,
            "meta-data/infiniband/devices/0/instances",
            "0=test-guid1\n1=test-guid2\n",
            StatusCode::OK,
        )
        .await;
        server.abort();
    }

    #[tokio::test]
    async fn test_get_ib_instance() {
        let metadata = InstanceMetadata {
            instance_id: Some(uuid!("67e55044-10b1-426f-9247-bb680e5fe0c8").into()),
            machine_id: Some(
                "fm100ht6n80e7do39u8gmt7cvhm89pb32st9ngevgdolu542l1nfa4an0rg"
                    .parse()
                    .unwrap(),
            ),
            public_addresses: PublicAddresses {
                ipv4: Some(Ipv4Addr::LOCALHOST),
                ipv6: None,
            },
            hostname: "localhost".to_string(),
            instance_name: Some("test-instance".to_string()),
            user_data: "\"userData\": {\"data\": 0}".to_string(),
            ib_devices: Some(vec![IBDeviceConfig {
                pf_guid: "guid".to_string(),
                instances: vec![IBInstanceConfig {
                    ib_partition_id: Some("67e55044-10b1-426f-9247-bb680e5fe0c8".parse().unwrap()),
                    ib_guid: Some("test-guid1".to_string()),
                    lid: 0,
                }],
            }]),
            config_version: "V2-T1666644937962267".parse().unwrap(),
            network_config_version: "V1-T1666644937952267".parse().unwrap(),
            sitename: Some("testsite".to_string()),
            extension_service_version: "V1-T1666644937952267".parse().unwrap(),
        };

        let (server, server_port) = setup_server(
            Some(metadata.clone()),
            Some(ManagedHostNetworkConfigResponse::default()),
        )
        .await;
        send_request_and_check_response(
            server_port,
            "meta-data/infiniband/devices/0/instances/0",
            "guid\npartition\nlid",
            StatusCode::OK,
        )
        .await;
        server.abort();
    }

    #[tokio::test]
    async fn test_get_ib_instance_not_all_attributes() {
        let metadata = InstanceMetadata {
            instance_id: Some(uuid!("67e55044-10b1-426f-9247-bb680e5fe0c8").into()),
            machine_id: Some(
                "fm100ht6n80e7do39u8gmt7cvhm89pb32st9ngevgdolu542l1nfa4an0rg"
                    .parse()
                    .unwrap(),
            ),
            public_addresses: PublicAddresses {
                ipv4: Some(Ipv4Addr::LOCALHOST),
                ipv6: None,
            },
            hostname: "localhost".to_string(),
            instance_name: Some("test-instance".to_string()),
            user_data: "\"userData\": {\"data\": 0}".to_string(),
            ib_devices: Some(vec![IBDeviceConfig {
                pf_guid: "guid".to_string(),
                instances: vec![IBInstanceConfig {
                    ib_partition_id: None,
                    ib_guid: None,
                    lid: 0,
                }],
            }]),
            config_version: "V2-T1666644937962267".parse().unwrap(),
            network_config_version: "V1-T1666644937952267".parse().unwrap(),
            sitename: Some("testsite".to_string()),
            extension_service_version: "V1-T1666644937952267".parse().unwrap(),
        };

        let (server, server_port) = setup_server(
            Some(metadata.clone()),
            Some(ManagedHostNetworkConfigResponse::default()),
        )
        .await;
        send_request_and_check_response(
            server_port,
            "meta-data/infiniband/devices/0/instances/0",
            "lid",
            StatusCode::OK,
        )
        .await;
        server.abort();
    }

    #[tokio::test]
    async fn test_get_incorrect_ib_instance() {
        let metadata = InstanceMetadata {
            instance_id: Some(uuid!("67e55044-10b1-426f-9247-bb680e5fe0c8").into()),
            machine_id: Some(
                "fm100ht6n80e7do39u8gmt7cvhm89pb32st9ngevgdolu542l1nfa4an0rg"
                    .parse()
                    .unwrap(),
            ),
            public_addresses: PublicAddresses {
                ipv4: Some(Ipv4Addr::LOCALHOST),
                ipv6: None,
            },
            hostname: "localhost".to_string(),
            instance_name: Some("test-instance".to_string()),
            user_data: "\"userData\": {\"data\": 0}".to_string(),
            ib_devices: Some(vec![IBDeviceConfig {
                pf_guid: "guid".to_string(),
                instances: vec![IBInstanceConfig {
                    ib_partition_id: None,
                    ib_guid: Some("test-guid1".to_string()),
                    lid: 0,
                }],
            }]),
            config_version: "V2-T1666644937962267".parse().unwrap(),
            network_config_version: "V1-T1666644937952267".parse().unwrap(),
            sitename: Some("testsite".to_string()),
            extension_service_version: "V1-T1666644937952267".parse().unwrap(),
        };

        let (server, server_port) = setup_server(
            Some(metadata.clone()),
            Some(ManagedHostNetworkConfigResponse::default()),
        )
        .await;
        send_request_and_check_response(
            server_port,
            "meta-data/infiniband/devices/0/instances/3",
            "no instance at index: 3",
            StatusCode::NOT_FOUND,
        )
        .await;
        server.abort();
    }

    #[tokio::test]
    async fn test_get_ib_instance_attribute() {
        let metadata = InstanceMetadata {
            instance_id: Some(uuid!("67e55044-10b1-426f-9247-bb680e5fe0c8").into()),
            machine_id: Some(
                "fm100ht6n80e7do39u8gmt7cvhm89pb32st9ngevgdolu542l1nfa4an0rg"
                    .parse()
                    .unwrap(),
            ),
            public_addresses: PublicAddresses {
                ipv4: Some(Ipv4Addr::LOCALHOST),
                ipv6: None,
            },
            hostname: "localhost".to_string(),
            instance_name: Some("test-instance".to_string()),
            user_data: "\"userData\": {\"data\": 0}".to_string(),
            ib_devices: Some(vec![IBDeviceConfig {
                pf_guid: "guid".to_string(),
                instances: vec![IBInstanceConfig {
                    ib_partition_id: None,
                    ib_guid: Some("test-guid".to_string()),
                    lid: 0,
                }],
            }]),
            config_version: "V2-T1666644937962267".parse().unwrap(),
            network_config_version: "V1-T1666644937952267".parse().unwrap(),
            sitename: Some("testsite".to_string()),
            extension_service_version: "V1-T1666644937952267".parse().unwrap(),
        };

        let (server, server_port) = setup_server(
            Some(metadata.clone()),
            Some(ManagedHostNetworkConfigResponse::default()),
        )
        .await;
        send_request_and_check_response(
            server_port,
            "meta-data/infiniband/devices/0/instances/0/guid",
            "test-guid",
            StatusCode::OK,
        )
        .await;
        server.abort();
    }

    #[tokio::test]
    async fn test_get_ib_instance_nonexistent_attribute() {
        let metadata = InstanceMetadata {
            instance_id: Some(uuid!("67e55044-10b1-426f-9247-bb680e5fe0c8").into()),
            machine_id: Some(
                "fm100ht6n80e7do39u8gmt7cvhm89pb32st9ngevgdolu542l1nfa4an0rg"
                    .parse()
                    .unwrap(),
            ),
            public_addresses: PublicAddresses {
                ipv4: Some(Ipv4Addr::LOCALHOST),
                ipv6: None,
            },
            hostname: "localhost".to_string(),
            instance_name: Some("test-instance".to_string()),
            user_data: "\"userData\": {\"data\": 0}".to_string(),
            ib_devices: Some(vec![IBDeviceConfig {
                pf_guid: "guid".to_string(),
                instances: vec![IBInstanceConfig {
                    ib_partition_id: None,
                    ib_guid: None,
                    lid: 0,
                }],
            }]),
            config_version: "V2-T1666644937962267".parse().unwrap(),
            network_config_version: "V1-T1666644937952267".parse().unwrap(),
            sitename: Some("testsite".to_string()),
            extension_service_version: "V1-T1666644937952267".parse().unwrap(),
        };

        let (server, server_port) = setup_server(
            Some(metadata.clone()),
            Some(ManagedHostNetworkConfigResponse::default()),
        )
        .await;
        send_request_and_check_response(
            server_port,
            "meta-data/infiniband/devices/0/instances/0/partition",
            "ib partition not found at index: 0",
            StatusCode::NOT_FOUND,
        )
        .await;
        server.abort();
    }

    #[tokio::test]
    async fn test_get_instance_id() {
        let metadata = InstanceMetadata {
            instance_id: Some(uuid!("67e55044-10b1-426f-9247-bb680e5fe0c8").into()),
            machine_id: Some(
                "fm100ht6n80e7do39u8gmt7cvhm89pb32st9ngevgdolu542l1nfa4an0rg"
                    .parse()
                    .unwrap(),
            ),
            public_addresses: PublicAddresses {
                ipv4: Some(Ipv4Addr::LOCALHOST),
                ipv6: None,
            },
            hostname: "localhost".to_string(),
            instance_name: Some("test-instance".to_string()),
            user_data: "\"userData\": {\"data\": 0}".to_string(),
            ib_devices: None,
            config_version: "V2-T1666644937962267".parse().unwrap(),
            network_config_version: "V1-T1666644937952267".parse().unwrap(),
            sitename: Some("testsite".to_string()),
            extension_service_version: "V1-T1666644937952267".parse().unwrap(),
        };

        let (server, server_port) = setup_server(
            Some(metadata.clone()),
            Some(ManagedHostNetworkConfigResponse::default()),
        )
        .await;
        send_request_and_check_response(
            server_port,
            "meta-data/instance-id",
            "67e55044-10b1-426f-9247-bb680e5fe0c8",
            StatusCode::OK,
        )
        .await;
        server.abort();
    }

    #[tokio::test]
    async fn test_get_machine_id() {
        let metadata = InstanceMetadata {
            instance_id: None,
            machine_id: Some(
                "fm100ht6n80e7do39u8gmt7cvhm89pb32st9ngevgdolu542l1nfa4an0rg"
                    .parse()
                    .unwrap(),
            ),
            public_addresses: PublicAddresses {
                ipv4: Some(Ipv4Addr::LOCALHOST),
                ipv6: None,
            },
            hostname: "localhost".to_string(),
            instance_name: Some("test-instance".to_string()),
            user_data: "\"userData\": {\"data\": 0}".to_string(),
            ib_devices: None,
            config_version: "V2-T1666644937962267".parse().unwrap(),
            network_config_version: "V1-T1666644937952267".parse().unwrap(),
            sitename: Some("testsite".to_string()),
            extension_service_version: "V1-T1666644937952267".parse().unwrap(),
        };

        let (server, server_port) = setup_server(
            Some(metadata.clone()),
            Some(ManagedHostNetworkConfigResponse::default()),
        )
        .await;
        send_request_and_check_response(
            server_port,
            "meta-data/machine-id",
            "fm100ht6n80e7do39u8gmt7cvhm89pb32st9ngevgdolu542l1nfa4an0rg",
            StatusCode::OK,
        )
        .await;
        server.abort();
    }

    #[tokio::test]
    async fn test_get_asn() {
        let metadata = InstanceMetadata {
            instance_id: None,
            machine_id: Some(
                "fm100ht6n80e7do39u8gmt7cvhm89pb32st9ngevgdolu542l1nfa4an0rg"
                    .parse()
                    .unwrap(),
            ),
            public_addresses: PublicAddresses {
                ipv4: Some(Ipv4Addr::LOCALHOST),
                ipv6: None,
            },
            hostname: "localhost".to_string(),
            instance_name: Some("test-instance".to_string()),
            user_data: "\"userData\": {\"data\": 0}".to_string(),
            ib_devices: None,
            config_version: "V2-T1666644937962267".parse().unwrap(),
            network_config_version: "V1-T1666644937952267".parse().unwrap(),
            sitename: Some("testsite".to_string()),
            extension_service_version: "V1-T1666644937952267".parse().unwrap(),
        };

        let network_config = ManagedHostNetworkConfigResponse {
            asn: 123,
            ..Default::default()
        };

        let (server, server_port) =
            setup_server(Some(metadata.clone()), Some(network_config)).await;
        send_request_and_check_response(server_port, "meta-data/asn", "123", StatusCode::OK).await;
        server.abort();
    }

    #[tokio::test]
    async fn test_get_sitename() {
        let metadata = InstanceMetadata {
            instance_id: None,
            machine_id: Some(
                "fm100ht6n80e7do39u8gmt7cvhm89pb32st9ngevgdolu542l1nfa4an0rg"
                    .parse()
                    .unwrap(),
            ),
            public_addresses: PublicAddresses {
                ipv4: Some(Ipv4Addr::LOCALHOST),
                ipv6: None,
            },
            hostname: "localhost".to_string(),
            instance_name: Some("test-instance".to_string()),
            user_data: "\"userData\": {\"data\": 0}".to_string(),
            ib_devices: None,
            config_version: "V2-T1666644937962267".parse().unwrap(),
            network_config_version: "V1-T1666644937952267".parse().unwrap(),
            sitename: Some("testsite".to_string()),
            extension_service_version: "V1-T1666644937952267".parse().unwrap(),
        };

        let (server, server_port) = setup_server(
            Some(metadata.clone()),
            Some(ManagedHostNetworkConfigResponse::default()),
        )
        .await;

        send_request_and_check_response(
            server_port,
            "meta-data/sitename",
            "testsite",
            StatusCode::OK,
        )
        .await;
        server.abort();
    }
}
