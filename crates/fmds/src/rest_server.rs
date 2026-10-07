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

use axum::Router;
use axum::extract::{Path, State};
use axum::http::header::HeaderMap;
use axum::http::{StatusCode, Uri};
use axum::response::Response;
use axum::routing::{get, post};
use forge_dpu_fmds_shared::machine_identity;

use crate::state::FmdsState;

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
const DEVICES_CATEGORY: &str = "devices";
const INFINIBAND_CATEGORY: &str = "infiniband";
const MACHINE_ID_CATEGORY: &str = "machine-id";
const INSTANCE_ID_CATEGORY: &str = "instance-id";
const PHONE_HOME_CATEGORY: &str = "phone_home";
const ASN_CATEGORY: &str = "asn";

pub fn get_fmds_router(state: Arc<FmdsState>) -> Router {
    let user_data_router =
        Router::new().route(&format!("/{USER_DATA_CATEGORY}"), get(get_userdata));

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
        // The additional ending slash is a cloud init issue as
        // found when looking at the cloud init src.
        // https://bugs.launchpad.net/cloud-init/+bug/1356855
        .route(&format!("/{META_DATA_CATEGORY}/"), get(get_metadata_params))
        .route(&format!("/{META_DATA_CATEGORY}"), get(get_metadata_params))
        .nest(&format!("/{META_DATA_CATEGORY}"), service_router);

    Router::new()
        .merge(metadata_router)
        .merge(user_data_router)
        .with_state(state)
}

async fn get_metadata_parameter(
    State(state): State<Arc<FmdsState>>,
    Path(category): Path<String>,
) -> (StatusCode, String) {
    extract_metadata(category, &state)
}

async fn get_userdata(State(state): State<Arc<FmdsState>>) -> (StatusCode, String) {
    extract_metadata(USER_DATA_CATEGORY.to_string(), &state)
}

fn extract_metadata(category: String, state: &FmdsState) -> (StatusCode, String) {
    let config = match state.config.load_full() {
        Some(config) => config,
        None => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "metadata currently unavailable".to_string(),
            );
        }
    };

    match category.as_str() {
        PUBLIC_IPV4_CATEGORY => (
            StatusCode::OK,
            config
                .public_ipv4
                .map(|address| address.to_string())
                .unwrap_or_default(),
        ),
        PUBLIC_IPV6_CATEGORY => (
            StatusCode::OK,
            config
                .public_ipv6
                .map(|address| address.to_string())
                .unwrap_or_default(),
        ),
        HOSTNAME_CATEGORY => (StatusCode::OK, config.hostname.clone()),
        INSTANCE_NAME_CATEGORY => match &config.instance_name {
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
            config.sitename.clone().unwrap_or(String::new()),
        ),
        USER_DATA_CATEGORY => (StatusCode::OK, config.user_data.clone()),
        ASN_CATEGORY => (StatusCode::OK, config.asn.to_string()),
        _ => (
            StatusCode::NOT_FOUND,
            format!("metadata category not found: {category}"),
        ),
    }
}

async fn get_metadata_identity(
    State(state): State<Arc<FmdsState>>,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    machine_identity::serve_meta_data_identity(state.as_ref(), uri, headers).await
}

async fn get_machine_id(State(state): State<Arc<FmdsState>>) -> (StatusCode, String) {
    let config = match state.config.load_full() {
        Some(config) => config,
        None => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "metadata currently unavailable".to_string(),
            );
        }
    };

    if let Some(machine_id) = &config.machine_id {
        (StatusCode::OK, machine_id.to_string())
    } else {
        (
            StatusCode::NOT_FOUND,
            "machine id not available".to_string(),
        )
    }
}

async fn get_instance_id(State(state): State<Arc<FmdsState>>) -> (StatusCode, String) {
    let config = match state.config.load_full() {
        Some(config) => config,
        None => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "metadata currently unavailable".to_string(),
            );
        }
    };

    if let Some(instance_id) = &config.instance_id {
        (StatusCode::OK, instance_id.to_string())
    } else {
        (
            StatusCode::NOT_FOUND,
            "instance id not available".to_string(),
        )
    }
}

