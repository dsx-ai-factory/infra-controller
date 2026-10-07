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

use std::path::{Path, PathBuf};
use std::string::ToString;

use forge_dpu_agent_utils::machine_identity::defaults::{
    BURST, REQUESTS_PER_SECOND, SIGN_TIMEOUT_SECS, WAIT_TIMEOUT_SECS,
};
use forge_tls::default as tls_default;
use serde::{Deserialize, Serialize};

/// HBN container root
const HBN_DEFAULT_ROOT: &str = "/var/lib/hbn";

/// Where DPU agent will try to connect to carbide-api
/// Unbound should define this in all environments
const DEFAULT_API_SERVER: &str = "https://carbide-api.forge";

// TODO(ianderson) we need to figure out the addresses on which those services should run
const INSTANCE_METADATA_SERVICE_ADDRESS: &str = "0.0.0.0:7777";
const TELEMETRY_METRICS_SERVICE_ADDRESS: &str = "0.0.0.0:8888";

/// The sub-part of the agent config that PXE server sets
///
/// This is what we WRITE to /etc/forge/config.toml
#[derive(Debug, Clone, Serialize)]
pub struct AgentConfigFromPxe {
    // This is primarily used in the case of "external" overrides. If a host is
    // being provisioned from an external location, this will ensure we correctly
    // populate the carbide-api endpoint with CARBIDE_EXTERNAL_API_URL, and
    // not [defaulting] to carbide-api.forge, to allow scout to work.
    #[serde(rename = "forge-system", skip_serializing_if = "Option::is_none")]
    pub forge_system: Option<ForgeSystemConfigFromPxe>,
    pub machine: MachineConfigFromPxe,
}

/// Optional forge-system overrides written by PXE for external hosts
/// whose DPU agents can't resolve the default internal hostname.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct ForgeSystemConfigFromPxe {
    pub api_server: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct MachineConfigFromPxe {
    pub interface_id: carbide_uuid::machine::MachineInterfaceId,
}

/// Describes the format of the configuration files that is used by Forge agents
/// that run on the DPU and host
///
/// This is what we READ from /etc/forge/config.toml. In prod most of the fields will default.
/// We only implement Serialize for unit tests.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentConfig {
    #[serde(default, rename = "forge-system")]
    pub forge_system: ForgeSystemConfig,
    #[serde(default)]
    pub machine: MachineConfig,
    #[serde(default, rename = "metadata-service")]
    pub metadata_service: MetadataServiceConfig,
    #[serde(default)]
    pub telemetry: TelemetryConfig,
    #[serde(default)]
    pub hbn: HBNConfig,
    #[serde(default)]
    pub period: IterationTime,
    #[serde(default)]
    pub updates: UpdateConfig,
    #[serde(default, rename = "fmds-armos-networking")]
    pub fmds_armos_networking: FmdsDpuNetworkingConfig,
    #[serde(
        default,
        rename = "machine-identity",
        skip_serializing_if = "MachineIdentityConfig::is_default"
    )]
    pub machine_identity: MachineIdentityConfig,
    #[serde(default, skip_serializing_if = "NetworkConfig::is_default")]
    pub network: NetworkConfig,
}

