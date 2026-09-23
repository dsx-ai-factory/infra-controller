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
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::path::PathBuf;
use std::str::FromStr;

use carbide_network::virtualization::VpcVirtualizationType;
use carbide_uuid::machine::DpuMachineId;
use clap::{Parser, ValueEnum};
use url::Url;

use crate::network_monitor::NetworkPingerType;

#[derive(Parser)]
#[clap(name = "forge-dpu-agent")]
pub struct Options {
    #[clap(long, default_value = "false", help = "Print version number and exit")]
    pub version: bool,

    /// The path to the forge agent configuration file development overrides.
    /// This file will hold data in the `AgentConfig` format.
    #[clap(long)]
    pub config_path: Option<PathBuf>,

    #[clap(subcommand)]
    pub cmd: Option<AgentCommand>,
}

#[derive(Parser, Debug)]
pub enum AgentCommand {
    #[clap(
        about = "Run is the normal command. Runs main loop forever, configures networking, etc."
    )]
    Run(Box<RunOptions>),

    #[clap(about = "Detect hardware and exit")]
    Hardware(HardwareOptions),

    #[clap(
        about = "Init-container entry point: provision the root CA cert and snapshot hardware to the shared volume for the main container."
    )]
    InitContainer(InitContainerOptions),

    #[clap(about = "One-off health check")]
    Health,

    #[clap(about = "Print LLDP neighbors visible on this host and exit")]
    LldpNeighbors(LldpNeighborsOptions),

    #[clap(about = "Continuously publish host LLDP snapshots for a containerized agent")]
    SidecarMode,

    #[clap(about = "One-off network monitor")]
    Network(NetworkOptions),

    #[clap(about = "Do a duppet run for duppet-managed files")]
    Duppet(DuppetOptions),

    #[clap(about = "Write a templated config file", subcommand)]
    Write(WriteTarget),
}

const DEFAULT_BOOTSTRAP_CA_URL: &str = "http://carbide-pxe.forge/api/v0/tls/root_ca";

fn parse_bootstrap_ca_url(value: &str) -> Result<Url, String> {
    let url = Url::parse(value).map_err(|error| format!("invalid URL: {error}"))?;
    match url.scheme() {
        "http" | "https" => Ok(url),
        scheme => Err(format!(
            "unsupported bootstrap CA URL scheme {scheme:?}; expected http or https"
        )),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
#[value(rename_all = "snake_case")]
pub enum InitContainerBootstrapCaSource {
    LegacyDownload,
    Mounted,
}

#[derive(Parser, Debug)]
pub struct InitContainerOptions {
    /// Selects where the bootstrap CA is obtained. The default preserves the
    /// legacy unauthenticated PXE download.
    #[clap(long, default_value = "legacy_download")]
    pub bootstrap_ca_source: InitContainerBootstrapCaSource,

    /// URL used only when --bootstrap-ca-source=legacy_download.
    #[clap(
        long,
        default_value = DEFAULT_BOOTSTRAP_CA_URL,
        value_parser = parse_bootstrap_ca_url
    )]
    pub bootstrap_ca_url: Url,
}

#[derive(Parser, Debug)]
pub enum WriteTarget {
    #[clap(about = "Write frr.conf")]
    Frr(FrrOptions),
    #[clap(about = "Write /etc/network/interfaces")]
    Interfaces(InterfacesOptions),
    #[clap(about = "Write /etc/supervisor/conf.d/default-isc-dhcp-relay.conf")]
    Dhcp(DhcpOptions),
    #[clap(about = "Write NVUE startup.yaml")]
    Nvue(Box<NvueOptions>),
}

#[derive(Parser, Debug)]
pub struct NvueOptions {
    #[clap(long, help = "Full path of NVUE's startup.yaml")]
    pub path: String,

    #[clap(long, help = "Forge Native Networking mode")]
    pub is_fnn: bool,

