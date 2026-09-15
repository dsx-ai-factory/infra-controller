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

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::future::Future;
use std::net::IpAddr;
use std::sync::Arc;

use carbide_uuid::machine::MachineId;
use carbide_uuid::nvlink::NvLinkDomainId;
use carbide_uuid::power_shelf::PowerShelfId;
use carbide_uuid::rack::RackId;
use carbide_uuid::switch::SwitchId;
use mac_address::MacAddress;
use tokio::sync::OnceCell;
use url::Url;

use crate::HealthError;
use crate::bmc::{BmcClient, BoxFuture};

/// Shared, write-once UUID reported by a machine's primary ComputerSystem.
///
/// Collectors clone endpoint metadata when they start, so this state must remain
/// shared for a UUID resolved after collector startup to reach emitted events.
/// Clones of the same state compare equal. Distinct states compare equal only
/// after both cells are initialized with the same optional UUID.
#[derive(Clone, Debug, Default)]
pub struct SharedSystemUuid(Arc<OnceCell<Option<uuid::Uuid>>>);

impl PartialEq for SharedSystemUuid {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
            || matches!((self.0.get(), other.0.get()), (Some(left), Some(right)) if left == right)
    }
}

impl SharedSystemUuid {
    pub fn get(&self) -> Option<uuid::Uuid> {
        self.0.get().copied().flatten()
    }

    pub(crate) async fn get_or_try_init<E, F, Fut>(&self, f: F) -> Result<Option<uuid::Uuid>, E>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<Option<uuid::Uuid>, E>>,
    {
        self.0.get_or_try_init(f).await.copied()
    }
}

impl From<Option<uuid::Uuid>> for SharedSystemUuid {
    fn from(system_uuid: Option<uuid::Uuid>) -> Self {
        match system_uuid {
            Some(system_uuid) => Self(Arc::new(OnceCell::new_with(Some(Some(system_uuid))))),
            None => Self::default(),
        }
    }
}

#[derive(Clone)]
pub struct BmcEndpoint {
    pub addr: BmcAddr,
    pub metadata: Option<EndpointMetadata>,
    pub rack_id: Option<RackId>,
    pub labels: BTreeMap<String, String>,
    pub bmc: Arc<BmcClient>,
}

impl BmcEndpoint {
    pub fn key(&self) -> String {
        self.addr.mac.to_string()
    }

