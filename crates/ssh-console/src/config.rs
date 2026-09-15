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

use std::default::Default;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::Duration;

use carbide_uuid::machine::MachineIdParseError;
use duration_str::deserialize_duration;
use forge_tls::client_config::ClientCert;
use rpc::forge_api_client::ForgeApiClient;
use rpc::forge_tls_client::{ApiConfig, ForgeClientConfig};
use russh::keys::ssh_key::Fingerprint;
use serde::ser::SerializeSeq;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use size::Size;

use crate::bmc::vendor::{BmcVendor, SshBmcVendor};

#[derive(Debug, Serialize, Deserialize, PartialEq)]
/// Configuration for ssh-console. Fields are documented as comments in the output of [`Config::into_annotated_config_file`].
pub struct Config {
    #[serde(default = "Defaults::listen_address")]
    pub listen_address: SocketAddr,
    #[serde(default = "Defaults::metrics_address")]
    pub metrics_address: SocketAddr,
    #[serde(default = "Defaults::api_listen_address")]
    pub api_listen_address: SocketAddr,
    #[serde(default = "Defaults::api_allowed_client_spiffe_id")]
    pub api_allowed_client_spiffe_id: String,
    #[serde(
        rename = "carbide_url",
        default = "Defaults::carbide_uri",
        serialize_with = "serialize_uri",
        deserialize_with = "deserialize_uri"
    )]
    pub carbide_uri: http::Uri,
    #[serde(default)]
    pub authorized_keys_path: Option<PathBuf>,
    #[serde(default, rename = "bmcs")]
    pub override_bmcs: Option<Vec<BmcConfig>>,
    #[serde(default = "Defaults::host_key_path", rename = "host_key")]
    pub host_key_path: PathBuf,
    #[serde(default = "Defaults::dpus")]
    pub dpus: bool,
    #[serde(default = "Defaults::hosts")]
    pub hosts: bool,
    #[serde(default)]
    pub insecure: bool,
    #[serde(default)]
    pub override_bmc_ssh_port: Option<u16>,
    #[serde(default)]
    pub override_ipmi_port: Option<u16>,
    #[serde(default)]
    pub insecure_ipmi_ciphers: bool,
    #[serde(default)]
    pub force_deactivate_conflicting_ipmi_sol_sessions: bool,
    #[serde(default = "Defaults::root_ca_path")]
    pub forge_root_ca_path: PathBuf,
    #[serde(default = "Defaults::client_cert_path")]
    pub client_cert_path: PathBuf,
    #[serde(default = "Defaults::client_key_path")]
    pub client_key_path: PathBuf,
    #[serde(
        default,
        serialize_with = "serialize_openssh_certificate_ca_fingerprints",
        deserialize_with = "deserialize_openssh_certificate_ca_fingerprints"
    )]
    pub openssh_certificate_ca_fingerprints: Vec<Fingerprint>,
    #[serde(default)]
    pub admin_certificate_role: Option<String>,
    #[serde(
        default = "Defaults::api_poll_interval",
        serialize_with = "serialize_duration",
        deserialize_with = "deserialize_duration"
    )]
    pub api_poll_interval: Duration,
    #[serde(default = "Defaults::console_logs_path")]
    pub console_logs_path: PathBuf,
    #[serde(default = "Defaults::console_logging_enabled")]
    pub console_logging_enabled: bool,
    #[serde(default)]
    pub override_bmc_ssh_host: Option<String>,
    #[serde(
        default = "Defaults::reconnect_interval_base",
        serialize_with = "serialize_duration",
        deserialize_with = "deserialize_duration"
    )]
    pub reconnect_interval_base: Duration,
    #[serde(
        default = "Defaults::reconnect_interval_max",
        serialize_with = "serialize_duration",
        deserialize_with = "deserialize_duration"
    )]
    pub reconnect_interval_max: Duration,
    #[serde(
        default = "Defaults::successful_connection_minimum_duration",
        serialize_with = "serialize_duration",
        deserialize_with = "deserialize_duration"
    )]
    pub successful_connection_minimum_duration: Duration,
    #[serde(default = "Defaults::log_rotate_max_size")]
    pub log_rotate_max_size: Size,
    #[serde(default = "Defaults::log_rotate_max_rotated_files")]
    pub log_rotate_max_rotated_files: usize,
    #[serde(default = "Defaults::cert_authorization")]
    pub openssh_certificate_authorization: CertAuthorization,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct CertAuthorization {
    #[serde(default = "Defaults::cert_authorization_strategy")]
    pub strategy: Vec<CertAuthorizationStrategy>,
    #[serde(default)]
    pub keyid_format: KeyIdFormat,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum CertAuthorizationStrategy {
    // Extract key/values from KeyId, check if role matches
    KeyId,
    // Json,
    // ExternalCommand,
    // Extensions,
    // Principals,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct KeyIdFormat {
    #[serde(default = "Defaults::cert_authorization_keyid_field_separator")]
    pub field_separator: String,
    #[serde(default = "Defaults::cert_authorization_keyid_user_field")]
    pub user_field: String,
    #[serde(default = "Defaults::cert_authorization_keyid_role_field")]
    pub role_field: String,
    #[serde(default = "Defaults::cert_authorization_keyid_role_separator")]
    pub role_separator: String,
}

impl Default for KeyIdFormat {
    fn default() -> Self {
        Self {
            field_separator: Defaults::cert_authorization_keyid_field_separator(),
            user_field: Defaults::cert_authorization_keyid_user_field(),
            role_field: Defaults::cert_authorization_keyid_role_field(),
            role_separator: Defaults::cert_authorization_keyid_role_separator(),
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let cfg = std::fs::read_to_string(path).map_err(|error| ConfigError::CouldNotRead {
            path: path.to_string_lossy().to_string(),
            error,
        })?;
        toml::from_str::<Self>(&cfg).map_err(|error| ConfigError::InvalidToml {
            path: path.to_string_lossy().to_string(),
            error,
        })
    }

    pub async fn override_bmc_ssh_addr(
        &self,
        port: u16,
    ) -> Result<Option<SocketAddr>, ConfigError> {
        if let Some(override_bmc_ssh_host) = &self.override_bmc_ssh_host {
            let addr = tokio::net::lookup_host(format!("{override_bmc_ssh_host}:{port}"))
                .await
                .map_err(|error| ConfigError::HostLookup {
                    what: "override_bmc_ssh_host".to_string(),
                    host: override_bmc_ssh_host.to_string(),
                    error,
                })?
                .next()
                .ok_or_else(|| ConfigError::HostNotFound {
                    what: "override_bmc_ssh_host".to_string(),
                    host: override_bmc_ssh_host.to_string(),
                })?;
            Ok(Some(addr))
        } else {
            Ok(None)
        }
    }

    pub fn into_annotated_config_file(self) -> String {
        let Self {
            listen_address,
            metrics_address,
            api_listen_address,
            api_allowed_client_spiffe_id,
            authorized_keys_path: _,
            override_bmcs: _,
            host_key_path,
            dpus,
            hosts,
            insecure,
            carbide_uri,
            override_bmc_ssh_port: _,
            override_ipmi_port: _,
            insecure_ipmi_ciphers,
            force_deactivate_conflicting_ipmi_sol_sessions,
            forge_root_ca_path,
            client_cert_path,
            client_key_path,
            openssh_certificate_ca_fingerprints: _,
            admin_certificate_role: _,
            api_poll_interval,
            console_logs_path,
            console_logging_enabled,
            override_bmc_ssh_host: _,
            reconnect_interval_base,
            reconnect_interval_max,
            successful_connection_minimum_duration,
            log_rotate_max_size,
            log_rotate_max_rotated_files,
            openssh_certificate_authorization,
        } = self;
        let api_poll_interval = format!("{}s", api_poll_interval.as_secs());
        let reconnect_interval_base = format!("{}s", reconnect_interval_base.as_secs());
        let reconnect_interval_max = format!("{}s", reconnect_interval_max.as_secs());
        let successful_connection_minimum_duration =
            format!("{}s", successful_connection_minimum_duration.as_secs());
        let carbide_uri = carbide_uri.to_string();
        let listen_address = listen_address.to_string();
        let metrics_address = metrics_address.to_string();
        let api_listen_address = api_listen_address.to_string();
        let log_rotate_max_size = log_rotate_max_size
            .format()
            .with_base(size::Base::Base2)
            .with_style(size::Style::Abbreviated)
            .to_string();

        let cert_authorization_strategy = {
            let mut value = String::new();
            serde::Serialize::serialize(
                &openssh_certificate_authorization.strategy,
                toml::ser::ValueSerializer::new(&mut value),
            )
            .expect("Invalid default config");
            value
        };

        let cert_authorization_keyid_format_field_separator = openssh_certificate_authorization
            .keyid_format
            .field_separator;
        let cert_authorization_keyid_format_user_field =
            openssh_certificate_authorization.keyid_format.user_field;
        let cert_authorization_keyid_format_role_field =
            openssh_certificate_authorization.keyid_format.role_field;
        let cert_authorization_keyid_format_role_separator = openssh_certificate_authorization
            .keyid_format
            .role_separator;

        format!(
            r#"
#####
## This is a default config file for ssh-console. Everything in this file is optional: Any
## non-comment line in this file simply represents default values. Commented lines with a single `#`
## represent examples for optional configuration which is not part of the default config.
#####

## What address to listen on for SSH connections.
listen_address = {listen_address:?}

## Address to listen on for prometheus metrics requests (HTTP)
metrics_address = {metrics_address:?}

## Address for the private console-log gRPC API.
api_listen_address = {api_listen_address:?}

## The only SPIFFE identity permitted to call the private API.
api_allowed_client_spiffe_id = {api_allowed_client_spiffe_id:?}

## Address for carbide-api
carbide_url = {carbide_uri:?}

## Path to root CA cert for carbide-api
forge_root_ca_path = {forge_root_ca_path:?}

## Client cert path to communicate with carbide-api
client_cert_path = {client_cert_path:?}

## Client key path to communicate with carbide-api
client_key_path = {client_key_path:?}

## Path to the SSH host key path.
host_key = {host_key_path:?}

## Allow SSH'ing to DPU consoles
dpus = {dpus}

## Allow SSH'ing to host consoles (setting to false can be useful for dpu-only setups)
hosts = {hosts}

## Disable client SSH auth enforcement. This must NEVER be set in production: It forces all incoming
## client connections to succeed.
insecure = {insecure}

## Override ports to use when connecting to BMC's (useful for integration testing)
# override_bmc_ssh_port = <port>
# override_ipmi_port = <port>

## Signing CA fingerprints for openssh certificates. Defaults to not trusting any signing CA.
# openssh_certificate_ca_fingerprints = ["SHA256:<sha256>"]

## Roles which determine admin access (logins with an openssh certificate, signed by the above
## fingerprints, containing this role in its Key ID, are considered admins and can log into machines
## directly. Use the [openssh_certificate_authorization] section to configure how roles are
## extracted from certs.)
# admin_certificate_role = <group>

## If true, use insecure ciphers when connecting to IPMI, like SHA1. Useful for ipmi_sim.
insecure_ipmi_ciphers = {insecure_ipmi_ciphers}

## Force-deactivate a conflicting IPMI SOL session before reconnecting. The BMC cannot determine
## whether the existing session is stale or belongs to an active operator, so enabling this may
## disconnect someone using the console. Enable only when ssh-console has exclusive SOL ownership.
force_deactivate_conflicting_ipmi_sol_sessions = {force_deactivate_conflicting_ipmi_sol_sessions}

## Optional: For development mode, gives keys that are authorized to connect to ssh-console. Meant
## for integration tests. For interactive use, consider using openssh certificates instead.
# authorized_keys_path = <path>

## How often to poll the carbide API server for what machines are available
api_poll_interval = {api_poll_interval:?}

## Whether to output the console data for each machine to a log file
console_logging_enabled = {console_logging_enabled:?}

## Where to write console logs for each machine, if enabled
console_logs_path = {console_logs_path:?}

## If set, use this host to override all BMC backends. Useful for machine-a-tron mocks where we use
## a single SSH server to mock all BMC SSH connections.
# override_bmc_ssh_host = <hostname>

## How long to wait to reconnect after the first retry once a BMC disconnects. Upon disconnection, a
## reconnect will happen immediately, but if that fails, this interval will be used for the next
## reconnect. It will double every successive reconnect (with random jitter) up to a maximum of
## `reconnect_interval_max`.
reconnect_interval_base = {reconnect_interval_base:?}

## The maximum interval to wait to reconnect if a BMC is not responding to reconnects.
reconnect_interval_max = {reconnect_interval_max:?}

## How long should a connection to a BMC be up before it's considered a successful connection,
## and the exponential backoff timer is reset to zero. (This can be set to zero for integration
## tests where we intentionally test failure scenarios and want to quickly retry.)
successful_connection_minimum_duration = {successful_connection_minimum_duration:?}

## When rotating console logs, how big should we let it get before we rotate
log_rotate_max_size = {log_rotate_max_size:?}

## When rotating console logs, how many old logs should we keep? (e.g. 3 means we keep .log, .log.0, .log.1, and .log.2)
log_rotate_max_rotated_files = {log_rotate_max_rotated_files}

## Configure how the role is extracted from an SSH certificate
[openssh_certificate_authorization]
## How should roles be extracted from SSH certs? (Currently supported: "key_id")
strategy = {cert_authorization_strategy}

## Configure how to extract roles from the SSH certificate's Key ID field
[openssh_certificate_authorization.keyid_format]
field_separator = {cert_authorization_keyid_format_field_separator:?}
user_field = {cert_authorization_keyid_format_user_field:?}
role_field = {cert_authorization_keyid_format_role_field:?}
role_separator = {cert_authorization_keyid_format_role_separator:?}

## Optional: For development mode, you can hardcode a list of BMC's to talk to.
# [[bmcs]]
# # machine_id: the machine ID this BMC overrides
# machine_id = "fm100hteau2jdt69qg575qld4lj05me09u2qp7ei38uv7volvprkck9enkg"
# # instance_id: Optional, the instance_id to declare for this machine
# instance_id = "2020eb71-7674-4a15-a05b-c7d73da747b4"
# ip = "127.0.0.1"
# port = 22
# user = "user"                     # User to authenticate as when ssh-console connects to BMC
# password = "password"             # Password to use when ssh-console connects to BMC
# ssh_key_path = "/path/to/ssh_key" # Path to an SSH key to use when ssh-console connects to BMC (optional, overrides password.)
# bmc_vendor = "dell"               # Vendor for this BMC, determines connection behavior (currently supported: "dell", "lenovo", "hpe", "supermicro", "lenovo_ami", "dpu", "nvidia_viking")
#
# # [[bmcs]]
# # ... more bmcs sections can define more than one
"#
        )
    }

    pub fn make_forge_api_client(&self) -> ForgeApiClient {
        let carbide_uri_string = self.carbide_uri.to_string();
        tracing::info!(
            carbide_uri = carbide_uri_string.as_str(),
            "Configured Carbide API URI"
        );

        // TODO: The API's for ClientCert/ForgeClientConfig/etc really ought to take PathBufs, not Strings.
        let client_cert = ClientCert {
            cert_path: self
                .client_cert_path
                .to_str()
                .expect("Invalid utf-8 in client_cert_path")
                .to_string(),
            key_path: self
                .client_key_path
                .to_str()
                .expect("Invalid utf-8 in client_key_path")
                .to_string(),
        };
        let client_config = ForgeClientConfig::new(
            self.forge_root_ca_path
                .to_str()
                .expect("Invalid utf-8 in forge_root_ca_path")
                .to_string(),
            Some(client_cert),
        );

        let api_config = ApiConfig::new(&carbide_uri_string, &client_config);
        ForgeApiClient::new(&api_config)
    }
}

#[derive(Deserialize, Serialize, PartialEq, Debug, Clone)]
pub struct BmcConfig {
    pub machine_id: String,
    pub instance_id: Option<String>,
    pub ip: IpAddr,
    pub port: Option<u16>,
    pub user: String,
    pub password: String,
    pub ssh_key_path: Option<PathBuf>,
    pub bmc_vendor: BmcVendor,
}

impl BmcConfig {
    pub fn addr(&self) -> SocketAddr {
        let port = if let Some(port) = self.port {
            port
        } else {
            match self.bmc_vendor {
                // DPUs use port 2200 for a console-only SSH session
                BmcVendor::Ssh(SshBmcVendor::Dpu) => 2200,
                BmcVendor::Ssh(_) => 22,
                BmcVendor::Ipmi(_) => 623,
            }
        };
        SocketAddr::new(self.ip, port)
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            listen_address: Defaults::listen_address(),
            metrics_address: Defaults::metrics_address(),
            api_listen_address: Defaults::api_listen_address(),
            api_allowed_client_spiffe_id: Defaults::api_allowed_client_spiffe_id(),
            host_key_path: Defaults::host_key_path(),
            carbide_uri: Defaults::carbide_uri(),
            forge_root_ca_path: Defaults::root_ca_path(),
            client_cert_path: Defaults::client_cert_path(),
            client_key_path: Defaults::client_key_path(),
            api_poll_interval: Defaults::api_poll_interval(),
            console_logs_path: Defaults::console_logs_path(),
            console_logging_enabled: Defaults::console_logging_enabled(),
            successful_connection_minimum_duration:
                Defaults::successful_connection_minimum_duration(),
            log_rotate_max_size: Defaults::log_rotate_max_size(),
            log_rotate_max_rotated_files: Defaults::log_rotate_max_rotated_files(),
            reconnect_interval_base: Defaults::reconnect_interval_base(),
            reconnect_interval_max: Defaults::reconnect_interval_max(),
            dpus: Defaults::dpus(),
            hosts: Defaults::hosts(),
            openssh_certificate_authorization: Defaults::cert_authorization(),
            override_bmc_ssh_port: None,
            override_ipmi_port: None,
            authorized_keys_path: None,
            override_bmcs: None,
            insecure: false,
            insecure_ipmi_ciphers: false,
            force_deactivate_conflicting_ipmi_sol_sessions: false,
            override_bmc_ssh_host: None,
            admin_certificate_role: None,
            openssh_certificate_ca_fingerprints: vec![],
        }
    }
}

pub struct Defaults;

#[derive(thiserror::Error, Debug)]
pub enum ConfigError {
    #[error("could not read config file at {path}: {error}")]
    CouldNotRead { path: String, error: std::io::Error },
    #[error("TOML error reading config file at {path}: {error}")]
    InvalidToml {
        path: String,
        error: toml::de::Error,
    },
    #[error("error looking up {what} {host}: {error}")]
    HostLookup {
        what: String,
        host: String,
        error: std::io::Error,
    },
    #[error("{what} {host} did not resolve to any addresses")]
    HostNotFound { what: String, host: String },
    #[error("invalid machine_id in BMC override config: {0}")]
    InvalidBmcOverrideMachineId(MachineIdParseError),
}

impl Defaults {
    pub fn listen_address() -> SocketAddr {
        "[::]:22"
            .parse()
            .expect("BUG: default listen_address is invalid")
    }

    pub fn metrics_address() -> SocketAddr {
        "[::]:8080"
            .parse()
            .expect("BUG: default listen_address is invalid")
    }

    pub fn api_listen_address() -> SocketAddr {
        "[::]:1079"
            .parse()
            .expect("BUG: default api_listen_address is invalid")
    }

    pub fn api_allowed_client_spiffe_id() -> String {
        "spiffe://nico.local/nico-system/sa/nico-api".to_string()
    }

    pub fn host_key_path() -> PathBuf {
        "/etc/ssh/ssh_host_ed25519_key".into()
    }

    pub fn dpus() -> bool {
        true
    }

    pub fn hosts() -> bool {
        true
    }

    pub fn carbide_uri() -> http::Uri {
        "https://carbide-api.forge-system.svc.cluster.local:1079"
            .try_into()
            .expect("BUG: default carbide URI is invalid")
    }

    pub fn root_ca_path() -> PathBuf {
        "/var/run/secrets/spiffe.io/ca.crt".into()
    }

    pub fn client_cert_path() -> PathBuf {
        "/var/run/secrets/spiffe.io/tls.crt".into()
    }

    pub fn client_key_path() -> PathBuf {
        "/var/run/secrets/spiffe.io/tls.key".into()
    }

    pub fn api_poll_interval() -> Duration {
        Duration::from_secs(180)
    }

    pub fn console_logs_path() -> PathBuf {
        "/var/log/consoles".into()
    }

    pub fn console_logging_enabled() -> bool {
        true
    }

    pub fn reconnect_interval_base() -> Duration {
        Duration::from_secs(10)
    }

    pub fn reconnect_interval_max() -> Duration {
        Duration::from_secs(600)
    }

    pub fn successful_connection_minimum_duration() -> Duration {
        Duration::from_secs(60)
    }

    pub fn log_rotate_max_size() -> Size {
        Size::from_mebibytes(10)
    }

    pub fn log_rotate_max_rotated_files() -> usize {
        4
    }

    pub fn cert_authorization() -> CertAuthorization {
        CertAuthorization {
            strategy: vec![CertAuthorizationStrategy::KeyId],
            keyid_format: KeyIdFormat {
                field_separator: " ".to_string(),
                user_field: "user".to_string(),
                role_field: "roles".to_string(),
                role_separator: ",".to_string(),
            },
        }
    }

    pub fn cert_authorization_keyid_field_separator() -> String {
        " ".to_string()
    }

    pub fn cert_authorization_keyid_user_field() -> String {
        "user".to_string()
    }

    pub fn cert_authorization_keyid_role_field() -> String {
        "roles".to_string()
    }

    pub fn cert_authorization_keyid_role_separator() -> String {
        ",".to_string()
    }

    pub fn cert_authorization_strategy() -> Vec<CertAuthorizationStrategy> {
        vec![CertAuthorizationStrategy::KeyId]
    }
}

fn serialize_duration<S>(d: &std::time::Duration, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    serializer.serialize_str(&format!("{}s", d.as_secs()))
}

fn serialize_openssh_certificate_ca_fingerprints<S>(
    d: &Vec<Fingerprint>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    let mut seq = serializer.serialize_seq(Some(d.len()))?;
    for fingerprint in d {
        seq.serialize_element(&fingerprint.to_string())?;
    }
    seq.end()
}

fn deserialize_openssh_certificate_ca_fingerprints<'de, D>(
    deserializer: D,
) -> Result<Vec<Fingerprint>, D::Error>
where
    D: Deserializer<'de>,
{
    Vec::<String>::deserialize(deserializer)?
        .into_iter()
        .map(|s| Fingerprint::from_str(&s))
        .collect::<Result<Vec<_>, _>>()
        .map_err(serde::de::Error::custom)
}

