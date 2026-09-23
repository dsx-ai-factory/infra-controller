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
use std::fmt::Write;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::net::IpAddr;
use std::str::FromStr;
use std::time::Duration;

use carbide_uuid::machine::{DpuMachineId, MachineId};
use diff::Result as DiffResult;
use eyre::{OptionExt, WrapErr};
use forge_http_connector::resolver::{ForgeResolver, ForgeResolverOpts};
use hickory_resolver::config::ResolverConfig;
use hickory_resolver::proto::rr::Name;
use hyper::service::Service;
use resolv_conf::Config;
use rpc::forge::{
    InstancePhoneHomeLastContactRequest, ManagedHostNetworkConfigRequest, VersionRequest,
};
use rpc::forge_tls_client::ForgeClientT;
use rpc::{Instance, Timestamp, forge_resolver};

use crate::command_line::AgentPlatformType;
use crate::ethernet_virtualization::ServiceAddresses;

/// Host's `/etc/resolv.conf` is bind-mounted here in the init-container.
pub const HOST_RESOLV_CONF_PATH: &str = "/host-resolv.conf";

/// File the init-container writes nameservers to on the shared `/data` volume,
/// for the main container to read at startup.
pub const NAMESERVERS_FILE_PATH: &str = "/data/nameservers";

pub fn compare_lines(left: &str, right: &str, strip_behavior: Option<StripType>) -> CompareResult {
    let (left, right) = match strip_behavior {
        None => (left, right),
        Some(_) => unreachable!(),
    };
    let results = diff::lines(left, right);
    let identical = results
        .iter()
        .all(|r| matches!(r, diff::Result::Both(_, _)));
    match identical {
        true => CompareResult::Identical,
        false => {
            let mut report = String::new();
            results.into_iter().for_each(|r| {
                let (col1, linecontent) = match r {
                    DiffResult::Both(line, _) => (' ', line),
                    DiffResult::Left(line) => ('-', line),
                    DiffResult::Right(line) => ('+', line),
                };
                writeln!(&mut report, "{col1}{linecontent}").expect("can't write line to results?");
            });
            CompareResult::Different(report)
        }
    }
}

pub enum CompareResult {
    Identical,
    Different(String),
}

impl CompareResult {
    pub fn report(&self) -> &str {
        match self {
            CompareResult::Identical => "",
            CompareResult::Different(s) => s.as_str(),
        }
    }

    pub fn is_identical(&self) -> bool {
        matches!(self, CompareResult::Identical)
    }
}

pub enum StripType {}

pub struct UrlResolver {
    nameservers: Vec<IpAddr>,
    resolver: ForgeResolver,
}

impl UrlResolver {
    pub fn try_new() -> Result<Self, eyre::Report> {
        let config = Self::try_get_resolver_config()?;
        let nameservers = config
            .nameservers
            .iter()
            .map(|x| x.into())
            .collect::<Vec<IpAddr>>();

        let resolver = Self::try_get_resolver(config)?;
        Ok(Self {
            nameservers,
            resolver,
        })
    }
    pub fn nameservers(&self) -> Vec<IpAddr> {
        self.nameservers.clone()
    }

    fn try_get_resolver_config() -> Result<Config, eyre::Report> {
        let forge_resolv_config =
            forge_resolver::resolver::ForgeResolveConf::with_system_resolv_conf()?;
        let parsed_config = forge_resolv_config.parsed_configuration();
        Ok(parsed_config)
    }

    fn try_get_resolver(resolver_config: Config) -> Result<ForgeResolver, eyre::Report> {
        let forge_resolver_config =
            forge_resolver::resolver::into_forge_resolver_config(resolver_config)?;

        let hickory_resolver_config = ResolverConfig::from_parts(
            forge_resolver_config.0.domain,
            forge_resolver_config.0.search_domain,
            forge_resolver_config.0.inner,
        );

        let updated_opts = ForgeResolverOpts::new()
            .use_mgmt_vrf()
            .timeout(Duration::from_secs(5));
        let resolver_cfg =
            ForgeResolver::with_config_and_options(hickory_resolver_config, updated_opts);

        Ok(resolver_cfg)
    }

