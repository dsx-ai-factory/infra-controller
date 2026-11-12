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

use std::net::SocketAddr;
use std::sync::Arc;

use bmc_drivers::{
    DriverMap, Drivers, ResolvedSelection, Rules, SelectError, SelectedDrivers, SelectionHash,
};
use bmc_platform::{ClassifyBmcError, IpmiOps, OpCx, PlatformIdentity};
use carbide_secrets::credentials::{BmcCredentialType, CredentialKey};
use carbide_utils::redfish::{BmcAccessInfo, parse_uri_host_ip};
use mac_address::MacAddress;
use model::site_explorer::EndpointExplorationReport;
use nv_redfish::{Bmc, ServiceRoot};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::platform_identity;

/// Secret-free, serializable identity of a BMC access endpoint.
///
/// Deserialization applies the same validation as [`BmcRef::new`], so a
/// decoded reference always carries a per-BMC root credential key.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(try_from = "BmcRefWire")]
pub struct BmcRef {
    address: SocketAddr,
    credential_key: CredentialKey,
    #[serde(default)]
    identity: PlatformIdentity,
    selection: ResolvedSelection,
}

#[derive(Deserialize)]
struct BmcRefWire {
    address: SocketAddr,
    credential_key: CredentialKey,
    #[serde(default)]
    identity: PlatformIdentity,
    selection: ResolvedSelection,
}

impl TryFrom<BmcRefWire> for BmcRef {
    type Error = BmcRefError;

    fn try_from(wire: BmcRefWire) -> Result<Self, Self::Error> {
        Self::new(
            wire.address,
            wire.credential_key,
            wire.identity,
            wire.selection,
        )
    }
}

impl BmcRef {
    /// Creates a secret-free endpoint reference.
    ///
    /// The BMC MAC address lives only in `credential_key`, which must be the
    /// per-BMC root credential key.
    pub fn new(
        address: SocketAddr,
        credential_key: CredentialKey,
        identity: PlatformIdentity,
        selection: ResolvedSelection,
    ) -> Result<Self, BmcRefError> {
        if root_mac_address(&credential_key).is_none() {
            return Err(BmcRefError::UnsupportedCredentialKey);
        }
        Ok(Self {
            address,
            credential_key,
            identity,
            selection,
        })
    }

    /// Builds a runtime reference from canonical access metadata, selecting
    /// drivers by `rules` for the identity `report` recorded. Without a report
    /// that recorded the service root, nothing tells vendors apart, so every
    /// capability gets its standard driver.
    pub fn from_exploration(
        access: &BmcAccessInfo,
        report: Option<&EndpointExplorationReport>,
        rules: &Rules,
    ) -> Result<Self, ExploredBmcRefError> {
        let ip = parse_uri_host_ip(&access.host)
            .ok_or_else(|| BmcRefError::InvalidIpAddress(access.host.clone()))?;
        let identity = match report.filter(|report| report.service_root.is_some()) {
            Some(report) => platform_identity(report),
            None => {
                tracing::debug!(
                    bmc_ip_address = %ip,
                    "BMC has no explored service root; using the standard drivers"
                );
                PlatformIdentity::default()
            }
        };
        let selection = Drivers::select(rules, &identity)?;
        let credential_key = CredentialKey::BmcCredentials {
            credential_type: BmcCredentialType::BmcRoot {
                bmc_mac_address: access.mac_address,
            },
        };
        Ok(Self::new(
            SocketAddr::new(ip, access.port.unwrap_or(443)),
            credential_key,
            identity,
            selection,
        )?)
    }

    /// Returns the concrete BMC socket address required by the client pool.
    pub const fn address(&self) -> SocketAddr {
        self.address
    }

    /// Returns the BMC MAC address used for credential lookup.
    pub fn mac_address(&self) -> MacAddress {
        // Both constructors validated the key.
        root_mac_address(&self.credential_key)
            .expect("BmcRef only accepts per-BMC root credential keys")
    }

    /// Returns the secret-free key used by credential and IPMI providers.
    pub const fn credential_key(&self) -> &CredentialKey {
        &self.credential_key
    }

    /// Returns the identity and resource ids the selection matched on.
    pub const fn identity(&self) -> &PlatformIdentity {
        &self.identity
    }

    /// Returns the complete driver map selected for this BMC.
    pub const fn driver_map(&self) -> &DriverMap {
        &self.selection.drivers
    }

    /// Returns the complete selection decision for this BMC.
    pub const fn selection(&self) -> &ResolvedSelection {
        &self.selection
    }

