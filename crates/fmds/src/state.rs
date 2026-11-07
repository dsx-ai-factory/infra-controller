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

use std::net::{Ipv4Addr, Ipv6Addr};
use std::sync::Arc;

use arc_swap::{ArcSwap, ArcSwapOption};
use carbide_uuid::infiniband::IBPartitionId;
use carbide_uuid::instance::InstanceId;
use carbide_uuid::machine::MachineId;
use forge_dpu_fmds_shared::machine_identity::{MachineIdentityParams, MachineIdentityServing};
use governor::middleware::NoOpMiddleware;
use governor::state::{InMemoryState, NotKeyed};
use governor::{Quota, RateLimiter, clock};
use nonzero_ext::nonzero;
use rpc::forge_tls_client::ForgeClientConfig;

const PHONE_HOME_RATE_LIMIT: Quota = Quota::per_minute(nonzero!(10u32));

/// Shared state between the gRPC server (writer) and REST server (reader).
pub struct FmdsState {
    pub config: ArcSwapOption<FmdsConfig>,
    pub machine_id: ArcSwapOption<MachineId>,
    pub forge_api: String,
    pub forge_client_config: Option<Arc<ForgeClientConfig>>,
    pub outbound_governor:
        Arc<RateLimiter<NotKeyed, InMemoryState, clock::DefaultClock, NoOpMiddleware>>,
    pub machine_identity: ArcSwap<MachineIdentityServing>,
    last_machine_identity_params: ArcSwapOption<MachineIdentityParams>,
}

impl FmdsState {
    pub fn try_new(
        forge_api: String,
        forge_client_config: Option<Arc<ForgeClientConfig>>,
    ) -> Result<Self, String> {
        let serving = Arc::new(MachineIdentityServing::try_default()?);
        Ok(Self {
            config: ArcSwapOption::new(None),
            machine_id: ArcSwapOption::new(None),
            forge_api,
            forge_client_config,
            outbound_governor: Arc::new(RateLimiter::direct(PHONE_HOME_RATE_LIMIT)),
            machine_identity: ArcSwap::new(serving),
            last_machine_identity_params: ArcSwapOption::new(None),
        })
    }

    /// Applies gRPC `machine_identity` only when it differs from the last applied config.
    pub fn apply_machine_identity_from_proto(
        &self,
        mi: rpc::fmds::FmdsMachineIdentityConfig,
    ) -> Result<(), String> {
        let params = MachineIdentityParams::try_from(&mi)?;
        if self.last_machine_identity_params.load_full().as_deref() == Some(&params) {
            return Ok(());
        }
        let serving = MachineIdentityServing::try_from_params(params.clone())?;
        self.machine_identity.store(Arc::new(serving));
        self.last_machine_identity_params
            .store(Some(Arc::new(params)));
        Ok(())
    }

    pub fn update_config(&self, config: FmdsConfig) {
        // Stash the machine_id separately for phone_home lookups.
        if let Some(ref mid) = config.machine_id {
            self.machine_id.store(Some(Arc::new(*mid)));
        }
        self.config.store(Some(Arc::new(config)));
    }
}

/// FmdsConfig is the data FMDS serves to tenants.
/// Populated from FmdsConfigUpdate proto.
#[derive(Clone, Debug)]
pub struct FmdsConfig {
    pub public_ipv4: Option<Ipv4Addr>,
    pub public_ipv6: Option<Ipv6Addr>,
    pub hostname: String,
    pub instance_name: Option<String>,
    pub sitename: Option<String>,
    pub instance_id: Option<InstanceId>,
    pub machine_id: Option<MachineId>,
    pub user_data: String,
    pub ib_devices: Option<Vec<IBDeviceConfig>>,
    pub asn: u32,
}

#[derive(Clone, Debug)]
pub struct IBDeviceConfig {
    pub pf_guid: String,
    pub instances: Vec<IBInstanceConfig>,
}

#[derive(Clone, Debug)]
pub struct IBInstanceConfig {
    pub ib_partition_id: Option<IBPartitionId>,
    pub ib_guid: Option<String>,
    pub lid: u32,
}