    /// Input name should be hostname, not url.
    /// valid: carbide-pxe.forge, nvidia.com, www.nvidia.com
    /// Invalid: https://www.nvidia.com/extra/uri
    pub async fn resolve(&mut self, name: &str) -> Result<Vec<IpAddr>, eyre::Report> {
        let ip = self
            .resolver
            .call(Name::from_str(name)?)
            .await?
            .map(|x| x.ip())
            .collect::<Vec<IpAddr>>();

        Ok(ip)
    }
}

impl ServiceAddresses {
    /// Resolve all service addresses (PXE, NTP, DNS) used by the agent.
    ///
    /// On `DpuOs` the local resolver is authoritative for nameservers. In
    /// containerized mode the agent's own `/etc/resolv.conf` points at the
    /// cluster DNS, so nameservers are read from a file the init-container
    /// populated from the *host's* `/etc/resolv.conf`.
    pub(super) async fn build(
        agent_platform_type: &AgentPlatformType,
        is_fake_dpu: bool,
    ) -> Result<Self, eyre::Report> {
        if is_fake_dpu {
            return Ok(Self::fake());
        }

        let mut url_resolver = UrlResolver::try_new()?;

        let pxe_ips = url_resolver
            .resolve("carbide-pxe.forge")
            .await
            .wrap_err("DNS resolver for carbide-pxe")?;
        // This log should be removed after some time.
        tracing::info!(pxe_ip_addresses = ?pxe_ips, "Pxe server resolved");

        let ntpservers = match url_resolver.resolve("carbide-ntp.forge").await {
            Ok(x) => {
                // This log should be removed after some time.
                tracing::info!(ntp_server_ip_addresses = ?x, "NTP servers resolved.");
                x
            }
            Err(e) => {
                tracing::error!(error = %e, "NTP servers couldn't be resolved. Dhcp-server won't send NTP server IPs in dhcpoffer/ack.");
                vec![]
            }
        };

        let nameservers = if agent_platform_type.is_dpu_os() {
            url_resolver.nameservers()
        } else {
            read_saved_nameservers()?
        };

        Ok(Self {
            pxe_ips,
            ntpservers,
            nameservers,
        })
    }

    fn fake() -> Self {
        Self {
            pxe_ips: vec![IpAddr::from([127, 0, 0, 1])],
            ntpservers: vec![],
            nameservers: vec![IpAddr::from([127, 0, 0, 1])],
        }
    }
}

/// Read the host's `/etc/resolv.conf` (mounted at [`HOST_RESOLV_CONF_PATH`])
/// and persist its nameservers, one IP per line, to [`NAMESERVERS_FILE_PATH`]
/// on the shared `/data` volume. Called from the init-container.
pub fn save_host_nameservers() -> Result<(), eyre::Report> {
    let config = forge_resolver::read_resolv_conf(HOST_RESOLV_CONF_PATH)
        .wrap_err_with(|| format!("failed to read {HOST_RESOLV_CONF_PATH}"))?;
    let mut text = String::new();
    for ns in &config.nameservers {
        let ip: IpAddr = ns.into();
        writeln!(&mut text, "{ip}").expect("writing to String never fails");
    }
    std::fs::write(NAMESERVERS_FILE_PATH, &text)
        .wrap_err_with(|| format!("failed to write {NAMESERVERS_FILE_PATH}"))?;
    Ok(())
}

