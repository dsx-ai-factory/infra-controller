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
use clap::{Parser, ValueEnum, ValueHint};
use rpc::admin_cli::OutputFormat;

use crate::{
    attestation, bmc_machine, boot_interface, boot_override, browse, component_manager,
    compute_allocation, credential, devenv, domain, dpa, dpu, dpu_remediation, expected_machines,
    expected_power_shelf, expected_rack, expected_switch, extension_service, firmware,
    generate_docs, generate_man, generate_shell_complete, host, ib_partition, instance,
    instance_type, inventory, ip, ipxe_template, jump, machine, machine_interfaces,
    machine_validation, managed_host, managed_switch, mlx, network_devices, network_security_group,
    network_segment, nvl_domain, nvl_logical_partition, nvl_partition, nvlink_nmxc_endpoints,
    operating_system, os_image, ping, power_shelf, rack, redfish, resource_pool, rms, route_server,
    scout_stream, secrets, set, site_explorer, site_prefix, sku, spx_partition, ssh, switch,
    tenant, tenant_keyset, tpm_ca, trim_table, version, vpc, vpc_peering, vpc_prefix,
};

const MAX_INTERNAL_PAGE_SIZE: usize = 100;

/// Default number of ids per internal `*ByIds` page. The gRPC client refuses
/// replies above 4 MiB (tonic's default `max_decoding_message_size`), and a
/// 100-machine page has exceeded that in the field: 4.6 MiB, about 46 KB per
/// machine. Instances have measured about 78 KB each, so 25 keeps a page of
/// either under half the limit. Raise it per invocation with
/// `--internal-page-size`, or raise the limit itself with
/// `TONIC_MAX_DECODING_MESSAGE_SIZE`.
const DEFAULT_INTERNAL_PAGE_SIZE: usize = 25;

fn parse_internal_page_size(value: &str) -> Result<usize, String> {
    let page_size = value
        .parse::<usize>()
        .map_err(|err| format!("invalid internal page size: {err}"))?;

    if (1..=MAX_INTERNAL_PAGE_SIZE).contains(&page_size) {
        Ok(page_size)
    } else {
        Err(format!(
            "internal page size must be between 1 and {MAX_INTERNAL_PAGE_SIZE}"
        ))
    }
}

#[derive(Parser, Debug)]
#[clap(name = "nico-admin-cli")]
#[clap(author = "https://github.com/NVIDIA/infra-controller")]
pub(crate) struct CliOptions {
    #[clap(
        long,
        default_value = "false",
        help = "Print version number of nico-admin-cli and exit. For API server version see 'version' command."
    )]
    pub(crate) version: bool,

    #[clap(
        long,
        value_hint = ValueHint::Username,
        value_name = "USERNAME",
        help = "Never should be used against a production site. Use this flag only if you understand the impacts of inconsistencies with cloud db."
    )]
    pub(crate) cloud_unsafe_op: Option<String>,

    #[clap(short, long, env = "API_URL", visible_alias = "carbide-url")]
    #[clap(
        help = "Default to API_URL environment variable or $HOME/.config/nico_api_cli.json file or https://carbide-api.forge-system.svc.cluster.local:1079."
    )]
    pub(crate) api_url: Option<String>,

    #[clap(short, long, value_enum, default_value = "ascii-table")]
    pub(crate) format: OutputFormat,

    #[clap(short, long)]
    pub(crate) output: Option<String>,

    #[clap(long, env = "ROOT_CA_PATH", visible_alias = "forge-root-ca-path")]
    #[clap(
        help = "Default to ROOT_CA_PATH environment variable or $HOME/.config/nico_api_cli.json file."
    )]
    pub(crate) root_ca_path: Option<String>,

    #[clap(long, env = "CLIENT_CERT_PATH")]
    #[clap(
        help = "Default to CLIENT_CERT_PATH environment variable or $HOME/.config/nico_api_cli.json file."
    )]
    pub(crate) client_cert_path: Option<String>,

    #[clap(long, env = "CLIENT_KEY_PATH")]
    #[clap(
        help = "Default to CLIENT_KEY_PATH environment variable or $HOME/.config/nico_api_cli.json file."
    )]
    pub(crate) client_key_path: Option<String>,

    #[clap(long, env = "RMS_API_URL")]
    #[clap(help = "RMS API URL. Default to RMS_API_URL environment variable.")]
    pub(crate) rms_api_url: Option<String>,

    #[clap(long, env = "RMS_ROOT_CA_PATH")]
    #[clap(help = "RMS Root CA path. Default to RMS_ROOT_CA_PATH environment variable.")]
    pub(crate) rms_root_ca_path: Option<String>,

    #[clap(long, env = "RMS_CLIENT_CERT_PATH")]
    #[clap(
        help = "RMS client certificate path. Default to RMS_CLIENT_CERT_PATH environment variable."
    )]
    pub(crate) rms_client_cert_path: Option<String>,

    #[clap(long, env = "RMS_CLIENT_KEY_PATH")]
    #[clap(help = "RMS client key path. Default to RMS_CLIENT_KEY_PATH environment variable.")]
    pub(crate) rms_client_key_path: Option<String>,

    #[clap(short, long, num_args(0..), default_value = "0")]
    pub(crate) debug: u8,

    /// Extended result output.
    ///
    /// This is used by measured boot, where basic output contains just
    /// what you probably care about, and "extended" output also dumps out all
    /// the internal UUIDs that are used to associate instances.
    #[clap(long, global = true)]
    pub(crate) extended: bool,

    #[clap(subcommand)]
    pub(crate) commands: Option<CliCommand>,

    #[clap(
        short = 'p',
        long,
        default_value_t = DEFAULT_INTERNAL_PAGE_SIZE,
        value_parser = parse_internal_page_size
    )]
    #[clap(
        help = "For commands that internally retrieve data with paging, use this page size (1-100). \
                Smaller pages keep each gRPC reply under the client's 4 MiB receive limit; to raise \
                that limit instead, set TONIC_MAX_DECODING_MESSAGE_SIZE to a byte count, for \
                example 33554432 for 32 MiB."
    )]
    pub(crate) internal_page_size: usize,

    #[clap(
        long,
        value_enum,
        global = true,
        help = "Sort output by specified field",
        default_value = "primary-id"
    )]
    pub(crate) sort_by: SortField,
}