    /// Returns the hash of the rule set that produced the selection.
    pub const fn selection_hash(&self) -> SelectionHash {
        self.selection.hash
    }

    /// Whether the selection was produced by `rules`, which a reference
    /// deserialized after a rule change may not be.
    pub fn selection_is_current(&self, rules: &Rules) -> bool {
        self.selection_hash() == rules.hash()
    }
}

const fn root_mac_address(key: &CredentialKey) -> Option<MacAddress> {
    match key {
        CredentialKey::BmcCredentials {
            credential_type: BmcCredentialType::BmcRoot { bmc_mac_address },
        } => Some(*bmc_mac_address),
        _ => None,
    }
}

/// Failure to convert generic Redfish access data into a pool endpoint.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum BmcRefError {
    /// The configured host is not an IP literal.
    #[error("BMC host must be an IP literal for pooled access: {0}")]
    InvalidIpAddress(String),
    /// Runtime BMC references require per-BMC root credentials.
    #[error("BMC reference must use a per-BMC root credential key")]
    UnsupportedCredentialKey,
}

/// Failure to build a runtime reference from an exploration report.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum ExploredBmcRefError {
    #[error(transparent)]
    Select(#[from] SelectError),
    #[error(transparent)]
    Reference(#[from] BmcRefError),
}

/// A live Redfish endpoint using the driver map selected for its BMC.
///
/// The selection is resolved through the compiled catalogue once, on
/// connect, so a controller writes `bmc.power()?.set(reset_type)` and never
/// sees driver ids or operation contexts.
pub struct ConnectedBmc<B: Bmc + 'static> {
    endpoint: BmcRef,
    bmc: Arc<B>,
    service_root: Arc<ServiceRoot<B>>,
    ipmi: Option<Arc<dyn IpmiOps>>,
    drivers: SelectedDrivers<B>,
}