/// Read the nameservers persisted by the init-container at
/// [`NAMESERVERS_FILE_PATH`]. Fails hard if the file is missing, unreadable,
/// contains an invalid IP, or is empty.
pub fn read_saved_nameservers() -> Result<Vec<IpAddr>, eyre::Report> {
    let content = std::fs::read_to_string(NAMESERVERS_FILE_PATH)
        .wrap_err_with(|| format!("failed to read {NAMESERVERS_FILE_PATH}"))?;
    let nameservers: Vec<IpAddr> = content
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| {
            line.parse::<IpAddr>()
                .wrap_err_with(|| format!("invalid IP in {NAMESERVERS_FILE_PATH}: {line:?}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    if nameservers.is_empty() {
        return Err(eyre::eyre!(
            "{NAMESERVERS_FILE_PATH} contained no nameservers"
        ));
    }
    Ok(nameservers)
}

// get_instance finds the instance associated with this dpu
pub async fn get_instance(
    client: &mut ForgeClientT,
    dpu_machine_id: &MachineId,
) -> Result<Option<Instance>, eyre::Error> {
    let request = tonic::Request::new(*dpu_machine_id);

    let instances = match client.find_instance_by_machine_id(request).await {
        Ok(response) => response.into_inner().instances,
        Err(err) => {
            return Err(eyre::eyre!(
                "error while executing the FindInstanceByMachineId gRPC call: {}",
                err.to_string()
            ));
        }
    };

    Ok(instances.first().cloned())
}

pub async fn get_sitename(client: &mut ForgeClientT) -> Result<Option<String>, eyre::Error> {
    let request = tonic::Request::new(VersionRequest {
        display_config: true,
    });

    let resp = match client.version(request).await {
        Ok(response) => response.into_inner(),
        Err(err) => {
            return Err(eyre::eyre!(
                "error while executing the version gRPC call: {}",
                err.to_string()
            ));
        }
    };

    let sn = match resp.runtime_config {
        Some(rc) => rc.sitename,
        None => return Ok(None),
    };

    Ok(sn)
}

// Use grpc call GetPeriodicDpuConfig and return the retrieved info
pub async fn get_periodic_dpu_config(
    client: &mut ForgeClientT,
    dpu_machine_id: &MachineId,
) -> Result<rpc::forge::ManagedHostNetworkConfigResponse, eyre::Error> {
    let request = tonic::Request::new(ManagedHostNetworkConfigRequest {
        dpu_machine_id: Some(DpuMachineId::try_from(*dpu_machine_id)?),
    });

    let resp = match client.get_managed_host_network_config(request).await {
        Ok(response) => response,
        Err(err) => {
            return Err(eyre::eyre!(
                "error while executing the GetManagedHostNetworkConfig gRPC call: {}",
                err.to_string()
            ));
        }
    };

    Ok(resp.into_inner())
}

// phone_home returns the timestamp returned from Carbide as a string
pub async fn phone_home(
    client: &mut ForgeClientT,
    dpu_machine_id: &MachineId,
) -> Result<Timestamp, eyre::Error> {
    let Some(instance) = get_instance(client, dpu_machine_id).await? else {
        return Err(eyre::eyre!(
            "no instance found with dpu_machine {}",
            dpu_machine_id
        ));
    };

    let request: tonic::Request<InstancePhoneHomeLastContactRequest> =
        tonic::Request::new(InstancePhoneHomeLastContactRequest {
            instance_id: instance.id,
        });

    let response = client
        .update_instance_phone_home_last_contact(request)
        .await?;

    response
        .into_inner()
        .timestamp
        .ok_or_else(|| eyre::eyre!("timestamp is empty in response"))
}

pub fn get_host_boot_timestamp() -> Result<u64, eyre::Error> {
    let proc_stat = File::open("/proc/stat")
        .map(BufReader::new)
        .wrap_err("couldn't open /proc/stat")?;
    let btime_value = proc_stat
        .lines()
        .find_map(|line| match line {
            // We're looking for a line like this:
            // `btime 123456789`
            Ok(line) => match line.split_once(' ') {
                Some(("btime", value)) => {
                    let value = String::from(value);
                    Some(Ok(value))
                }
                _ => None,
            },
            err => Some(err),
        })
        .transpose()
        .wrap_err("couldn't read /proc/stat")?
        .ok_or_eyre("couldn't find btime line in /proc/stat")?;
    btime_value
        .parse()
        .wrap_err("couldn't parse btime value as u64")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn test_get_host_boot_timestamp() {
        let boot_timestamp = get_host_boot_timestamp().expect("Couldn't get boot timestamp");
        dbg!(&boot_timestamp);
        // This was around July 14, 2017 -- hopefully your machine hasn't been
        // running without a reboot that long. :)
        assert!(boot_timestamp >= 1500000000);
    }
}