#[derive(PartialEq, Eq, ValueEnum, Clone, Debug)]
#[clap(rename_all = "kebab_case")]
pub(crate) enum SortField {
    #[clap(help = "Sort by the primary ID")]
    PrimaryId,
    #[clap(help = "Sort by state")]
    State,
}

#[derive(Parser, Debug)]
pub(crate) enum CliCommand {
    #[clap(
        about = "MeasuredBoot or SPDM attestations",
        subcommand,
        visible_alias = "att"
    )]
    Attestation(attestation::Cmd),
    #[clap(
        about = "BMC Machine related handling",
        subcommand,
        visible_alias = "bmc"
    )]
    BmcMachine(bmc_machine::Cmd),
    #[clap(
        about = "Machine boot-interface management",
        subcommand,
        visible_alias = "bi"
    )]
    BootInterface(boot_interface::Cmd),
    #[clap(about = "Machine boot override", subcommand)]
    BootOverride(boot_override::Cmd),
    #[clap(
        about = "Browse subsystem resource trees via the API server",
        subcommand
    )]
    Browse(browse::Cmd),
    #[clap(about = "Component manager actions", visible_alias = "cm", subcommand)]
    ComponentManager(component_manager::Cmd),
    #[clap(
        about = "Compute allocation management",
        visible_alias = "ca",
        subcommand
    )]
    ComputeAllocation(compute_allocation::Cmd),
    #[clap(about = "Credential related handling", subcommand, visible_alias = "c")]
    Credential(credential::Cmd),
    #[clap(about = "Dev Env related handling", subcommand)]
    DevEnv(devenv::Cmd),
    #[clap(about = "Domain related handling", subcommand, visible_alias = "d")]
    Domain(domain::Cmd),
    #[clap(about = "DPA related handling", subcommand)]
    Dpa(dpa::Cmd),
    #[clap(subcommand)]
    #[clap(verbatim_doc_comment)]
    /// DPF-related commands.
    /// Note: These commands update the DPF state of the machine, which determines DPF-based DPU re-provisioning.
    /// The state is saved in the machine's metadata and will be deleted if the machine is force-deleted.
    /// To make the state persistent, add the DPF state for a machine (host) to the expected machines table.
    Dpf(crate::dpf::Cmd),
    #[clap(about = "DPU specific handling", subcommand)]
    Dpu(dpu::Cmd),
    #[clap(about = "Dpu Remediation handling", subcommand)]
    DpuRemediation(dpu_remediation::Cmd),
    #[clap(about = "Expected machine handling", subcommand, visible_alias = "em")]
    ExpectedMachine(expected_machines::Cmd),
    #[clap(
        about = "Expected power shelf handling",
        subcommand,
        visible_alias = "ep"
    )]
    ExpectedPowerShelf(expected_power_shelf::Cmd),
    #[clap(about = "Expected rack handling", subcommand, visible_alias = "er")]
    ExpectedRack(expected_rack::Cmd),
    #[clap(about = "Expected switch handling", subcommand, visible_alias = "ew")]
    ExpectedSwitch(expected_switch::Cmd),
    #[clap(
        about = "Extension service management",
        visible_alias = "es",
        subcommand
    )]
    ExtensionService(extension_service::Cmd),
    #[clap(about = "Firmware related actions", subcommand)]
    Firmware(firmware::Cmd),
    #[clap(about = "Secrets management", subcommand)]
    Secrets(secrets::Cmd),
    #[clap(
        about = "Regenerate the docs/manuals/nico-admin-cli markdown reference",
        hide = true
    )]
    GenerateCliDocs(generate_docs::Cmd),
    #[clap(about = "Generate man pages for the CLI", hide = true)]
    GenerateMan(generate_man::Cmd),
    #[clap(
        about = "Generate shell autocomplete. Source the output of this command: `source <(nico-admin-cli generate-shell-complete bash)`"
    )]
    GenerateShellComplete(generate_shell_complete::Cmd),
    #[clap(about = "Host specific handling", subcommand)]
    Host(host::Cmd),
    #[clap(
        about = "InfiniBand Partition related handling",
        subcommand,
        visible_alias = "ibp"
    )]
    IbPartition(ib_partition::Cmd),
    #[clap(about = "Instance related handling", subcommand, visible_alias = "i")]
    Instance(instance::Cmd),
    #[clap(about = "Instance type management", visible_alias = "it", subcommand)]
    InstanceType(instance_type::Cmd),
    #[clap(about = "Generate Ansible Inventory")]
    Inventory(inventory::Cmd),
    #[clap(about = "IP address handling", subcommand)]
    Ip(ip::Cmd),
    #[clap(
        about = "iPXE template management",
        visible_alias = "ipxe-tmpl",
        subcommand
    )]
    IpxeTemplate(ipxe_template::Cmd),
    #[clap(
        about = "Broad search across multiple object types",
        visible_alias = "j"
    )]
    Jump(jump::Cmd),
    #[clap(
        about = "Logical partition related handling",
        subcommand,
        visible_alias = "lp"
    )]
    LogicalPartition(nvl_logical_partition::Cmd),
    #[clap(about = "Machine related handling", subcommand, visible_alias = "m")]
    Machine(machine::Cmd),
    #[clap(
        about = "Machine interfaces and address management",
        subcommand,
        visible_alias = "mi"
    )]
    MachineInterfaces(machine_interfaces::Cmd),
    #[clap(about = "Machine Validation", subcommand, visible_alias = "mv")]
    MachineValidation(machine_validation::Cmd),
    #[clap(
        about = "Managed host related handling",
        subcommand,
        visible_alias = "mh"
    )]
    ManagedHost(managed_host::Cmd),
    #[clap(
        about = "Managed switch related handling",
        subcommand,
        visible_alias = "ms"
    )]
    ManagedSwitch(managed_switch::Cmd),
    #[clap(about = "Mellanox Device Handling", subcommand)]
    Mlx(mlx::MlxAction),
    #[clap(about = "Network Devices handling", subcommand)]
    NetworkDevice(network_devices::Cmd),
    #[clap(
        about = "Network security group management",
        visible_alias = "nsg",
        subcommand
    )]
    NetworkSecurityGroup(network_security_group::Cmd),
    #[clap(
        about = "Network Segment related handling",
        subcommand,
        visible_alias = "ns"
    )]
    NetworkSegment(network_segment::Cmd),
    #[clap(
        about = "NVLink domain related handling",
        subcommand,
        visible_alias = "nvd"
    )]
    NvlDomain(nvl_domain::Cmd),
    #[clap(
        about = "NvLink Partition related handling",
        subcommand,
        visible_alias = "nvp"
    )]
    NvlPartition(nvl_partition::Cmd),
    #[clap(
        name = "nvlink-nmxc-endpoints",
        about = "Rack chassis serial → NMX-C endpoint mappings",
        subcommand
    )]
    NvlinkNmxcEndpoints(nvlink_nmxc_endpoints::Cmd),
    #[clap(
        about = "Operating system definition management",
        visible_alias = "osd",
        subcommand
    )]
    OperatingSystem(operating_system::Cmd),
    #[clap(about = "OS catalog management", visible_alias = "os", subcommand)]
    OsImage(os_image::Cmd),
    #[clap(
        about = "Query the Version gRPC endpoint repeatedly printing how long it took and any failures."
    )]
    Ping(ping::Opts),
    #[clap(about = "Power Shelf management", subcommand, visible_alias = "ps")]
    PowerShelf(power_shelf::Cmd),
    #[clap(about = "Rack Management", subcommand)]
    Rack(rack::Cmd),
    #[clap(about = "Redfish BMC actions", visible_alias = "rf")]
    Redfish(redfish::RedfishAction),
    #[clap(about = "Resource pool handling", subcommand, visible_alias = "rp")]
    ResourcePool(resource_pool::Cmd),
    #[clap(about = "RMS Actions")]
    Rms(rms::args::RmsAction),
    #[clap(about = "Route server handling", subcommand)]
    RouteServer(route_server::Cmd),
    #[clap(about = "Scout Stream Connection Handling", subcommand)]
    ScoutStream(scout_stream::ScoutStreamAction),
    #[clap(about = "Set carbide-api dynamic features", subcommand)]
    Set(set::Cmd),
    #[clap(about = "Site explorer functions", subcommand)]
    SiteExplorer(site_explorer::Cmd),
    #[clap(about = "SitePrefix management", subcommand)]
    SitePrefix(site_prefix::Cmd),
    #[clap(about = "Manage machine SKUs", subcommand)]
    Sku(sku::Cmd),
    #[clap(
        about = "SPX Partition related handling",
        subcommand,
        visible_alias = "spx"
    )]
    SpxPartition(spx_partition::Cmd),
    #[clap(about = "SSH Util functions", subcommand)]
    Ssh(ssh::Cmd),
    #[clap(about = "Switch management", subcommand, visible_alias = "sw")]
    Switch(switch::Cmd),
    #[clap(about = "Tenant management", subcommand, visible_alias = "tm")]
    Tenant(tenant::Cmd),
    #[clap(
        about = "Tenant KeySet related handling",
        subcommand,
        visible_alias = "tks"
    )]
    TenantKeySet(tenant_keyset::Cmd),
    #[clap(about = "Manage TPM CA certificates", subcommand)]
    TpmCa(tpm_ca::Cmd),
    #[clap(about = "Trim DB tables", subcommand)]
    TrimTable(trim_table::Cmd),
    #[clap(about = "Print API server version", visible_alias = "v")]
    Version(version::Opts),
    #[clap(about = "VPC related handling", subcommand)]
    Vpc(vpc::Cmd),
    #[clap(about = "VPC peering handling", subcommand)]
    VpcPeering(vpc_peering::Cmd),
    #[clap(about = "VPC prefix handling", subcommand)]
    VpcPrefix(vpc_prefix::Cmd),
}