async fn get_metadata_params(State(_state): State<Arc<FmdsState>>) -> (StatusCode, String) {
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

async fn get_devices(State(state): State<Arc<FmdsState>>) -> (StatusCode, String) {
    let config = match state.config.load_full() {
        Some(config) => config,
        None => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "metadata currently unavailable".to_string(),
            );
        }
    };

    let mut response = String::new();
    if let Some(devices) = &config.ib_devices {
        for (index, device) in devices.iter().enumerate() {
            response.push_str(&format!("{}={}\n", index, device.pf_guid));
        }
        (StatusCode::OK, response)
    } else {
        (StatusCode::NOT_FOUND, "devices not available".to_string())
    }
}

async fn get_instances(
    State(state): State<Arc<FmdsState>>,
    Path(device_index): Path<usize>,
) -> (StatusCode, String) {
    let config = match state.config.load_full() {
        Some(config) => config,
        None => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "metadata currently unavailable".to_string(),
            );
        }
    };

    if let Some(devices) = &config.ib_devices {
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
    State(state): State<Arc<FmdsState>>,
    Path((device_index, instance_index)): Path<(usize, usize)>,
) -> (StatusCode, String) {
    let config = match state.config.load_full() {
        Some(config) => config,
        None => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "metadata currently unavailable".to_string(),
            );
        }
    };

    if let Some(devices) = &config.ib_devices {
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
        if inst.ib_guid.is_some() {
            response += &(GUID.to_owned() + "\n");
        }
        if inst.ib_partition_id.is_some() {
            response += &(IB_PARTITION.to_owned() + "\n");
        }
        response.push_str(LID);

        (StatusCode::OK, response)
    } else {
        (StatusCode::NOT_FOUND, "devices not available".to_string())
    }
}