fn serialize_uri<S>(u: &http::Uri, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    serializer.serialize_str(&format!("{u}"))
}

fn deserialize_uri<'de, D>(deserializer: D) -> Result<http::Uri, D::Error>
where
    D: Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;
    http::Uri::from_str(&s).map_err(serde::de::Error::custom)
}

#[cfg(test)]
mod tests {
    use indoc::indoc;

    use super::*;

    #[test]
    fn test_default_file_is_actually_default() {
        let default_toml: Config = toml::from_str(&Config::default().into_annotated_config_file())
            .expect("default toml didn't parse");
        let default_file = Config::default();
        assert_eq!(default_toml, default_file);
    }

    #[test]
    fn test_empty_config_file_is_default() {
        let empty_config: Config = toml::from_str("").expect("empty toml didn't parse");
        let default_config = Config::default();
        assert_eq!(empty_config, default_config);
        assert!(!empty_config.force_deactivate_conflicting_ipmi_sol_sessions);
    }

    #[test]
    fn test_conflicting_ipmi_sol_deactivation_requires_explicit_opt_in() {
        let config: Config =
            toml::from_str("force_deactivate_conflicting_ipmi_sol_sessions = true")
                .expect("explicit opt-in config didn't parse");

        assert!(config.force_deactivate_conflicting_ipmi_sol_sessions);
    }