impl CliOptions {
    pub(crate) fn load() -> Self {
        Self::parse()
    }
}

// =============================================================================
// CLI documentation domains
// =============================================================================
//
// The generated CLI reference (docs/manuals/nico-admin-cli) groups top-level commands into four
// operator-facing domains. clap has no native way to categorize subcommands in
// `--help`, so the grouping lives in `cli_domains.yaml` (at the crate root) as
// the single source of truth and is consumed by the `generate-cli-docs`
// generator.
//
// The YAML maps each domain to its list of command names as clap renders them
// (e.g. "managed-host", "tenant-key-set"). We embed it with `include_str!` so
// the binary carries the mapping with no runtime file dependency; the cost is
// that editing the YAML requires a rebuild. The `every_command_has_a_domain`
// test fails CI if a newly added top-level command is not assigned a domain, so
// the docs stay categorized as the CLI grows.

use std::collections::HashMap;
use std::sync::LazyLock;

use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum CliDomain {
    Hardware,
    Network,
    Tenant,
    Admin,
}

impl CliDomain {
    pub(crate) fn title(self) -> &'static str {
        match self {
            CliDomain::Hardware => "Hardware",
            CliDomain::Network => "Network",
            CliDomain::Tenant => "Tenant",
            CliDomain::Admin => "Admin",
        }
    }
}