    #[clap(
        long,
        help = "A single VNI to use for all VPCs.  This is a special case to handle environments where upstream switches are unable to handle traffic for route import for multiple VNIs.  Route targets will still be derived from the dynamically allocated VNI of the VPC."
    )]
    pub site_global_vpc_vni: Option<u32>,

    #[clap(long)]
    pub loopback_ip: IpAddr,

    #[clap(long)]
    pub loopback_ip_v6: Option<Ipv6Addr>,

    #[clap(long)]
    pub asn: u32,

    #[clap(long)]
    pub datacenter_asn: u32,

    #[clap(long)]
    pub common_internal_route_target: Option<String>,

    #[clap(
        long,
        help = "Full JSON representation of a RouteConfig (see nvue.rs) to be used as additional route targets to import in FNN. Repeats with multiple --additional_fnn_route_target_import."
    )]
    pub additional_fnn_route_target_import: Vec<String>,

    #[clap(long)]
    pub dpu_hostname: String,

    #[clap(long, use_value_delimiter = true, help = "Comma separated")]
    pub uplinks: Vec<String>,

    #[clap(long, use_value_delimiter = true, help = "Comma separated")]
    pub route_servers: Vec<IpAddr>,

    #[clap(long, use_value_delimiter = true, help = "Comma separated")]
    pub dhcp_servers: Vec<IpAddr>,

    #[clap(
        long,
        help = "Format is l3vni,vrf_loopback,services_svi, e.g. --l3_domain 4096,10.0.0.1,svi . Repeats."
    )]
    pub l3_domain: Vec<String>,

    #[clap(long, help = "Format is 'id,host_route', e.g. --vlan 1,xyz. Repeats.")]
    pub vlan: Vec<String>,

    #[clap(long, help = "Compute Tenant [VRF] name")]
    pub ct_vrf_name: String,

    #[clap(long, help = "The VPC-specific L3VNI.")]
    pub ct_l3vni: Option<u32>,

    #[clap(long)]
    pub ct_vrf_loopback: String,

    #[clap(
        long,
        help = "Full JSON representation of a PortConfig (see nvue.rs). Repeats with multiple --ct-port-config."
    )]
    pub ct_port_config: Vec<String>,

    #[clap(long, use_value_delimiter = true, help = "Comma separated")]
    pub ct_external_access: Vec<String>,

    #[clap(long, help = "What version of hbn in format: 1.5.0-doca2.2.0")]
    pub hbn_version: Option<String>,

    #[clap(
        long,
        help = "Site-wide GNI-supplied VNI to use for VPCs to access the Internet."
    )]
    pub ct_internet_l3_vni: Option<u32>,

    #[clap(
        long,
        help = "The VpcVirtualizationType to use for this config + template (etv, etv_nvue, fnn_classic, fnn_l3)"
    )]
    pub virtualization_type: VpcVirtualizationType,

    #[clap(
        long,
        help = "Whether stateful ACLs are allowed and the DPU should adjust config to handle them.",
        default_value_t = false
    )]
    pub stateful_acls_enabled: bool,

    #[clap(
        long,
        help = "Full JSON representation of a NetworkSecurityGroupRule (see nvue.rs) that will be evaluated before any tenant-defined rules. Repeats with multiple --network_security_policy_override_rule."
    )]
    pub network_security_policy_override_rule: Vec<String>,

    #[clap(
        long,
        help = "Full JSON representation of a NetworkSecurityGroup (see nvue.rs). Repeats with multiple --network_security_group"
    )]
    pub network_security_group: Vec<String>,

    #[clap(
        long,
        help = "Full JSON representation of a RoutingProfile (see nvue.rs)."
    )]
    pub ct_routing_profile: Option<String>,

    #[clap(
        long,
        help = "BGP password to use with session between the DPU and the TOR"
    )]
    pub bgp_leaf_session_password: Option<String>,
}

