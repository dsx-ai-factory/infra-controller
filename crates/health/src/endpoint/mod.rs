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

mod cluster;
mod model;
mod sources;

pub use cluster::ClusterEndpointSource;
pub use model::{
    BmcAddr, BmcCredentials, BmcEndpoint, ComponentInventory, EndpointMetadata, EndpointSnapshot,
    EndpointSource, InventorySnapshot, MachineData, PowerShelfData, RackInventory,
    SharedSystemUuid, SwitchData, SwitchEndpointRole,
};
pub use sources::{CompositeEndpointSource, StaticEndpointSource};

pub use crate::bmc::{BoxFuture, CredentialProvider};

#[cfg(test)]
pub(crate) mod test_support {
    use std::str::FromStr;
    use std::sync::Arc;

    use mac_address::MacAddress;
    use nv_redfish::bmc_http::reqwest::{
        Client as ReqwestClient, ClientParams as ReqwestClientParams,
    };

    use super::*;
    use crate::bmc::{BmcClient, FixedCredentialProvider};

    pub(crate) fn reqwest() -> ReqwestClient {
        ReqwestClient::with_params(ReqwestClientParams::new().accept_invalid_certs(true))
            .expect("reqwest client builds")
    }

    pub(crate) fn endpoint_with_creds(
        addr: BmcAddr,
        creds: BmcCredentials,
        metadata: Option<EndpointMetadata>,
        rack_id: Option<carbide_uuid::rack::RackId>,
    ) -> BmcEndpoint {
        let provider = Arc::new(FixedCredentialProvider::new(creds));
        let bmc = Arc::new(
            BmcClient::new(
                reqwest(),
                addr.clone(),
                provider,
                None,
                10,
                std::num::NonZeroUsize::MIN,
                None,
            )
            .expect("fixed-credential BmcClient construction is infallible"),
        );
        BmcEndpoint {
            addr,
            metadata,
            rack_id,
            labels: Default::default(),
            bmc,
        }
    }

    pub(crate) fn test_endpoint(mac: MacAddress) -> BmcEndpoint {
        endpoint_with_creds(
            BmcAddr {
                ip: "10.0.0.1".parse().unwrap(),
                port: Some(443),
                mac,
            },
            BmcCredentials::UsernamePassword {
                username: "admin".to_string(),
                password: Some("password".to_string()),
            },
            None,
            None,
        )
    }

    pub(crate) fn mac(s: &str) -> MacAddress {
        MacAddress::from_str(s).unwrap()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::test_support::{mac, test_endpoint};
    use super::*;

    #[tokio::test]
    async fn test_static_endpoint_source_shares_arc_data() {
        let endpoints = vec![
            test_endpoint(mac("00:11:22:33:44:55")),
            test_endpoint(mac("aa:bb:cc:dd:ee:ff")),
        ];
        let source = StaticEndpointSource::new(endpoints);

        let first = source.fetch_bmc_hosts().await.unwrap();
        let second = source.fetch_bmc_hosts().await.unwrap();

        assert_eq!(first.len(), 2);
        assert_eq!(second.len(), 2);
        assert!(Arc::ptr_eq(&first[0], &second[0]));
        assert!(Arc::ptr_eq(&first[1], &second[1]));
    }

    #[tokio::test]
    async fn test_composite_endpoint_source_preserves_arc_sharing() {
        let endpoints1 = vec![test_endpoint(mac("00:11:22:33:44:55"))];
        let endpoints2 = vec![test_endpoint(mac("aa:bb:cc:dd:ee:ff"))];

        let source1 = Arc::new(StaticEndpointSource::new(endpoints1));
        let source2 = Arc::new(StaticEndpointSource::new(endpoints2));

        let composite = CompositeEndpointSource::new(vec![source1.clone(), source2.clone()]);

        let composite_result = composite.fetch_bmc_hosts().await.unwrap();
        let source1_result = source1.fetch_bmc_hosts().await.unwrap();
        let source2_result = source2.fetch_bmc_hosts().await.unwrap();

        assert_eq!(composite_result.len(), 2);
        assert!(Arc::ptr_eq(&composite_result[0], &source1_result[0]));
        assert!(Arc::ptr_eq(&composite_result[1], &source2_result[0]));
    }

    #[test]
    fn test_to_url_uses_http_for_port_80_and_https_otherwise() {
        let addr_http = BmcAddr {
            ip: "10.0.0.1".parse().expect("valid ip"),
            port: Some(80),
            mac: mac("00:11:22:33:44:55"),
        };
        let addr_https = BmcAddr {
            ip: "10.0.0.2".parse().expect("valid ip"),
            port: Some(443),
            mac: mac("aa:bb:cc:dd:ee:ff"),
        };

        let url_http = addr_http.to_url().expect("url should build");
        let url_https = addr_https.to_url().expect("url should build");

        assert_eq!(url_http.scheme(), "http");
        assert_eq!(url_https.scheme(), "https");
    }
}