/// Command-name → domain, inverted once from the `domain -> [command names]`
/// shape of the embedded `cli_domains.yaml`.
static COMMAND_DOMAINS: LazyLock<HashMap<String, CliDomain>> = LazyLock::new(|| {
    let by_domain: HashMap<CliDomain, Vec<String>> = serde_yaml::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/cli_domains.yaml"
    )))
    .expect("cli_domains.yaml parses as a mapping of domain -> [command names]");

    let mut by_command = HashMap::new();
    for (domain, commands) in by_domain {
        for command in commands {
            if let Some(previous) = by_command.insert(command.clone(), domain) {
                panic!(
                    "command `{command}` is listed under two domains \
                     ({previous:?} and {domain:?}) in cli_domains.yaml"
                );
            }
        }
    }
    by_command
});

/// The domain a top-level command belongs to, keyed by its rendered name.
/// Returns `None` for a command absent from `cli_domains.yaml` — every visible
/// command must be listed there or `every_command_has_a_domain` fails.
pub(crate) fn domain_for_command(name: &str) -> Option<CliDomain> {
    COMMAND_DOMAINS.get(name).copied()
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use clap::error::ErrorKind;
    use clap::{CommandFactory, Parser};

    use super::{COMMAND_DOMAINS, CliOptions, domain_for_command};

    // Visible top-level command names as clap renders them, excluding hidden
    // commands (e.g. generate-man) and the synthetic `help` command.
    fn visible_command_names() -> Vec<String> {
        CliOptions::command()
            .get_subcommands()
            .filter(|sub| !sub.is_hide_set())
            .map(|sub| sub.get_name().to_string())
            .filter(|name| name != "help")
            .collect()
    }

    // Fails if a visible top-level command is missing from cli_domains.yaml.
    // When someone adds a new command, this points them at the YAML so the
    // generated docs (docs/manuals/nico-admin-cli) stay grouped by domain.
    #[test]
    fn every_command_has_a_domain() {
        let missing: Vec<String> = visible_command_names()
            .into_iter()
            .filter(|name| domain_for_command(name).is_none())
            .collect();
        assert!(
            missing.is_empty(),
            "these top-level commands have no CLI-docs domain; add them to \
             crates/admin-cli/cli_domains.yaml: {missing:?}"
        );
    }

    // Fails if cli_domains.yaml lists a command that no longer exists, so a
    // rename or removal can't leave a stale entry behind.
    #[test]
    fn no_unknown_commands_in_domain_map() {
        let real: HashSet<String> = visible_command_names().into_iter().collect();
        let stale: Vec<&String> = COMMAND_DOMAINS
            .keys()
            .filter(|name| !real.contains(*name))
            .collect();
        assert!(
            stale.is_empty(),
            "cli_domains.yaml lists commands that are not real top-level \
             commands; rename or remove them: {stale:?}"
        );
    }

    #[test]
    fn internal_page_size_rejects_zero() {
        let err = CliOptions::try_parse_from(["nico-admin-cli", "--internal-page-size", "0"])
            .expect_err("zero page size should be rejected");

        assert_eq!(err.kind(), ErrorKind::ValueValidation);
    }

    #[test]
    fn internal_page_size_rejects_values_above_max() {
        let err = CliOptions::try_parse_from(["nico-admin-cli", "--internal-page-size", "101"])
            .expect_err("page sizes above max should be rejected");

        assert_eq!(err.kind(), ErrorKind::ValueValidation);
    }

    #[test]
    fn internal_page_size_accepts_valid_values() {
        let opts = CliOptions::try_parse_from(["nico-admin-cli", "--internal-page-size", "100"])
            .expect("valid page size should parse");

        assert_eq!(opts.internal_page_size, 100);
    }

    #[test]
    fn internal_page_size_defaults_below_the_grpc_receive_limit() {
        let opts = CliOptions::try_parse_from(["nico-admin-cli"]).expect("bare invocation parses");

        // A 100-id page of machines has exceeded tonic's 4 MiB receive limit in
        // the field, so the default must keep a page of the largest records seen
        // (instances, about 78 KB each) comfortably under it.
        assert_eq!(opts.internal_page_size, 25);
    }
}