#[cfg(test)]
mod tests {
    use rpc::fmds::FmdsMachineIdentityConfig;

    use super::*;

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

    #[test]
    fn machine_identity_apply_skips_when_unchanged() {
        let state = FmdsState::try_new("https://api.test".to_string(), None).unwrap();
        let mi = FmdsMachineIdentityConfig {
            requests_per_second: 5,
            burst: 10,
            wait_timeout_secs: 3,
            sign_timeout_secs: 6,
            sign_proxy_url: None,
            sign_proxy_tls_root_ca: None,
        };
        state.apply_machine_identity_from_proto(mi.clone()).unwrap();
        let p_first = Arc::as_ptr(&*state.machine_identity.load());
        state.apply_machine_identity_from_proto(mi).unwrap();
        let p_second = Arc::as_ptr(&*state.machine_identity.load());
        assert_eq!(p_first, p_second);
    }

    #[test]
    fn machine_identity_apply_replaces_when_changed() {
        let state = FmdsState::try_new("https://api.test".to_string(), None).unwrap();
        let mi1 = FmdsMachineIdentityConfig {
            requests_per_second: 5,
            burst: 10,
            wait_timeout_secs: 3,
            sign_timeout_secs: 6,
            sign_proxy_url: None,
            sign_proxy_tls_root_ca: None,
        };
        let mi2 = FmdsMachineIdentityConfig {
            requests_per_second: 6,
            burst: 10,
            wait_timeout_secs: 3,
            sign_timeout_secs: 6,
            sign_proxy_url: None,
            sign_proxy_tls_root_ca: None,
        };
        state.apply_machine_identity_from_proto(mi1).unwrap();
        let p1 = Arc::as_ptr(&*state.machine_identity.load());
        state.apply_machine_identity_from_proto(mi2).unwrap();
        let p2 = Arc::as_ptr(&*state.machine_identity.load());
        assert_ne!(p1, p2);
    }

    #[test]
    fn test_new_state_starts_empty() {
        let state = FmdsState::try_new("https://api.test".to_string(), None).unwrap();
        assert!(state.config.load_full().is_none());
        assert!(state.machine_id.load_full().is_none());
    }

    #[test]
    fn test_update_config_stores_config() {
        let state = FmdsState::try_new("https://api.test".to_string(), None).unwrap();
        let config = make_test_config();

        state.update_config(config);

        let loaded = state.config.load_full().unwrap();
        assert_eq!(loaded.public_ipv4, Some("10.0.0.1".parse().unwrap()));
        assert_eq!(loaded.public_ipv6, Some("2001:db8::1".parse().unwrap()));
        assert_eq!(loaded.hostname, "test-host");
        assert_eq!(loaded.instance_name.as_deref(), Some("test-instance"));
        assert_eq!(loaded.sitename.as_deref(), Some("test-site"));
        assert_eq!(loaded.user_data, "cloud-init-data");
        assert_eq!(loaded.asn, 65000);
    }

    #[test]
    fn test_update_config_extracts_machine_id() {
        let state = FmdsState::try_new("https://api.test".to_string(), None).unwrap();
        let config = make_test_config();
        let expected_mid = config.machine_id.unwrap();

        state.update_config(config);

        let loaded_mid = state.machine_id.load_full().unwrap();
        assert_eq!(*loaded_mid, expected_mid);
    }

    #[test]
    fn test_update_config_without_machine_id() {
        let state = FmdsState::try_new("https://api.test".to_string(), None).unwrap();
        let config = FmdsConfig {
            machine_id: None,
            ..make_test_config()
        };

        state.update_config(config);

        assert!(state.config.load_full().is_some());
        assert!(state.machine_id.load_full().is_none());
    }

    #[test]
    fn test_update_config_replaces_previous() {
        let state = FmdsState::try_new("https://api.test".to_string(), None).unwrap();

        state.update_config(make_test_config());
        assert_eq!(state.config.load_full().unwrap().hostname, "test-host");

        state.update_config(FmdsConfig {
            hostname: "updated-host".to_string(),
            ..make_test_config()
        });
        assert_eq!(state.config.load_full().unwrap().hostname, "updated-host");
    }
}