#[derive(Parser, Debug)]
pub struct FrrOptions {
    #[clap(long, help = "Full path of frr.conf")]
    pub path: String,
    #[clap(long)]
    pub asn: u32,
    #[clap(long)]
    pub loopback_ip: IpAddr,
    #[clap(long, help = "Format is 'id,host_route', e.g. --vlan 1,xyz. Repeats.")]
    pub vlan: Vec<String>,
    #[clap(long, default_value = "etv")]
    pub network_virtualization_type: VpcVirtualizationType,
    #[clap(long, default_value = "0")]
    pub vpc_vni: u32,
    #[clap(long, use_value_delimiter = true)]
    pub route_servers: Vec<String>,
    #[clap(
        long,
        help = "Use admin interface, which removes tenant BGP config (Feature: Bring Your Own IP) from frr.conf"
    )]
    pub admin: bool,
}

#[derive(Parser, Debug)]
pub struct InterfacesOptions {
    #[clap(long, help = "Full path of interfaces file")]
    pub path: String,
    #[clap(long)]
    pub loopback_ip: IpAddr,
    #[clap(long, help = "Blank for admin network, vxlan48 for tenant networks")]
    pub vni_device: String,
    #[clap(
        long,
        help = "Format is JSON see PortConfig in interfaces.rs. Repeats."
    )]
    pub network: Vec<String>,
    #[clap(long, default_value = "etv")]
    pub network_virtualization_type: VpcVirtualizationType,
}

#[derive(Parser, Debug)]
pub struct DhcpOptions {
    #[clap(long, help = "Full path of dhcp relay config file")]
    pub path: String,
    #[clap(long, help = "vlan numeric id. Repeats")]
    pub vlan: Vec<u32>,
    // Note that these will be staying IPv4 only for now. This
    // config block is pretty tailored towards DHCPv4, and may
    // get refactored a bit as part of adding DHCPv6 support.
    #[clap(long, help = "DHCP server IP address. Repeats")]
    pub dhcp: Vec<Ipv4Addr>,
    #[clap(long, help = "Remote ID to be filled in Option 82 - Agent Remote ID")]
    pub remote_id: String,
    #[clap(long, default_value = "etv")]
    pub network_virtualization_type: VpcVirtualizationType,
}

#[derive(Parser, Debug)]
pub struct RunOptions {
    #[clap(long, help = "Enable metadata service")]
    pub enable_metadata_service: bool,
    #[clap(
        long,
        help = "Use this machine id instead of building it from hardware enumeration. Development/testing only"
    )]
    pub override_machine_id: Option<DpuMachineId>,
    #[clap(
        long,
        help = "Use this network_virtualization_type for both service network and all instances."
    )]
    pub override_network_virtualization_type: Option<VpcVirtualizationType>,
    #[clap(
        long,
        default_value = "false",
        help = "Do not perform upgrade checks. This is for development only. Do not use in production."
    )]
    pub skip_upgrade_check: bool,
    #[clap(
        long,
        help = "gRPC address of the DHCP server control service (e.g. http://localhost:50051). \
                When set, the agent sends config updates via gRPC instead of writing files directly."
    )]
    pub dhcp_grpc_server: Option<String>,
    #[clap(
        long,
        help = "gRPC address of the external FMDS service (e.g. http://localhost:50052). \
                When set, the agent sends config updates via gRPC instead of running embedded FMDS."
    )]
    pub fmds_grpc_server: Option<String>,
    #[clap(
        long,
        default_value = "3",
        value_parser = clap::value_parser!(u64).range(1..),
        help = "Seconds to wait for one connection attempt to --fmds-grpc-server. The agent \
                reconnects on every main-loop iteration, so this bounds how long an unreachable \
                FMDS can hold the loop up. Ignored without --fmds-grpc-server."
    )]
    pub fmds_connect_timeout_secs: u64,
    #[clap(
        long,
        default_value = "container-exec",
        help = "Set the configuration mode for HBN. Specify \"container-exec\" or \"nvue-rest\".",
        env = "HBN_CONFIG_MODE"
    )]
    pub hbn_config_mode: HbnConfigMode,
    #[clap(
        long,
        default_value = "dpu-os",
        help = "Set the platform type. Specify \"dpu-os\" or \"containerized\".",
        env = "AGENT_PLATFORM_TYPE"
    )]
    pub agent_platform_type: AgentPlatformType,
    #[clap(
        long,
        help = "Prepend this string to interface names before sending them to the DHCP server",
        env = "DHCP_SERVER_INTERFACE_PREPEND"
    )]
    pub dhcp_server_interface_prepend: Option<String>,
}