    #[test]
    fn test_default_file_parses() {
        let default = Config::default();
        let default_toml = toml::to_string(&default).expect("default toml didn't serialize");
        let roundtripped =
            toml::from_str::<Config>(&default_toml).expect("default toml didn't parse");
        assert_eq!(default, roundtripped);
    }

    #[test]
    fn test_authz_partial_config() {
        let partial_config = indoc! {r#"
        [openssh_certificate_authorization]
        strategy = ["key_id"]

        [openssh_certificate_authorization.keyid_format]
        # don't configure field_separator or role_field, serde should pick defaults
        role_field = "roles"
        role_separator = ","
        "#};

        let partial_config =
            toml::from_str::<Config>(partial_config).expect("Couldn't parse config toml");

        // The unspecified keyid_format fields should be at defaults
        assert_eq!(
            partial_config.openssh_certificate_authorization.strategy,
            vec![CertAuthorizationStrategy::KeyId],
        );
        assert_eq!(
            partial_config
                .openssh_certificate_authorization
                .keyid_format
                .field_separator,
            " ".to_string()
        );
        assert_eq!(
            partial_config
                .openssh_certificate_authorization
                .keyid_format
                .user_field,
            "user".to_string()
        );

        // Should be equivalent to the default config
        assert_eq!(partial_config, Config::default());
    }

    #[test]
    fn test_authz_partial_config_no_strategy() {
        let partial_config = indoc! {r#"
        [openssh_certificate_authorization.keyid_format]
        # don't configure field_separator or role_field, serde should pick defaults
        role_field = "roles"
        role_separator = ","
        "#};

        let partial_config =
            toml::from_str::<Config>(partial_config).expect("Couldn't parse config toml");

        // The strategy should be at the default
        assert_eq!(
            partial_config.openssh_certificate_authorization.strategy,
            vec![CertAuthorizationStrategy::KeyId],
        );
        // The unspecified keyid_format fields should be at defaults
        assert_eq!(
            partial_config
                .openssh_certificate_authorization
                .keyid_format
                .field_separator,
            " ".to_string()
        );
        assert_eq!(
            partial_config
                .openssh_certificate_authorization
                .keyid_format
                .user_field,
            "user".to_string()
        );

        // Should be equivalent to the default config
        assert_eq!(partial_config, Config::default());
    }
}