    pub fn hash_key(&self) -> Cow<'static, str> {
        Cow::Owned(
            self.rack_id
                .as_ref()
                .map(|id| id.to_string())
                .unwrap_or_else(|| self.key()),
        )
    }

    /// Returns the endpoint identity used for collector log state.
    ///
    /// Machines prefer their NICo ID, switches use their serial number, and PowerShelves prefer
    /// their serial number followed by their NICo ID. Other cases use the BMC MAC address.
    pub fn log_identity(&self) -> Cow<'_, str> {
        match &self.metadata {
            Some(EndpointMetadata::Machine(MachineData {
                machine_id: Some(id),
                ..
            })) => Cow::Owned(id.to_string()),
            Some(EndpointMetadata::PowerShelf(power_shelf)) => {
                if let Some(serial) = power_shelf.serial.as_deref() {
                    Cow::Borrowed(serial)
                } else if let Some(id) = power_shelf.id {
                    Cow::Owned(id.to_string())
                } else {
                    Cow::Owned(self.addr.mac.to_string())
                }
            }
            Some(EndpointMetadata::Switch(switch)) => Cow::Borrowed(&switch.serial),
            _ => Cow::Owned(self.addr.mac.to_string()),
        }
    }

    /// Returns whether this endpoint supports periodic Redfish log collection.
    pub(crate) fn supports_periodic_logs(&self) -> bool {
        match self.metadata.as_ref() {
            Some(EndpointMetadata::Machine(_)) => true,
            Some(EndpointMetadata::Switch(switch)) => {
                switch.endpoint_role == SwitchEndpointRole::Bmc
            }
            Some(EndpointMetadata::PowerShelf(_)) => {
                // Power shelves may expose compatible LogServices, but behavior depends on
                // hardware and firmware. Keep collection disabled until future implementation
                // and validation establish support.
                false
            }
            None => false,
        }
    }

    pub fn bmc(&self) -> &Arc<BmcClient> {
        &self.bmc
    }

    pub fn switch_data(&self) -> Option<&SwitchData> {
        self.metadata.as_ref().and_then(EndpointMetadata::as_switch)
    }

    /// Returns the connect host direct switch collectors should place in URIs.
    ///
    /// Switch collectors connect to the discovered endpoint IP address. DNS
    /// names used for TLS verification are handled separately.
    pub fn switch_connect_host_for_uri(&self) -> Cow<'_, str> {
        match self.addr.ip {
            IpAddr::V4(ip) => Cow::Owned(ip.to_string()),
            IpAddr::V6(ip) => Cow::Owned(format!("[{ip}]")),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum EndpointMetadata {
    Machine(MachineData),
    PowerShelf(PowerShelfData),
    Switch(SwitchData),
}

impl EndpointMetadata {
    pub fn as_switch(&self) -> Option<&SwitchData> {
        match self {
            EndpointMetadata::Switch(switch) => Some(switch),
            _ => None,
        }
    }

    /// Returns the hardware serial number when the endpoint metadata provides one.
    ///
    /// Machine and PowerShelf serial numbers may be absent; switch serial numbers are always
    /// present.
    pub fn serial_number(&self) -> Option<&str> {
        match self {
            EndpointMetadata::Machine(machine) => machine.machine_serial.as_deref(),
            EndpointMetadata::PowerShelf(power_shelf) => power_shelf.serial.as_deref(),
            EndpointMetadata::Switch(switch) => Some(switch.serial.as_str()),
        }
    }

    /// Returns the PHR component category represented by this endpoint metadata.
    pub const fn component_type(&self) -> &'static str {
        match self {
            Self::Machine(_) => "compute_node",
            Self::PowerShelf(_) => "power_shelf",
            Self::Switch(_) => "nvlink_switch",
        }
    }
}

/// Metadata that describes a machine endpoint for health telemetry.
#[derive(Clone, Debug, PartialEq)]
pub struct MachineData {
    /// Stable NICo machine identifier. None when running without NICo.
    pub machine_id: Option<MachineId>,

    /// Hardware chassis serial discovered from machine DMI data, when known.
    pub machine_serial: Option<String>,

    /// UUID reported by the primary Redfish ComputerSystem resource.
    ///
    /// Endpoint discovery resolves this on demand. The write-once shared state
    /// propagates enrichment to collectors that already started and records
    /// both present and absent UUID results so successful BMC queries happen
    /// only once.
    pub system_uuid: SharedSystemUuid,

    /// Physical rack slot where the machine is installed, when known.
    pub slot_number: Option<i32>,

    /// Compute tray index where the machine is installed, when known.
    pub tray_index: Option<i32>,

    /// NVLink domain UUID for the machine, when it participates in an NVLink domain.
    pub nvlink_domain_uuid: Option<NvLinkDomainId>,