#[derive(Parser, Debug, Clone)]
pub enum HbnConfigMode {
    // ContainerExec: The old default, where we use crictl to exec into the HBN container.
    ContainerExec,
    // NvueRest: We use the NVUE REST API to manage HBN configuration.
    NvueRest,
}

impl HbnConfigMode {
    pub fn is_container_exec(&self) -> bool {
        matches!(self, Self::ContainerExec)
    }
}

impl FromStr for HbnConfigMode {
    type Err = eyre::Report;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        use HbnConfigMode::*;
        match s {
            "container-exec" => Ok(ContainerExec),
            "nvue-rest" => Ok(NvueRest),
            unknown_mode => Err(eyre::eyre!("unknown HBN config mode \"{unknown_mode}\"")),
        }
    }
}

#[derive(Parser, Debug, Clone)]
pub enum AgentPlatformType {
    // DpuOs: The old default, where we're running inside the main DPU OS and
    // are free to poke any files and containers directly through whatever
    // method we feel like.
    DpuOs,
    // Containerized: Something suitable for DPF, where the agent is running
    // inside a container with no direct access to the OS resources or any of
    // the other containers.
    Containerized,
    // should "fake DPU" be modeled as a variant here?
}

impl AgentPlatformType {
    pub fn is_dpu_os(&self) -> bool {
        matches!(self, AgentPlatformType::DpuOs)
    }
}

impl FromStr for AgentPlatformType {
    type Err = eyre::Report;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        use AgentPlatformType::*;
        match s {
            "dpu-os" => Ok(DpuOs),
            "containerized" => Ok(Containerized),
            unknown_type => Err(eyre::eyre!("unknown platform type \"{unknown_type}\"")),
        }
    }
}

#[derive(Parser, Debug)]
pub struct HardwareOptions {
    #[clap(
        long,
        help = "Write the hardware output (a JSON-serialized rpc::DiscoveryInfo message) to the specified file"
    )]
    pub output_file: Option<PathBuf>,
    #[clap(
        long,
        default_value = "dpu-os",
        help = "Set the platform type. Specify \"dpu-os\" or \"containerized\".",
        env = "AGENT_PLATFORM_TYPE"
    )]
    pub agent_platform_type: AgentPlatformType,
}

#[derive(Parser, Debug)]
pub struct LldpNeighborsOptions {
    #[clap(
        long,
        default_value = "dpu-os",
        help = "Set the platform type. Specify \"dpu-os\" or \"containerized\".",
        env = "AGENT_PLATFORM_TYPE"
    )]
    pub agent_platform_type: AgentPlatformType,
}

#[derive(Parser, Debug)]
pub struct NetworkOptions {
    #[clap(
        long,
        help = "Use this network_pinger_type for the interface used for pinging."
    )]
    pub network_pinger_type: Option<NetworkPingerType>,
}

#[derive(Parser, Debug)]
pub struct DuppetOptions {
    #[arg(
        long,
        help = "Do everything, including logging, but don't actually create/update files or permissions."
    )]
    pub dry_run: bool,

    #[arg(
        long,
        help = "Don't log anything, but still dump out a report summary at the end."
    )]
    pub quiet: bool,

    #[arg(
        long = "no-color",
        help = "Don't show pretty colors with log messages, if that's how you feel."
    )]
    pub no_color: bool,

    /// Output format for the final summary: plaintext, json, or yaml
    #[arg(long, default_value = "plaintext", value_parser = ["plaintext", "json", "yaml"], help="The format to use for the report summary at the end of the run.")]
    pub summary_format: String,
}

impl Options {
    pub fn load() -> Self {
        Self::parse()
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::*;
    use carbide_test_support::{Check, check_values, scenarios, value_scenarios};

    use super::*;

    /// Names an `AgentPlatformType` so parse results compare as a stable tag --
    /// the enum is a clap value type and isn't `PartialEq`.
    fn platform_tag(t: &AgentPlatformType) -> &'static str {
        match t {
            AgentPlatformType::DpuOs => "dpu-os",
            AgentPlatformType::Containerized => "containerized",
        }
    }