async fn get_instance_attribute(
    State(state): State<Arc<FmdsState>>,
    Path((device_index, instance_index, attribute)): Path<(usize, usize, String)>,
) -> (StatusCode, String) {
    let config = match state.config.load_full() {
        Some(config) => config,
        None => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "metadata currently unavailable".to_string(),
            );
        }
    };

    if let Some(devices) = &config.ib_devices {
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

async fn post_phone_home(State(state): State<Arc<FmdsState>>) -> (StatusCode, String) {
    match crate::phone_home::phone_home(&state).await {
        Ok(()) => (StatusCode::OK, "successfully phoned home\n".to_string()),
        Err(err) => (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};

    use axum::http;
    use carbide_instrument::testing::MetricsCapture;
    use forge_dpu_fmds_shared::machine_identity::MachineIdentityParams;
    use http_body_util::{BodyExt, Full};
    use hyper::body::Bytes;
    use hyper_util::rt::TokioExecutor;

    use super::*;
    use crate::state::{FmdsConfig, IBDeviceConfig, IBInstanceConfig};

    fn make_test_state() -> Arc<FmdsState> {
        Arc::new(FmdsState::try_new("https://api.test".to_string(), None).unwrap())
    }

    fn make_test_config() -> FmdsConfig {
        FmdsConfig {
            public_ipv4: Some("10.0.0.1".parse().unwrap()),
            public_ipv6: Some("2001:db8::1".parse().unwrap()),
            hostname: "test-host".to_string(),
            instance_name: Some("test-instance".to_string()),
            sitename: Some("test-site".to_string()),
            instance_id: Some(uuid::uuid!("67e55044-10b1-426f-9247-bb680e5fe0c8").into()),
            machine_id: Some(
                "fm100ht6n80e7do39u8gmt7cvhm89pb32st9ngevgdolu542l1nfa4an0rg"
                    .parse()
                    .unwrap(),
            ),
            user_data: "cloud-init-data".to_string(),
            ib_devices: None,
            asn: 65000,
        }
    }

    async fn setup_server(state: Arc<FmdsState>) -> (tokio::task::JoinHandle<()>, u16) {
        let router = get_fmds_router(state);

        let addr = std::net::SocketAddr::from(([127, 0, 0, 1], 0));
        let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
        let server_port = listener.local_addr().unwrap().port();

        let server = tokio::spawn(async move {
            axum_server::Server::<SocketAddr>::from_listener(listener)
                .serve(router.into_make_service())
                .await
                .unwrap();
        });

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        (server, server_port)
    }

    async fn get_request(port: u16, path: &str) -> (http::StatusCode, String) {
        let client = hyper_util::client::legacy::Client::builder(TokioExecutor::new()).build_http();
        let request: hyper::Request<Full<Bytes>> = hyper::Request::builder()
            .method(hyper::Method::GET)
            .uri(format!("http://127.0.0.1:{port}/{path}"))
            .body("".into())
            .unwrap();

        let response = client.request(request).await.unwrap();
        let status = response.status();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let body_str = std::str::from_utf8(&body).unwrap().to_string();

        (status, body_str)
    }

    // Metadata unavailable (empty state) test.
    #[tokio::test]
    async fn test_returns_error_when_no_config() {
        let state = make_test_state();
        let (server, port) = setup_server(state).await;

        let (status, body) = get_request(port, "meta-data/hostname").await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body, "metadata currently unavailable");

        server.abort();
    }

    // Test basic metadata fields.
    #[tokio::test]
    async fn test_get_hostname_and_instance_name() {
        let state = make_test_state();
        state.update_config(make_test_config());
        let (server, port) = setup_server(state.clone()).await;

        let (status, body) = get_request(port, "meta-data/hostname").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, "test-host");

        for (instance_name, expected_status, expected_body) in [
            (Some("test-instance"), StatusCode::OK, "test-instance"),
            (None, StatusCode::NOT_FOUND, "instance name not available"),
            (
                Some(""),
                StatusCode::NOT_FOUND,
                "instance name not available",
            ),
        ] {
            state.update_config(FmdsConfig {
                instance_name: instance_name.map(str::to_owned),
                ..make_test_config()
            });
            let (status, body) = get_request(port, "meta-data/instance-name").await;
            assert_eq!(status, expected_status);
            assert_eq!(body, expected_body);
        }

        server.abort();
    }

    #[tokio::test]
    async fn test_get_public_ip_addresses() {
        struct PublicAddressCase {
            public_ipv4: Option<Ipv4Addr>,
            public_ipv6: Option<Ipv6Addr>,
            expected_ipv4: &'static str,
            expected_ipv6: &'static str,
        }

        let state = make_test_state();
        state.update_config(make_test_config());
        let (server, port) = setup_server(state.clone()).await;

        let (status, body) = get_request(port, "meta-data/public-ipv4").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, "10.0.0.1");

        let (status, body) = get_request(port, "meta-data/public-ipv6").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, "2001:db8::1");

        for case in [
            PublicAddressCase {
                public_ipv4: None,
                public_ipv6: Some("2001:db8::1".parse().unwrap()),
                expected_ipv4: "",
                expected_ipv6: "2001:db8::1",
            },
            PublicAddressCase {
                public_ipv4: Some("10.0.0.1".parse().unwrap()),
                public_ipv6: None,
                expected_ipv4: "10.0.0.1",
                expected_ipv6: "",
            },
            PublicAddressCase {
                public_ipv4: None,
                public_ipv6: None,
                expected_ipv4: "",
                expected_ipv6: "",
            },
        ] {
            state.update_config(FmdsConfig {
                public_ipv4: case.public_ipv4,
                public_ipv6: case.public_ipv6,
                ..make_test_config()
            });

            let (status, body) = get_request(port, "meta-data/public-ipv4").await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(body, case.expected_ipv4);

            let (status, body) = get_request(port, "meta-data/public-ipv6").await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(body, case.expected_ipv6);
        }

        server.abort();
    }

    #[tokio::test]
    async fn test_get_sitename() {
        let state = make_test_state();
        state.update_config(make_test_config());
        let (server, port) = setup_server(state).await;

        let (status, body) = get_request(port, "meta-data/sitename").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, "test-site");

        server.abort();
    }

    #[tokio::test]
    async fn test_get_user_data() {
        let state = make_test_state();
        state.update_config(make_test_config());
        let (server, port) = setup_server(state).await;

        let (status, body) = get_request(port, "user-data").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, "cloud-init-data");

        server.abort();
    }

    #[tokio::test]
    async fn test_get_asn() {
        let state = make_test_state();
        state.update_config(make_test_config());
        let (server, port) = setup_server(state).await;

        let (status, body) = get_request(port, "meta-data/asn").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, "65000");

        server.abort();
    }

    #[tokio::test]
    async fn test_get_instance_id() {
        let state = make_test_state();
        state.update_config(make_test_config());
        let (server, port) = setup_server(state).await;

        let (status, body) = get_request(port, "meta-data/instance-id").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, "67e55044-10b1-426f-9247-bb680e5fe0c8");

        server.abort();
    }

    #[tokio::test]
    async fn test_get_machine_id() {
        let state = make_test_state();
        state.update_config(make_test_config());
        let (server, port) = setup_server(state).await;

        let (status, body) = get_request(port, "meta-data/machine-id").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body,
            "fm100ht6n80e7do39u8gmt7cvhm89pb32st9ngevgdolu542l1nfa4an0rg"
        );

        server.abort();
    }

    // Test metadata listing.
    //
    // `identity` must not appear in the plain /meta-data/ index: cloud-init's EC2
    // IMDS crawler GETs every advertised key without a Metadata header, and the
    // identity handler returns 400 without that header — which aborts the crawl.
    // The /meta-data/identity route remains for clients that send Metadata: true.
    #[tokio::test]
    async fn test_get_metadata_listing() {
        let state = make_test_state();
        state.update_config(make_test_config());
        let (server, port) = setup_server(state).await;

        let expected = [
            "hostname",
            "instance-name",
            "sitename",
            "machine-id",
            "instance-id",
            "asn",
            "public-ipv4",
            "public-ipv6",
        ]
        .join("\n");

        let (status, body) = get_request(port, "meta-data").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, expected);
        assert!(
            !body
                .lines()
                .any(|line| line == machine_identity::META_DATA_IDENTITY_CATEGORY),
            "identity must not be listed in /meta-data/ (cloud-init EC2 crawl)"
        );

        // Also check with trailing slash (cloud-init compat).
        let (status, body) = get_request(port, "meta-data/").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, expected);
        assert!(
            !body
                .lines()
                .any(|line| line == machine_identity::META_DATA_IDENTITY_CATEGORY),
            "identity must not be listed in /meta-data/ (cloud-init EC2 crawl)"
        );

        server.abort();
    }

    // Unknown data category checking.
    #[tokio::test]
    async fn test_get_unknown_category() {
        let state = make_test_state();
        state.update_config(make_test_config());
        let (server, port) = setup_server(state).await;

        let (status, _body) = get_request(port, "meta-data/nope").await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        server.abort();
    }

    // IB device tests.
    fn config_with_ib_devices() -> FmdsConfig {
        FmdsConfig {
            ib_devices: Some(vec![
                IBDeviceConfig {
                    pf_guid: "pfguid1".to_string(),
                    instances: vec![IBInstanceConfig {
                        ib_partition_id: Some(
                            "67e55044-10b1-426f-9247-bb680e5fe0c8".parse().unwrap(),
                        ),
                        ib_guid: Some("guid1".to_string()),
                        lid: 42,
                    }],
                },
                IBDeviceConfig {
                    pf_guid: "pfguid2".to_string(),
                    instances: vec![IBInstanceConfig {
                        ib_partition_id: None,
                        ib_guid: Some("guid2".to_string()),
                        lid: 43,
                    }],
                },
            ]),
            ..make_test_config()
        }
    }

    #[tokio::test]
    async fn test_get_ib_devices() {
        let state = make_test_state();
        state.update_config(config_with_ib_devices());
        let (server, port) = setup_server(state).await;

        let (status, body) = get_request(port, "meta-data/infiniband/devices").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, "0=pfguid1\n1=pfguid2\n");

        server.abort();
    }

    #[tokio::test]
    async fn test_get_ib_instances() {
        let state = make_test_state();
        state.update_config(config_with_ib_devices());
        let (server, port) = setup_server(state).await;

        let (status, body) = get_request(port, "meta-data/infiniband/devices/0/instances").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, "0=guid1\n");

        server.abort();
    }

    #[tokio::test]
    async fn test_get_ib_instance_attributes() {
        let state = make_test_state();
        state.update_config(config_with_ib_devices());
        let (server, port) = setup_server(state).await;

        let (status, body) = get_request(port, "meta-data/infiniband/devices/0/instances/0").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, "guid\npartition\nlid");

        server.abort();
    }

    #[tokio::test]
    async fn test_get_ib_instance_guid() {
        let state = make_test_state();
        state.update_config(config_with_ib_devices());
        let (server, port) = setup_server(state).await;

        let (status, body) =
            get_request(port, "meta-data/infiniband/devices/0/instances/0/guid").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, "guid1");

        server.abort();
    }

    #[tokio::test]
    async fn test_get_ib_instance_lid() {
        let state = make_test_state();
        state.update_config(config_with_ib_devices());
        let (server, port) = setup_server(state).await;

        let (status, body) =
            get_request(port, "meta-data/infiniband/devices/0/instances/0/lid").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, "42");

        server.abort();
    }

    #[tokio::test]
    async fn test_get_ib_device_out_of_range() {
        let state = make_test_state();
        state.update_config(config_with_ib_devices());
        let (server, port) = setup_server(state).await;

        let (status, body) = get_request(port, "meta-data/infiniband/devices/99").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body, "no device at index: 99");

        server.abort();
    }

    #[tokio::test]
    async fn test_get_ib_instance_out_of_range() {
        let state = make_test_state();
        state.update_config(config_with_ib_devices());
        let (server, port) = setup_server(state).await;

        let (status, body) = get_request(port, "meta-data/infiniband/devices/0/instances/99").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body, "no instance at index: 99");

        server.abort();
    }

    // Test integration from gRPC push -> REST read.
    #[tokio::test]
    async fn test_grpc_push_then_rest_read() {
        use rpc::fmds::fmds_config_service_server::FmdsConfigService;
        use rpc::fmds::{FmdsConfigUpdate, UpdateConfigRequest};
        use tonic::Request;

        use crate::grpc_server::FmdsGrpcServer;

        let state = make_test_state();

        // Push config via gRPC server (calling the trait method directly).
        let grpc_server = FmdsGrpcServer::new(state.clone());
        let update = FmdsConfigUpdate {
            address: "192.168.1.1".to_string(),
            address_ipv6: "2001:db8::1".to_string(),
            hostname: "grpc-pushed-host".to_string(),
            instance_name: Some("grpc-pushed-instance".to_string()),
            sitename: Some("grpc-site".to_string()),
            instance_id: Some(uuid::uuid!("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee").into()),
            machine_id: None,
            user_data: "grpc-user-data".to_string(),
            ib_devices: vec![],
            asn: 12345,
            machine_identity: Some(MachineIdentityParams::default().into()),
        };
        {
            // The gRPC handler emits into the process-global test registry, so
            // participate in the metric-capture lock while driving it.
            let _metrics = MetricsCapture::start();
            grpc_server
                .update_config(Request::new(UpdateConfigRequest {
                    config_update: Some(update),
                }))
                .await
                .unwrap();
        }

        // Read via REST
        let (server, port) = setup_server(state).await;

        let (status, body) = get_request(port, "meta-data/hostname").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, "grpc-pushed-host");

        let (status, body) = get_request(port, "meta-data/instance-name").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, "grpc-pushed-instance");

        let (status, body) = get_request(port, "meta-data/public-ipv4").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, "192.168.1.1");

        let (status, body) = get_request(port, "meta-data/public-ipv6").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, "2001:db8::1");

        let (status, body) = get_request(port, "meta-data/asn").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, "12345");

        let (status, body) = get_request(port, "user-data").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, "grpc-user-data");

        let (status, body) = get_request(port, "meta-data/instance-id").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee");

        server.abort();
    }
}