    /// Machine-level GPU driver version.
    ///
    /// This is populated only when API discovery reports exactly one unique
    /// non-empty GPU driver version for the machine. It stays absent when the
    /// version is unknown or the discovered GPUs report conflicting versions.
    pub driver_version: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PowerShelfData {
    pub id: Option<PowerShelfId>,
    /// Hardware serial number, when explicitly known.
    pub serial: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SwitchEndpointRole {
    Bmc,
    Host,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SwitchData {
    pub id: Option<SwitchId>,
    pub serial: String,
    pub slot_number: Option<i32>,
    pub tray_index: Option<i32>,

    /// NVLink domain UUID associated with the switch, when known.
    ///
    /// Discovery restarts collectors when this value changes so subsequent
    /// telemetry uses current metadata.
    pub nvlink_domain_uuid: Option<NvLinkDomainId>,

    pub endpoint_role: SwitchEndpointRole,
    pub is_primary: bool,
    pub nmxc_enabled: bool,
    pub nmxt_enabled: bool,
}

#[derive(Clone)]
pub enum BmcCredentials {
    UsernamePassword {
        username: String,
        password: Option<String>,
    },
    SessionToken {
        token: String,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct BmcAddr {
    pub ip: IpAddr,
    pub port: Option<u16>,
    pub mac: MacAddress,
}

impl BmcAddr {
    /// Builds the BMC base URL. IPv6 literals are bracketed so the URL
    /// authority parses — a bare `IpAddr` Display leaves IPv6 unbracketed,
    /// which `Url::parse` would otherwise reject.
    pub fn to_url(&self) -> Result<Url, url::ParseError> {
        let scheme = if self.port.is_some_and(|v| v == 80) {
            "http"
        } else {
            "https"
        };
        // Bracket IPv6 hosts; IPv4 renders unchanged.
        let host = match self.ip {
            IpAddr::V4(v4) => v4.to_string(),
            IpAddr::V6(v6) => format!("[{v6}]"),
        };
        let mut url = Url::parse(&format!("{scheme}://{host}"))?;
        let _ = url.set_port(self.port);
        Ok(url)
    }
}

impl From<BmcCredentials> for nv_redfish::bmc_http::BmcCredentials {
    fn from(value: BmcCredentials) -> Self {
        match value {
            BmcCredentials::UsernamePassword { username, password } => {
                nv_redfish::bmc_http::BmcCredentials::username_password(username, password)
            }
            BmcCredentials::SessionToken { token } => {
                nv_redfish::bmc_http::BmcCredentials::token(token)
            }
        }
    }
}

pub trait EndpointSource: Send + Sync {
    fn fetch_bmc_hosts<'a>(&'a self) -> BoxFuture<'a, Result<Vec<Arc<BmcEndpoint>>, HealthError>>;
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;
    use std::net::IpAddr;
    use std::str::FromStr;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use carbide_test_support::{Check, check_values};
    use carbide_uuid::power_shelf::PowerShelfId;
    use mac_address::MacAddress;

    use super::{
        BmcAddr, BmcCredentials, EndpointMetadata, MachineData, PowerShelfData, SharedSystemUuid,
        SwitchData, SwitchEndpointRole,
    };
    use crate::endpoint::test_support::{endpoint_with_creds, mac, test_endpoint};

    fn addr(ip: &str, port: Option<u16>) -> BmcAddr {
        BmcAddr {
            ip: IpAddr::from_str(ip).unwrap(),
            port,
            mac: MacAddress::from_str("00:11:22:33:44:55").unwrap(),
        }
    }

    // A v6 BMC IP must render as a bracketed URL authority, else Url::parse rejects it.
    #[test]
    fn to_url_brackets_ipv6() {
        let url = addr("2001:db8::1", Some(443)).to_url().unwrap();
        assert_eq!(url.scheme(), "https");
        assert_eq!(url.host_str(), Some("[2001:db8::1]"));
    }

    // v4 hosts are byte-identical to the old behaviour.
    #[test]
    fn to_url_v4_unchanged() {
        let url = addr("10.0.0.5", Some(443)).to_url().unwrap();
        assert_eq!(url.scheme(), "https");
        assert_eq!(url.host_str(), Some("10.0.0.5"));
    }

    // Port 80 selects the http scheme (v6 still bracketed).
    #[test]
    fn to_url_port_80_is_http() {
        let url = addr("2001:db8::1", Some(80)).to_url().unwrap();
        assert_eq!(url.scheme(), "http");
        assert_eq!(url.host_str(), Some("[2001:db8::1]"));
    }

    #[test]
    fn switch_connect_host_for_uri_brackets_ipv6() {
        let endpoint = endpoint_with_creds(
            addr("2001:db8::1", Some(443)),
            BmcCredentials::UsernamePassword {
                username: "admin".to_string(),
                password: Some("pass".to_string()),
            },
            None,
            None,
        );

        assert_eq!(endpoint.switch_connect_host_for_uri(), "[2001:db8::1]");
    }

    #[test]
    fn periodic_log_collection_endpoint_eligibility() {
        let switch_bmc = SwitchData {
            id: None,
            serial: "switch".to_string(),
            slot_number: Some(1),
            tray_index: Some(2),
            nvlink_domain_uuid: None,
            endpoint_role: SwitchEndpointRole::Bmc,
            is_primary: true,
            nmxc_enabled: false,
            nmxt_enabled: false,
        };

        let switch_host = SwitchData {
            endpoint_role: SwitchEndpointRole::Host,
            ..switch_bmc.clone()
        };

        check_values(
            [
                Check {
                    scenario: "machine BMC is eligible",
                    input: Some(EndpointMetadata::Machine(MachineData {
                        machine_id: None,
                        machine_serial: None,
                        system_uuid: SharedSystemUuid::default(),
                        slot_number: None,
                        tray_index: None,
                        nvlink_domain_uuid: None,
                        driver_version: None,
                    })),
                    expect: true,
                },
                Check {
                    scenario: "switch BMC is eligible",
                    input: Some(EndpointMetadata::Switch(switch_bmc)),
                    expect: true,
                },
                Check {
                    scenario: "switch host is not eligible",
                    input: Some(EndpointMetadata::Switch(switch_host)),
                    expect: false,
                },
                Check {
                    scenario: "power shelf is not eligible",
                    input: Some(EndpointMetadata::PowerShelf(PowerShelfData {
                        id: None,
                        serial: None,
                    })),
                    expect: false,
                },
                Check {
                    scenario: "endpoint without metadata is not eligible",
                    input: None,
                    expect: false,
                },
            ],
            |metadata| {
                let mut endpoint = test_endpoint(mac("00:11:22:33:44:55"));
                endpoint.metadata = metadata;

                endpoint.supports_periodic_logs()
            },
        );
    }

    #[test]
    fn power_shelf_log_identity_falls_back_to_id_then_mac() {
        let power_shelf_id =
            PowerShelfId::from_str("ps100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg")
                .expect("valid power shelf id");

        check_values(
            [
                Check {
                    scenario: "PowerShelf ID is available",
                    input: Some(power_shelf_id),
                    expect: power_shelf_id.to_string(),
                },
                Check {
                    scenario: "PowerShelf ID is unavailable",
                    input: None,
                    expect: "00:11:22:33:44:55".to_string(),
                },
            ],
            |id| {
                let mut endpoint = test_endpoint(mac("00:11:22:33:44:55"));
                endpoint.metadata = Some(EndpointMetadata::PowerShelf(PowerShelfData {
                    id,
                    serial: None,
                }));

                endpoint.log_identity().into_owned()
            },
        );
    }

    #[tokio::test]
    async fn shared_system_uuid_caches_absent_result_across_clones() {
        let state = SharedSystemUuid::default();
        let clone = state.clone();
        let query_count = AtomicUsize::new(0);

        for state in [&state, &clone] {
            state
                .get_or_try_init(|| async {
                    query_count.fetch_add(1, Ordering::SeqCst);
                    Ok::<_, Infallible>(None)
                })
                .await
                .expect("infallible UUID initialization");
        }

        assert_eq!(query_count.load(Ordering::SeqCst), 1);
        assert_eq!(state.get(), None);
        assert_eq!(clone.get(), None);
    }

    #[tokio::test]
    async fn shared_system_uuid_equality_distinguishes_unresolved_from_absent() {
        let unresolved = SharedSystemUuid::default();
        let other_unresolved = SharedSystemUuid::default();

        assert_eq!(unresolved, unresolved.clone());
        assert_ne!(unresolved, other_unresolved);

        let initialized_absent = SharedSystemUuid::default();
        initialized_absent
            .get_or_try_init(|| async { Ok::<_, Infallible>(None) })
            .await
            .expect("infallible UUID initialization");

        assert_ne!(initialized_absent, unresolved);
    }
}