    /// Names an `HbnConfigMode` for the same reason as [`platform_tag`].
    fn hbn_mode_tag(m: &HbnConfigMode) -> &'static str {
        match m {
            HbnConfigMode::ContainerExec => "container-exec",
            HbnConfigMode::NvueRest => "nvue-rest",
        }
    }

    #[test]
    fn test_platform_type_from_str() {
        scenarios!(run = |s: &str| s
            .parse::<AgentPlatformType>()
            .map(|t| platform_tag(&t))
            .map_err(|e| e.to_string());
            "valid values map to their variant" {
                "dpu-os" => Yields("dpu-os"),
                "containerized" => Yields("containerized"),
            }

            "unknown values are rejected" {
                // `init-container` is now a dedicated subcommand, not a platform-type
                // value; callers must use the subcommand instead.
                "banana" => Fails,
                "init-container" => Fails,
            }
        );
    }

    #[test]
    fn test_platform_type_rejects_unknown_value_naming_the_input() {
        // The rejection message echoes the offending value so operators can see
        // what they mistyped.
        for bad in ["banana", "init-container"] {
            let err = bad.parse::<AgentPlatformType>().unwrap_err();
            assert!(err.to_string().contains(bad), "error should name {bad}");
        }
    }

    #[test]
    fn test_hbn_config_mode_from_str() {
        scenarios!(run = |s: &str| s
            .parse::<HbnConfigMode>()
            .map(|m| hbn_mode_tag(&m))
            .map_err(|e| e.to_string());
            "valid modes map to their variant" {
                "container-exec" => Yields("container-exec"),
                "nvue-rest" => Yields("nvue-rest"),
            }

            "unknown modes are rejected" {
                "banana" => Fails,
                "" => Fails,
            }
        );
    }

    #[test]
    fn test_hbn_config_mode_rejects_unknown_value_naming_the_input() {
        let err = "banana".parse::<HbnConfigMode>().unwrap_err();
        assert!(err.to_string().contains("banana"));
    }

    #[test]
    fn test_is_dpu_os_only_true_for_dpu_os() {
        check_values(
            [
                Check {
                    scenario: "dpu-os is the DPU OS",
                    input: AgentPlatformType::DpuOs,
                    expect: true,
                },
                Check {
                    scenario: "containerized is not the DPU OS",
                    input: AgentPlatformType::Containerized,
                    expect: false,
                },
            ],
            |t| t.is_dpu_os(),
        );
    }

    #[test]
    fn test_is_container_exec_only_true_for_container_exec() {
        check_values(
            [
                Check {
                    scenario: "container-exec uses crictl exec",
                    input: HbnConfigMode::ContainerExec,
                    expect: true,
                },
                Check {
                    scenario: "nvue-rest does not use crictl exec",
                    input: HbnConfigMode::NvueRest,
                    expect: false,
                },
            ],
            |m| m.is_container_exec(),
        );
    }

    #[test]
    fn test_init_container_platform_type_no_longer_accepted() {
        // Guard against regressing the refactor: `init-container` is now a dedicated
        // subcommand, not a platform-type value. Callers must use the subcommand instead.
        let err = "init-container".parse::<AgentPlatformType>().unwrap_err();
        assert!(err.to_string().contains("init-container"));
    }

    #[test]
    fn test_init_container_subcommand_parses_without_args() {
        let opts = Options::try_parse_from(["forge-dpu-agent", "init-container"]).unwrap();
        let Some(AgentCommand::InitContainer(options)) = opts.cmd else {
            panic!("expected init-container command");
        };
        assert!(matches!(
            options.bootstrap_ca_source,
            InitContainerBootstrapCaSource::LegacyDownload
        ));
        assert_eq!(options.bootstrap_ca_url.as_str(), DEFAULT_BOOTSTRAP_CA_URL);
    }

    #[test]
    fn test_init_container_subcommand_parses_bootstrap_ca_sources() {
        value_scenarios!(run = |value: &str| {
            Options::try_parse_from([
                "forge-dpu-agent",
                "init-container",
                "--bootstrap-ca-source",
                value,
            ])
            .is_ok_and(|opts| {
                matches!(opts.cmd, Some(AgentCommand::InitContainer(_)))
            })
        };
            "supported bootstrap CA sources parse" {
                "legacy_download" => true,
                "mounted" => true,
            }

            "unknown bootstrap CA sources are rejected" {
                "embedded" => false,
                "download-or-whatever" => false,
            }
        );
    }

    #[test]
    fn test_init_container_subcommand_parses_custom_bootstrap_ca_url() {
        let opts = Options::try_parse_from([
            "forge-dpu-agent",
            "init-container",
            "--bootstrap-ca-url",
            "https://pxe.example.test/custom/ca.pem",
        ])
        .unwrap();
        let Some(AgentCommand::InitContainer(options)) = opts.cmd else {
            panic!("expected init-container command");
        };
        assert_eq!(
            options.bootstrap_ca_url.as_str(),
            "https://pxe.example.test/custom/ca.pem"
        );
    }

    #[test]
    fn test_init_container_subcommand_rejects_invalid_bootstrap_ca_url() {
        let result = Options::try_parse_from([
            "forge-dpu-agent",
            "init-container",
            "--bootstrap-ca-url",
            "not a URL",
        ]);
        assert!(result.is_err());
    }

    #[test]
    fn test_init_container_subcommand_rejects_unsupported_bootstrap_ca_url_scheme() {
        let result = Options::try_parse_from([
            "forge-dpu-agent",
            "init-container",
            "--bootstrap-ca-url",
            "file:///tmp/site-ca.pem",
        ]);
        let error = result.err().expect("file URL should be rejected");
        assert!(
            error.to_string().contains("expected http or https"),
            "unexpected parser error: {error}"
        );
    }

    #[test]
    fn test_init_container_subcommand_rejects_output_file_flag() {
        // Hardware output remains fixed even though bootstrap CA options are configurable.
        let result = Options::try_parse_from([
            "forge-dpu-agent",
            "init-container",
            "--output-file",
            "/tmp/x",
        ]);
        assert!(result.is_err());
    }

    #[test]
    fn test_hardware_subcommand_rejects_init_container_platform_type() {
        // `hardware --agent-platform-type=init-container` used to download certs + save.
        // That behavior moved to the InitContainer subcommand; this value must now fail.
        let result = Options::try_parse_from([
            "forge-dpu-agent",
            "hardware",
            "--agent-platform-type=init-container",
        ]);
        assert!(result.is_err());
    }

    #[test]
    fn test_hardware_subcommand_accepts_remaining_platform_types() {
        value_scenarios!(run = |value: &str| {
            Options::try_parse_from(["forge-dpu-agent", "hardware", "--agent-platform-type", value])
                .is_ok_and(|opts| matches!(opts.cmd, Some(AgentCommand::Hardware(_))))
        };
            "remaining platform types parse as the hardware subcommand" {
                "dpu-os" => true,
                "containerized" => true,
            }
        );
    }

    #[test]
    fn test_lldp_neighbors_subcommand_accepts_platform_types() {
        value_scenarios!(run = |value: &str| {
            Options::try_parse_from([
                "forge-dpu-agent",
                "lldp-neighbors",
                "--agent-platform-type",
                value,
            ])
            .is_ok_and(|opts| matches!(opts.cmd, Some(AgentCommand::LldpNeighbors(_))))
        };
            "supported LLDP platform types parse" {
                "dpu-os" => true,
                "containerized" => true,
            }

            "unknown LLDP platform types are rejected" {
                "init-container" => false,
            }
        );
    }

    #[test]
    fn test_sidecar_mode_subcommand_parses_without_args() {
        let opts = Options::try_parse_from(["forge-dpu-agent", "sidecar-mode"]).unwrap();
        assert!(matches!(opts.cmd, Some(AgentCommand::SidecarMode)));
    }
}