impl AgentConfig {
    /// Loads the agent configuration file in toml format from the given path
    pub fn load_from(path: &Path) -> Result<Self, std::io::Error> {
        let data = std::fs::read_to_string(path)?;

        toml::from_str(&data).map_err(|e| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("Invalid AgentConfig toml data: {e}"),
            )
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct ForgeSystemConfig {
    #[serde(default = "default_api_server")]
    pub api_server: String,
    #[serde(default = "default_root_ca")]
    pub root_ca: String,
    #[serde(default = "default_client_cert")]
    pub client_cert: String,
    #[serde(default = "default_client_key")]
    pub client_key: String,
    /// Unix socket where the agent serves its local API (node tokens for
    /// co-located services, issue #355). Works the same containerized (DPF)
    /// and as a plain service on DPU OS; override when `/opt/forge` is not
    /// the shared credential directory in a deployment.
    #[serde(default = "default_local_api_socket")]
    pub local_api_socket: String,
}

// Called if no `[forge-system]` is provided at all.
// The serde defaults above are called if one or more fields are missing.
impl Default for ForgeSystemConfig {
    fn default() -> Self {
        Self {
            api_server: default_api_server(),
            root_ca: default_root_ca(),
            client_cert: default_client_cert(),
            client_key: default_client_key(),
            local_api_socket: default_local_api_socket(),
        }
    }
}

pub fn default_api_server() -> String {
    DEFAULT_API_SERVER.to_string()
}

pub fn default_root_ca() -> String {
    tls_default::default_root_ca().to_string()
}

pub fn default_client_cert() -> String {
    tls_default::default_client_cert().to_string()
}

pub fn default_client_key() -> String {
    tls_default::default_client_key().to_string()
}

pub fn default_local_api_socket() -> String {
    ::rpc::node_token_socket::DEFAULT_AGENT_LOCAL_SOCKET.to_string()
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct MachineConfig {
    pub interface_id: Option<uuid::Uuid>,
    /// Local dev only. Pretend to be a DPU for discovery.
    /// If it's set to false, don't even serialize it out
    /// to config.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_fake_dpu: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MetadataServiceConfig {
    pub address: String,
}

impl Default for MetadataServiceConfig {
    fn default() -> Self {
        Self {
            address: INSTANCE_METADATA_SERVICE_ADDRESS.to_string(),
        }
    }
}

/// Rate limit and timeout for `GET …/meta-data/identity` on the embedded metadata service
/// (paths such as `/latest/meta-data/identity` and `/2009-04-04/meta-data/identity`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct MachineIdentityConfig {
    /// Sustained admission rate (Generic Cell Rate Algorithm refill), in requests per second.
    /// Valid range: 1–20.
    #[serde(default = "default_machine_identity_requests_per_second")]
    pub requests_per_second: u8,
    /// Maximum burst (cells) allowed by the rate limiter. Valid range: 1–40.
    #[serde(default = "default_machine_identity_burst")]
    pub burst: u8,
    /// Max time to wait for a rate-limit permit before failing the request (seconds).
    /// Applies to governor wait (`until_ready`), not to the Forge signing call.
    /// Valid range: 1–10.
    #[serde(default = "default_machine_identity_wait_timeout_secs")]
    pub wait_timeout_secs: u8,
    /// Wall-clock limit for the full Forge signing path on `GET …/meta-data/identity`
    /// (client build including connect retries, plus `SignMachineIdentity` RPC).
    /// Valid range: 1–60 seconds. Applies to the Forge gRPC signing path and to the optional
    /// HTTP sign proxy origin (`sign-proxy-url`).
    #[serde(default = "default_machine_identity_sign_timeout_secs")]
    pub sign_timeout_secs: u8,
    /// When set, `GET …/meta-data/identity` is forwarded over HTTP to `{url}/latest/meta-data/identity`
    /// with the same query string; the upstream response (status, body, `Content-Type`) is returned
    /// verbatim. When unset, the agent uses `SignMachineIdentity` gRPC to Forge.
    #[serde(default, rename = "sign-proxy-url")]
    pub sign_proxy_url: Option<String>,
    /// PEM file (one or more concatenated certificates) trusted as additional TLS roots when
    /// connecting to `sign-proxy-url` over HTTPS. Ignored for `http:` sign-proxy URLs.
    /// Requires `sign-proxy-url` to be set.
    #[serde(default, rename = "sign-proxy-tls-root-ca")]
    pub sign_proxy_tls_root_ca: Option<String>,
}

fn default_machine_identity_requests_per_second() -> u8 {
    REQUESTS_PER_SECOND
}

fn default_machine_identity_burst() -> u8 {
    BURST
}

fn default_machine_identity_wait_timeout_secs() -> u8 {
    WAIT_TIMEOUT_SECS
}

fn default_machine_identity_sign_timeout_secs() -> u8 {
    SIGN_TIMEOUT_SECS
}

impl Default for MachineIdentityConfig {
    fn default() -> Self {
        Self {
            requests_per_second: default_machine_identity_requests_per_second(),
            burst: default_machine_identity_burst(),
            wait_timeout_secs: default_machine_identity_wait_timeout_secs(),
            sign_timeout_secs: default_machine_identity_sign_timeout_secs(),
            sign_proxy_url: None,
            sign_proxy_tls_root_ca: None,
        }
    }
}

impl MachineIdentityConfig {
    pub fn validate(&self) -> Result<(), String> {
        use forge_dpu_agent_utils::machine_identity::limits::{
            BURST_MAX, BURST_MIN, REQUESTS_PER_SECOND_MAX, REQUESTS_PER_SECOND_MIN,
            SIGN_TIMEOUT_SECS_MAX, SIGN_TIMEOUT_SECS_MIN, WAIT_TIMEOUT_SECS_MAX,
            WAIT_TIMEOUT_SECS_MIN,
        };

        if !(REQUESTS_PER_SECOND_MIN..=REQUESTS_PER_SECOND_MAX).contains(&self.requests_per_second)
        {
            return Err(format!(
                "machine-identity.requests-per-second: must be between {REQUESTS_PER_SECOND_MIN} and {REQUESTS_PER_SECOND_MAX} (inclusive)"
            ));
        }
        if !(BURST_MIN..=BURST_MAX).contains(&self.burst) {
            return Err(format!(
                "machine-identity.burst: must be between {BURST_MIN} and {BURST_MAX} (inclusive)"
            ));
        }
        if !(WAIT_TIMEOUT_SECS_MIN..=WAIT_TIMEOUT_SECS_MAX).contains(&self.wait_timeout_secs) {
            return Err(format!(
                "machine-identity.wait-timeout-secs: must be between {WAIT_TIMEOUT_SECS_MIN} and {WAIT_TIMEOUT_SECS_MAX} (inclusive)"
            ));
        }
        if !(SIGN_TIMEOUT_SECS_MIN..=SIGN_TIMEOUT_SECS_MAX).contains(&self.sign_timeout_secs) {
            return Err(format!(
                "machine-identity.sign-timeout-secs: must be between {SIGN_TIMEOUT_SECS_MIN} and {SIGN_TIMEOUT_SECS_MAX} (inclusive)"
            ));
        }
        if let Some(ref raw) = self.sign_proxy_url {
            let s = raw.trim();
            if s.is_empty() {
                return Err(
                    "machine-identity.sign-proxy-url: must not be empty or whitespace-only"
                        .to_string(),
                );
            }
            let u = url::Url::parse(s)
                .map_err(|e| format!("machine-identity.sign-proxy-url: invalid URL ({e})"))?;
            match u.scheme() {
                "http" | "https" => {}
                other => {
                    return Err(format!(
                        "machine-identity.sign-proxy-url: scheme must be http or https, got {other}"
                    ));
                }
            }
        }
        if let Some(ref raw_ca) = self.sign_proxy_tls_root_ca {
            let path = raw_ca.trim();
            if path.is_empty() {
                return Err(
                    "machine-identity.sign-proxy-tls-root-ca: must not be empty or whitespace-only"
                        .to_string(),
                );
            }
            let url_ok = self
                .sign_proxy_url
                .as_ref()
                .is_some_and(|u| !u.trim().is_empty());
            if !url_ok {
                return Err(
                    "machine-identity.sign-proxy-tls-root-ca: requires machine-identity.sign-proxy-url"
                        .to_string(),
                );
            }
            let pem = std::fs::read(path).map_err(|e| {
                format!("machine-identity.sign-proxy-tls-root-ca: failed to read {path}: {e}")
            })?;
            if pem.is_empty() {
                return Err(format!(
                    "machine-identity.sign-proxy-tls-root-ca: file is empty: {path}"
                ));
            }
            let mut cursor = std::io::Cursor::new(&pem[..]);
            let certs: Vec<_> = rustls_pemfile::certs(&mut cursor)
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| {
                    format!("machine-identity.sign-proxy-tls-root-ca: invalid PEM in {path}: {e}")
                })?;
            if certs.is_empty() {
                return Err(format!(
                    "machine-identity.sign-proxy-tls-root-ca: no certificates found in {path}"
                ));
            }
        }
        Ok(())
    }

    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// Agent-local network configuration knobs (the `[network]` section).
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct NetworkConfig {
    /// Path to an operator-supplied RFC 7386 JSON Merge Patch document that the
    /// agent merges into the `set` section of its generated NVUE config before
    /// every apply. Top-level keys must be NVUE config sections the agent
    /// already models (bridge, evpn, interface, nve, router, system, vrf, acl).
    /// The file's contents participate in change detection, so editing it in
    /// place triggers a re-render without an agent restart. When set, the file
    /// must exist and be readable; an unreadable file fails the network
    /// reconciliation loudly rather than silently applying an unmerged config.
    /// An empty string means "not configured", same as omitting the key.
    #[serde(default, deserialize_with = "empty_path_as_none")]
    pub supplemental_config_path: Option<PathBuf>,
}

impl NetworkConfig {
    // Only referenced by the serde skip_serializing_if attribute above.
    fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// Normalizes `supplemental-config-path = ""` to `None`: an empty path means
/// "disabled", not "read the empty path" (which would fail every network
/// reconciliation instead of turning the feature off).
fn empty_path_as_none<'de, D>(deserializer: D) -> Result<Option<PathBuf>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let path = Option::<PathBuf>::deserialize(deserializer)?;
    Ok(path.filter(|path| !path.as_os_str().is_empty()))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct TelemetryConfig {
    pub metrics_address: String,
    /// Serves `/metrics` and `/ready` on `metrics_address`. `/ready` is
    /// bound to this endpoint, so disabling it also removes readiness.
    #[serde(default = "default_prometheus_enabled")]
    pub prometheus_enabled: bool,
}

fn default_prometheus_enabled() -> bool {
    true
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        Self {
            metrics_address: TELEMETRY_METRICS_SERVICE_ADDRESS.to_string(),
            prometheus_enabled: default_prometheus_enabled(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct HBNConfig {
    /// Where to write the network config files
    pub root_dir: PathBuf,
    /// Do not run the config reload commands. Local dev only.
    pub skip_reload: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DpuNetworkingInterface {
    pub addresses: Vec<ipnetwork::IpNetwork>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FmdsDpuNetworkingConfig {
    pub config: DpuNetworkingInterface,
}

impl Default for FmdsDpuNetworkingConfig {
    fn default() -> Self {
        Self {
            config: DpuNetworkingInterface {
                addresses: vec!["169.254.169.254/30".to_string().parse().unwrap()],
            },
        }
    }
}

impl Default for HBNConfig {
    fn default() -> Self {
        Self {
            root_dir: PathBuf::from(HBN_DEFAULT_ROOT),
            skip_reload: false,
        }
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct UpdateConfig {
    /// Override normal upgrade command. For automated testing only.
    #[serde(default)]
    pub override_upgrade_cmd: Option<String>,
}

impl UpdateConfig {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct IterationTime {
    /// How often to report network health and poll for new configs when in stable state.
    /// Eventually we will need an event system. Block storage requires very fast DPU responses.
    pub main_loop_idle_secs: u64,

    /// How often to report network health and poll for new configs when things are in flux.
    /// This should be slightly bigger than bgpTimerHoldTimeMsecs as displayed in HBN
    /// container by 'show bgp neighbors json' - which is currently 9s.
    pub main_loop_active_secs: u64,

    /// How often we fetch the desired network configuration for a host
    pub network_config_fetch_secs: u64,

    /// How often to check if we have latest forge-dpu-agent version
    pub version_check_secs: u64,

    /// How often to update inventory
    #[serde(default = "default_inventory_update_secs")]
    pub inventory_update_secs: u64,

    /// How often to retry discover_machine registration
    /// calls in the event that retries are necessary.
    /// Default is every 60 seconds.
    #[serde(default = "default_discovery_retry_secs")]
    pub discovery_retry_secs: u64,

    /// How many times to retry discover_machine registration
    /// calls until giving up. Default is 10080, which,
    /// combine with the default discovery_retry_secs of 60,
    /// equals retrying for 1 week.
    #[serde(default = "default_discovery_retries_max")]
    pub discovery_retries_max: u32,
}

fn default_inventory_update_secs() -> u64 {
    3600u64
}

fn default_discovery_retry_secs() -> u64 {
    60u64
}

fn default_discovery_retries_max() -> u32 {
    10080u32
}

impl Default for IterationTime {
    fn default() -> Self {
        Self {
            main_loop_idle_secs: 30,
            main_loop_active_secs: 10,
            network_config_fetch_secs: 30,
            version_check_secs: 600, // 10 minutes
            inventory_update_secs: default_inventory_update_secs(),
            discovery_retry_secs: default_discovery_retry_secs(),
            discovery_retries_max: default_discovery_retries_max(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use carbide_test_support::Outcome::*;
    use carbide_test_support::{Case, check_cases, scenarios, value_scenarios};

    use super::*;

    const TEST_DATA_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/test");

    /// PEM file that ships in-tree and parses to at least one certificate.
    const VALID_PEM_PATH: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../dev/certs/forge_root.pem"
    );

    // Convenience: a `MachineIdentityConfig` with all numeric fields at defaults
    // and the two proxy options threaded through.
    fn mid(
        requests_per_second: u8,
        burst: u8,
        wait_timeout_secs: u8,
        sign_timeout_secs: u8,
        sign_proxy_url: Option<&str>,
        sign_proxy_tls_root_ca: Option<&str>,
    ) -> MachineIdentityConfig {
        MachineIdentityConfig {
            requests_per_second,
            burst,
            wait_timeout_secs,
            sign_timeout_secs,
            sign_proxy_url: sign_proxy_url.map(ToString::to_string),
            sign_proxy_tls_root_ca: sign_proxy_tls_root_ca.map(ToString::to_string),
        }
    }

    // `[network] supplemental-config-path` parsing — one row per input shape.
    // An empty string must behave exactly like an omitted key (disabled),
    // never as a readable path.
    #[test]
    fn network_config_supplemental_path_parsing() {
        for (scenario, toml_src, expected) in [
            ("section omitted", "", None),
            ("key omitted", "[network]\n", None),
            (
                "empty path is disabled",
                "[network]\nsupplemental-config-path = \"\"\n",
                None,
            ),
            (
                "non-empty path is kept",
                "[network]\nsupplemental-config-path = \"/etc/forge/supplemental-network-config/supplemental.json\"\n",
                Some(PathBuf::from(
                    "/etc/forge/supplemental-network-config/supplemental.json",
                )),
            ),
        ] {
            let config: AgentConfig =
                toml::from_str(toml_src).unwrap_or_else(|e| panic!("{scenario}: {e}"));
            assert_eq!(
                config.network.supplemental_config_path, expected,
                "{scenario}"
            );
        }
    }

    // `MachineIdentityConfig::validate` — one row per branch/boundary. The
    // operation returns `Result<(), String>`; rows whose exact message is part
    // of the contract use `FailsWith`, the rest assert only Ok-vs-Err.
    #[test]
    fn machine_identity_config_validate() {
        let d = MachineIdentityConfig::default();
        check_cases(
            [
                // ----- accepts -----
                Case {
                    scenario: "defaults are valid",
                    input: MachineIdentityConfig::default(),
                    expect: Yields(()),
                },
                Case {
                    scenario: "all-min boundary",
                    input: mid(1, 1, 1, 1, None, None),
                    expect: Yields(()),
                },
                Case {
                    scenario: "all-max boundary (sign-timeout low end)",
                    input: mid(20, 40, 10, 1, None, None),
                    expect: Yields(()),
                },
                Case {
                    scenario: "sign-timeout at max",
                    input: mid(20, 40, 10, 60, None, None),
                    expect: Yields(()),
                },
                Case {
                    scenario: "https sign-proxy-url accepted",
                    input: mid(3, 8, 2, 5, Some("https://idp.example.com/prefix"), None),
                    expect: Yields(()),
                },
                Case {
                    scenario: "http sign-proxy-url accepted",
                    input: mid(3, 8, 2, 5, Some("http://idp.example.com"), None),
                    expect: Yields(()),
                },
                Case {
                    scenario: "tls-root-ca with valid pem and url accepted",
                    input: mid(
                        3,
                        8,
                        2,
                        5,
                        Some("https://sign-proxy.example"),
                        Some(VALID_PEM_PATH),
                    ),
                    expect: Yields(()),
                },
                // ----- requests-per-second range -----
                Case {
                    scenario: "rps below min (0)",
                    input: mid(0, 8, 2, 5, None, None),
                    expect: Fails,
                },
                Case {
                    scenario: "rps above max (21)",
                    input: mid(21, 8, 2, 5, None, None),
                    expect: Fails,
                },
                // ----- burst range -----
                Case {
                    scenario: "burst below min (0)",
                    input: mid(3, 0, 2, 5, None, None),
                    expect: Fails,
                },
                Case {
                    scenario: "burst above max (41)",
                    input: mid(3, 41, 2, 5, None, None),
                    expect: Fails,
                },
                // ----- wait-timeout range -----
                Case {
                    scenario: "wait-timeout below min (0)",
                    input: mid(3, 8, 0, 5, None, None),
                    expect: Fails,
                },
                Case {
                    scenario: "wait-timeout above max (11)",
                    input: mid(3, 8, 11, 5, None, None),
                    expect: Fails,
                },
                // ----- sign-timeout range -----
                Case {
                    scenario: "sign-timeout below min (0)",
                    input: mid(3, 8, 2, 0, None, None),
                    expect: Fails,
                },
                Case {
                    scenario: "sign-timeout above max (61)",
                    input: mid(3, 8, 2, 61, None, None),
                    expect: Fails,
                },
                // ----- sign-proxy-url -----
                Case {
                    scenario: "sign-proxy-url whitespace-only rejected",
                    input: mid(3, 8, 2, 5, Some("   "), None),
                    expect: Fails,
                },
                Case {
                    scenario: "sign-proxy-url empty rejected",
                    input: mid(3, 8, 2, 5, Some(""), None),
                    expect: Fails,
                },
                Case {
                    scenario: "sign-proxy-url unsupported scheme rejected",
                    input: mid(3, 8, 2, 5, Some("ftp://x"), None),
                    expect: Fails,
                },
                Case {
                    scenario: "sign-proxy-url unparseable rejected",
                    input: mid(3, 8, 2, 5, Some("not a url"), None),
                    expect: Fails,
                },
                // ----- sign-proxy-tls-root-ca -----
                Case {
                    scenario: "tls-root-ca without url rejected (exact message mentions sign-proxy-url)",
                    input: mid(3, 8, 2, 5, None, Some("/etc/forge/sign_proxy_ca.pem")),
                    expect: FailsWith(
                        "machine-identity.sign-proxy-tls-root-ca: requires machine-identity.sign-proxy-url"
                            .to_string(),
                    ),
                },
                Case {
                    scenario: "tls-root-ca whitespace-only rejected",
                    input: mid(3, 8, 2, 5, Some("https://x.example"), Some("  \t  ")),
                    expect: Fails,
                },
                Case {
                    scenario: "tls-root-ca empty rejected",
                    input: mid(3, 8, 2, 5, Some("https://x.example"), Some("")),
                    expect: Fails,
                },
                Case {
                    scenario: "tls-root-ca path that does not exist rejected",
                    input: mid(
                        3,
                        8,
                        2,
                        5,
                        Some("https://x.example"),
                        Some("/nonexistent/path/to/ca.pem"),
                    ),
                    expect: Fails,
                },
            ],
            |c| c.validate(),
        );
        // `d` proves the default is reusable / unmodified by the table above.
        assert!(d.validate().is_ok());
    }

    // `validate` rejects a tls-root-ca file whose contents are not a certificate.
    // Kept out of the table because it needs a runtime-created temp file.
    #[test]
    fn machine_identity_config_validate_rejects_invalid_pem_contents() {
        use std::io::Write;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(b"not a certificate").unwrap();
        let c = mid(
            3,
            8,
            2,
            5,
            Some("https://x"),
            Some(&f.path().to_string_lossy()),
        );
        assert!(c.validate().is_err());
    }

    // `validate` rejects a tls-root-ca file that exists but is empty.
    #[test]
    fn machine_identity_config_validate_rejects_empty_pem_file() {
        let f = tempfile::NamedTempFile::new().unwrap();
        let c = mid(
            3,
            8,
            2,
            5,
            Some("https://x"),
            Some(&f.path().to_string_lossy()),
        );
        assert!(c.validate().is_err());
    }

    // `is_default` predicates (total ops).
    #[test]
    fn config_is_default_predicates() {
        value_scenarios!(
            run = |c| c.is_default();
            "machine-identity default is default" {
                MachineIdentityConfig::default() => true,
            }

            "machine-identity with changed rps is not default" {
                mid(7, 8, 2, 5, None, None) => false,
            }

            "machine-identity with proxy url is not default" {
                mid(3, 8, 2, 5, Some("https://x"), None) => false,
            }
        );

        value_scenarios!(
            run = |c| c.is_default();
            "update default is default" {
                UpdateConfig::default() => true,
            }

            "update with override cmd is not default" {
                UpdateConfig {
                    override_upgrade_cmd: Some("update".to_string()),
                } => false,
            }
        );
    }

    // TOML deserialization of `AgentConfig`: well-formed inputs parse, malformed
    // ones fail. The operation is `toml::from_str::<AgentConfig>` (fallible).
    #[test]
    fn agent_config_from_toml() {
        const FULL: &str = r#"[forge-system]
api-server = "https://127.0.0.1:1234"
root-ca = "/opt/forge/forge_root.pem"

[machine]
is-fake-dpu = true
interface-id = "91609f10-c91d-470d-a260-6293ea0c1200"

[metadata-service]
address = "0.0.0.0:7777"

[telemetry]
metrics-address = "0.0.0.0:8888"

[hbn]
root-dir = "/tmp/hbn-root"
skip-reload = true

[period]
main-loop-active-secs = 10
main-loop-idle-secs = 30
network-config-fetch-secs = 20
version-check-secs = 600
inventory-update-secs = 3600
discovery-retry-secs = 1
discovery-retries-max = 1000

[updates]
override-upgrade-cmd = "update"

[fmds-armos-networking.config]
addresses = ["168.254.169.254/30"]
"#;

        const MIN_NO_SERVICES: &str = "[forge-system]
api-server = \"https://127.0.0.1:1234\"
root-ca = \"/opt/forge/forge_root.pem\"

[machine]
interface-id = \"91609f10-c91d-470d-a260-6293ea0c1200\"
";

        const MID_SECTION: &str = r#"[forge-system]
api-server = "https://127.0.0.1:1234"
root-ca = "/opt/forge/forge_root.pem"

[machine]
interface-id = "91609f10-c91d-470d-a260-6293ea0c1200"

[machine-identity]
requests-per-second = 7
burst = 12
wait-timeout-secs = 4
sign-timeout-secs = 9
"#;

        scenarios!(
            run = |raw| toml::from_str::<AgentConfig>(raw).map(drop).map_err(drop);
            "full config parses" {
                FULL => Yields(()),
            }

            "minimal config without optional service sections parses" {
                MIN_NO_SERVICES => Yields(()),
            }

            "machine-identity section parses" {
                MID_SECTION => Yields(()),
            }

            "completely empty config uses defaults" {
                "" => Yields(()),
            }

            "unknown top-level key is ignored" {
                "totally-unknown-key = 5\n" => Yields(()),
            }

            "interface-id not a uuid fails" {
                "[machine]\ninterface-id = \"not-a-uuid\"\n" => Fails,
            }

            "machine-identity rps wrong type fails" {
                "[machine-identity]\nrequests-per-second = \"seven\"\n" => Fails,
            }

            "rps that overflows u8 fails" {
                "[machine-identity]\nrequests-per-second = 999\n" => Fails,
            }

            "syntactically invalid toml fails" {
                "this is not = = toml" => Fails,
            }

            "fmds addresses with bad cidr fails" {
                "[fmds-armos-networking.config]\naddresses = [\"not-a-cidr\"]\n" => Fails,
            }
        );
    }

    #[test]
    fn machine_identity_only_uses_agent_defaults() {
        let url = "http://dsx-imds.dpf-operator-system.svc.cluster.local:8080";
        let actual: AgentConfig =
            toml::from_str(&format!("[machine-identity]\nsign-proxy-url = {url:?}\n")).unwrap();
        let mut expected = AgentConfig::default();
        expected.machine_identity.sign_proxy_url = Some(url.to_string());

        assert_eq!(actual, expected);
    }

    // Field-level assertions on the FULL parse: each original `assert_eq!` becomes
    // a `Yields(true)` row over a derived boolean, so one parse covers them all.
    #[test]
    fn agent_config_full_fields() {
        const FULL: &str = r#"[forge-system]
api-server = "https://127.0.0.1:1234"
root-ca = "/opt/forge/forge_root.pem"

[machine]
is-fake-dpu = true
interface-id = "91609f10-c91d-470d-a260-6293ea0c1200"

[metadata-service]
address = "0.0.0.0:7777"

[telemetry]
metrics-address = "0.0.0.0:8888"

[hbn]
root-dir = "/tmp/hbn-root"
skip-reload = true

[period]
main-loop-active-secs = 10
main-loop-idle-secs = 30
network-config-fetch-secs = 20
version-check-secs = 600
inventory-update-secs = 3600
discovery-retry-secs = 1
discovery-retries-max = 1000

[updates]
override-upgrade-cmd = "update"

[fmds-armos-networking.config]
addresses = ["168.254.169.254/30"]
"#;
        let c: AgentConfig = toml::from_str(FULL).unwrap();
        let expected_id = uuid::uuid!("91609f10-c91d-470d-a260-6293ea0c1200");

        value_scenarios!(
            run = |b| b;
            "api-server" {
                c.forge_system.api_server == "https://127.0.0.1:1234" => true,
            }

            "interface-id" {
                c.machine.interface_id == Some(expected_id) => true,
            }

            "is-fake-dpu" {
                c.machine.is_fake_dpu => true,
            }

            "metadata-service address" {
                c.metadata_service.address == "0.0.0.0:7777" => true,
            }

            "telemetry metrics-address" {
                c.telemetry.metrics_address == "0.0.0.0:8888" => true,
            }

            "telemetry prometheus-enabled defaults to true when omitted" {
                c.telemetry.prometheus_enabled => true,
            }

            "hbn root-dir" {
                c.hbn.root_dir == Path::new("/tmp/hbn-root") => true,
            }

            "hbn skip-reload" {
                c.hbn.skip_reload => true,
            }

            "updates override-upgrade-cmd" {
                c.updates.override_upgrade_cmd == Some("update".to_string()) => true,
            }
        );
    }

    // The machine-identity section parse yields exactly the supplied values, and
    // the proxy options remain unset.
    #[test]
    fn agent_config_machine_identity_fields() {
        const MID_SECTION: &str = r#"[forge-system]
api-server = "https://127.0.0.1:1234"
root-ca = "/opt/forge/forge_root.pem"

[machine]
interface-id = "91609f10-c91d-470d-a260-6293ea0c1200"

[machine-identity]
requests-per-second = 7
burst = 12
wait-timeout-secs = 4
sign-timeout-secs = 9
"#;
        let c: AgentConfig = toml::from_str(MID_SECTION).unwrap();
        let m = &c.machine_identity;

        value_scenarios!(
            run = |b| b;
            "requests-per-second" {
                m.requests_per_second == 7 => true,
            }

            "burst" {
                m.burst == 12 => true,
            }

            "wait-timeout-secs" {
                m.wait_timeout_secs == 4 => true,
            }

            "sign-timeout-secs" {
                m.sign_timeout_secs == 9 => true,
            }

            "sign-proxy-url unset" {
                m.sign_proxy_url.is_none() => true,
            }

            "sign-proxy-tls-root-ca unset" {
                m.sign_proxy_tls_root_ca.is_none() => true,
            }
        );

        // The parsed section validates.
        c.machine_identity.validate().unwrap();
    }

    // The minimal (no optional service sections) parse defaults every omitted
    // section, and validates.
    #[test]
    fn agent_config_minimal_defaults() {
        const MIN_NO_SERVICES: &str = "[forge-system]
api-server = \"https://127.0.0.1:1234\"
root-ca = \"/opt/forge/forge_root.pem\"

[machine]
interface-id = \"91609f10-c91d-470d-a260-6293ea0c1200\"
";
        let c: AgentConfig = toml::from_str(MIN_NO_SERVICES).unwrap();
        let expected_id = uuid::uuid!("91609f10-c91d-470d-a260-6293ea0c1200");

        value_scenarios!(
            run = |b| b;
            "api-server preserved" {
                c.forge_system.api_server == "https://127.0.0.1:1234" => true,
            }

            "interface-id preserved" {
                c.machine.interface_id == Some(expected_id) => true,
            }

            "is-fake-dpu defaults false" {
                c.machine.is_fake_dpu => false,
            }

            "metadata-service defaults" {
                c.metadata_service == MetadataServiceConfig::default() => true,
            }

            "telemetry defaults" {
                c.telemetry == TelemetryConfig::default() => true,
            }

            "machine-identity defaults" {
                c.machine_identity == MachineIdentityConfig::default() => true,
            }

            "hbn root-dir defaults" {
                c.hbn.root_dir == Path::new(HBN_DEFAULT_ROOT) => true,
            }

            "hbn skip-reload defaults false" {
                c.hbn.skip_reload => false,
            }

            "updates override-upgrade-cmd defaults unset" {
                c.updates.override_upgrade_cmd.is_none() => true,
            }
        );
    }

    // Default constructors expose the documented default values.
    #[test]
    fn config_defaults() {
        value_scenarios!(
            run = |b| b;
            "forge-system api-server" {
                ForgeSystemConfig::default().api_server == DEFAULT_API_SERVER => true,
            }

            "metadata-service address" {
                MetadataServiceConfig::default().address
                == INSTANCE_METADATA_SERVICE_ADDRESS => true,
            }

            "telemetry metrics-address" {
                TelemetryConfig::default().metrics_address
                == TELEMETRY_METRICS_SERVICE_ADDRESS => true,
            }

            "hbn root-dir" {
                HBNConfig::default().root_dir == Path::new(HBN_DEFAULT_ROOT) => true,
            }

            "hbn skip-reload" {
                HBNConfig::default().skip_reload => false,
            }

            "machine-identity requests-per-second" {
                MachineIdentityConfig::default().requests_per_second
                == REQUESTS_PER_SECOND => true,
            }

            "machine-identity burst" {
                MachineIdentityConfig::default().burst == BURST => true,
            }

            "machine-identity wait-timeout-secs" {
                MachineIdentityConfig::default().wait_timeout_secs == WAIT_TIMEOUT_SECS => true,
            }

            "machine-identity sign-timeout-secs" {
                MachineIdentityConfig::default().sign_timeout_secs == SIGN_TIMEOUT_SECS => true,
            }

            "iteration-time inventory-update-secs" {
                IterationTime::default().inventory_update_secs == 3600 => true,
            }

            "iteration-time discovery-retry-secs" {
                IterationTime::default().discovery_retry_secs == 60 => true,
            }

            "iteration-time discovery-retries-max" {
                IterationTime::default().discovery_retries_max == 10080 => true,
            }
        );
    }

    // Load the barebones config and round-trip it back out; the dumped string
    // (with defaults filled in) must match the recorded expected output.
    #[test]
    fn test_load_forge_agent_config_defaults() {
        let input_config: AgentConfig = toml::from_str(
            fs::read_to_string(format!("{TEST_DATA_DIR}/min_agent_config/input.toml"))
                .unwrap()
                .as_str(),
        )
        .unwrap();
        let observed_output = toml::to_string(&input_config).unwrap();
        let expected_output =
            fs::read_to_string(format!("{TEST_DATA_DIR}/min_agent_config/output.toml")).unwrap();
        assert_eq!(observed_output, expected_output);
    }
}