impl<B: Bmc + 'static> ConnectedBmc<B>
where
    B::Error: ClassifyBmcError,
{
    /// Creates a connected endpoint without repeating exploration or selection.
    pub fn new(
        endpoint: BmcRef,
        bmc: Arc<B>,
        service_root: Arc<ServiceRoot<B>>,
        ipmi: Option<Arc<dyn IpmiOps>>,
    ) -> Self {
        let drivers = SelectedDrivers::resolve(endpoint.driver_map());
        Self {
            endpoint,
            bmc,
            service_root,
            ipmi,
            drivers,
        }
    }

    /// Replaces the resolved drivers, for tests that script driver behavior.
    pub fn with_drivers(mut self, drivers: SelectedDrivers<B>) -> Self {
        self.drivers = drivers;
        self
    }

    /// The drivers selected for this BMC.
    pub(crate) const fn drivers(&self) -> &SelectedDrivers<B> {
        &self.drivers
    }

    /// Returns the unconnected endpoint metadata.
    pub const fn endpoint(&self) -> &BmcRef {
        &self.endpoint
    }

    /// Returns the authenticated transport, for requests that need no
    /// resolved system or manager.
    pub(crate) fn bmc(&self) -> &B {
        &self.bmc
    }

    pub(crate) fn replace_redfish(&mut self, bmc: Arc<B>, service_root: Arc<ServiceRoot<B>>) {
        self.bmc = bmc;
        self.service_root = service_root;
    }

    /// Builds an operation context; the selected system and manager are
    /// resolved on first use.
    pub(crate) fn operation_context(&self) -> OpCx<'_, B> {
        let context = OpCx::new(
            self.bmc.as_ref(),
            self.service_root.as_ref(),
            self.endpoint.identity(),
        )
        .with_quirks(&self.endpoint.selection().quirks);
        match self.ipmi.as_deref() {
            Some(ipmi) => context.with_ipmi(ipmi),
            None => context,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use bmc_drivers::CapabilitySelection;
    use carbide_secrets::credentials::{BmcCredentialType, CredentialKey};

    use super::*;
    use crate::test_support::{root_credential_key, selection};

    fn unsupported_driver_map() -> DriverMap {
        DriverMap::filled(CapabilitySelection::Unsupported)
    }

    fn report_with_service_root() -> EndpointExplorationReport {
        EndpointExplorationReport {
            service_root: Some(model::site_explorer::ServiceRoot {
                vendor: Some("Dell".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn bmc_ref_round_trip_preserves_selection_without_secrets() {
        let mac_address = MacAddress::new([2, 0, 0, 0, 0, 1]);
        let driver_map = unsupported_driver_map();
        let reference = BmcRef::new(
            "192.0.2.10:8443".parse().expect("valid socket address"),
            root_credential_key(mac_address),
            PlatformIdentity::default(),
            selection(driver_map.clone(), BTreeSet::new()),
        )
        .expect("root credential key is valid");

        let encoded = serde_json::to_string(&reference).expect("BMC reference serializes");
        let decoded = serde_json::from_str::<BmcRef>(&encoded).expect("BMC reference deserializes");
        assert_eq!(decoded.address(), reference.address());
        assert_eq!(decoded.mac_address(), mac_address);
        assert_eq!(decoded.driver_map(), &driver_map);
        assert_eq!(decoded.selection_hash(), reference.selection_hash());
        assert!(
            decoded.selection_is_current(&Rules::new(Vec::new()).expect("empty rules are valid"))
        );
        assert!(encoded.contains("192.0.2.10"));
        assert!(!encoded.to_ascii_lowercase().contains("password"));
        assert!(!encoded.to_ascii_lowercase().contains("token"));

        let site_wide = encoded.replace(
            &serde_json::to_string(&root_credential_key(mac_address)).expect("key serializes"),
            &serde_json::to_string(&CredentialKey::BmcCredentials {
                credential_type: BmcCredentialType::SiteWideRoot,
            })
            .expect("key serializes"),
        );
        assert!(
            serde_json::from_str::<BmcRef>(&site_wide).is_err(),
            "non-root credential keys are rejected on decode"
        );
    }

    /// Without a recorded service root, the built-in rules match nothing, so
    /// the reference carries the standard drivers and no quirks.
    #[test]
    fn exploration_without_a_service_root_selects_the_standard_drivers() {
        let access = BmcAccessInfo {
            host: "192.0.2.14".to_string(),
            port: None,
            mac_address: MacAddress::new([2, 0, 0, 0, 0, 4]),
        };
        let standard = Drivers::select(
            &Rules::new(Vec::new()).expect("empty rules are valid"),
            &PlatformIdentity::default(),
        )
        .expect("no rules select the standard drivers");

        for report in [None, Some(&EndpointExplorationReport::default())] {
            let endpoint =
                BmcRef::from_exploration(&access, report, &bmc_drivers::built_in_rules())
                    .expect("a missing service root selects the standard drivers");
            assert_eq!(endpoint.identity(), &PlatformIdentity::default());
            assert_eq!(endpoint.selection().drivers, standard.drivers);
            assert_eq!(endpoint.selection().quirks, standard.quirks);
        }
    }

    #[test]
    fn exploration_reference_selects_by_the_recorded_identity() {
        let mac_address = MacAddress::new([2, 0, 0, 0, 0, 4]);
        let access = BmcAccessInfo {
            host: "192.0.2.14".to_string(),
            port: None,
            mac_address,
        };
        let rules = Rules::new(Vec::new()).expect("empty rules are valid");

        let endpoint = BmcRef::from_exploration(&access, Some(&report_with_service_root()), &rules)
            .expect("a report with a service root selects drivers");
        assert_eq!(
            endpoint.address(),
            "192.0.2.14:443".parse().expect("valid socket address")
        );
        assert_eq!(endpoint.mac_address(), mac_address);
        assert!(matches!(
            endpoint.credential_key(),
            CredentialKey::BmcCredentials {
                credential_type: BmcCredentialType::BmcRoot { bmc_mac_address },
            } if *bmc_mac_address == mac_address
        ));
        assert_eq!(
            endpoint.identity().service_root.vendor.as_deref(),
            Some("Dell")
        );
        assert!(endpoint.selection_is_current(&rules));
    }

    #[test]
    fn endpoint_rejects_hostname_without_dns_resolution() {
        assert!(matches!(
            BmcRef::from_exploration(
                &BmcAccessInfo {
                    host: "bmc.example.test".to_string(),
                    port: None,
                    mac_address: MacAddress::new([2, 0, 0, 0, 0, 2]),
                },
                Some(&report_with_service_root()),
                &Rules::new(Vec::new()).expect("empty rules are valid"),
            ),
            Err(ExploredBmcRefError::Reference(BmcRefError::InvalidIpAddress(host)))
                if host == "bmc.example.test"
        ));
    }

    #[test]
    fn bmc_ref_rejects_non_device_credentials() {
        let address = "192.0.2.11:443".parse().expect("valid socket address");
        assert!(matches!(
            BmcRef::new(
                address,
                CredentialKey::BmcCredentials {
                    credential_type: BmcCredentialType::SiteWideRoot,
                },
                PlatformIdentity::default(),
                selection(unsupported_driver_map(), BTreeSet::new()),
            ),
            Err(BmcRefError::UnsupportedCredentialKey)
        ));
    }
}
