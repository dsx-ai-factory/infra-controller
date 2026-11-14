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

//! `site-def.yaml`: the one file a provider writes for the checks. It states
//! intent; the collaterals NICo's tooling reads are checked against it.
//! Key names follow the provider guide's Appendix A; where a quantity also
//! exists in the CP-DPU site yaml the key name is the same.

use std::net::IpAddr;
use std::path::{Path, PathBuf};

use eyre::{Context, Result};
use indexmap::IndexMap;
use ipnet::IpNet;
use serde::Deserialize;
use uuid::Uuid;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct SiteDef {
    pub(crate) schema_version: u32,
    pub(crate) site: Site,
    pub(crate) collaterals: Collaterals,
    /// Output directory, relative to this file; the only path the tool writes to.
    pub(crate) results: PathBuf,
    /// One key per register entry; the value is the option's short name, or
    /// a list of them for an "Any of" entry (an empty list means nothing ticked).
    #[serde(default)]
    pub(crate) decisions: IndexMap<String, DecisionValue>,
    pub(crate) hosts: Hosts,
    pub(crate) network: Network,
    pub(crate) counts: Counts,
    #[serde(default)]
    pub(crate) fleet: Option<Fleet>,
    pub(crate) versions: Versions,
    #[serde(default)]
    pub(crate) credentials: Option<Credentials>,
    pub(crate) setup: Setup,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct Site {
    pub(crate) name: String,
    pub(crate) datacenter: String,
    pub(crate) dns_zone: String,
    /// The external name of the Core API; must resolve to the API VIP.
    pub(crate) api_hostname: String,
    pub(crate) uuid_source: UuidSource,
    #[serde(default)]
    pub(crate) description: Option<String>,
}

/// A decision's stated value: one option name, or a list for a multi-select.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub(crate) enum DecisionValue {
    One(String),
    Many(Vec<String>),
}

impl DecisionValue {
    pub(crate) fn values(&self) -> Vec<String> {
        match self {
            DecisionValue::One(s) => vec![s.clone()],
            DecisionValue::Many(v) => v.clone(),
        }
    }

    /// `a` for one value, `[a, b]` for a list, as a site definition writes it. A value YAML
    /// would not read back as the same plain scalar is double-quoted.
    pub(crate) fn render(&self) -> String {
        match self {
            DecisionValue::One(s) => yaml_scalar(s),
            DecisionValue::Many(v) => format!(
                "[{}]",
                v.iter()
                    .map(|s| yaml_scalar(s))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }
}

/// A string as a YAML plain scalar when YAML reads it back unchanged, else double-quoted
/// with `"` and `\` escaped. Option names are plain words; this guards the odd one.
fn yaml_scalar(s: &str) -> String {
    let plain = !s.is_empty()
        && !s.starts_with([
            ' ', '-', '?', ':', ',', '[', ']', '{', '}', '#', '&', '*', '!', '|', '>', '\'', '"',
            '%', '@', '`',
        ])
        && !s.ends_with(' ')
        && !s.contains(": ")
        && !s.contains(" #")
        && !s.contains(['\n', ',', '[', ']', '{', '}'])
        && s.parse::<f64>().is_err()
        && !matches!(
            s.to_ascii_lowercase().as_str(),
            "true" | "false" | "null" | "yes" | "no" | "on" | "off" | "~"
        );
    if plain {
        s.to_string()
    } else {
        format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
    }
}

/// `setup` (setup.sh mints and seeds the UUID) or `fixed: <uuid>`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(try_from = "String")]
pub(crate) enum UuidSource {
    Setup,
    Fixed(Uuid),
}

impl TryFrom<String> for UuidSource {
    type Error = String;

    fn try_from(value: String) -> std::result::Result<Self, Self::Error> {
        let value = value.trim();
        if value == "setup" {
            return Ok(UuidSource::Setup);
        }
        match value.strip_prefix("fixed:") {
            Some(rest) => rest
                .trim()
                .parse::<Uuid>()
                .map(UuidSource::Fixed)
                .map_err(|e| format!("uuidSource `{value}`: not a UUID: {e}")),
            None => Err(format!(
                "uuidSource must be `setup` or `fixed: <uuid>`, got `{value}`"
            )),
        }
    }
}

/// Paths of the configuration files NICo's own tooling reads, relative to
/// the site definition. `nico-pre-config` validates them; nothing generates them.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct Collaterals {
    /// Absent when the site controllers carry no DPU: no ISO is built.
    #[serde(default)]
    pub(crate) cp_dpu_site_yaml: Option<PathBuf>,
    pub(crate) helm_prereqs_values: PathBuf,
    /// Carries the NICo TOML.
    pub(crate) core_values: PathBuf,
    pub(crate) rest_values: PathBuf,
    pub(crate) site_agent_values: PathBuf,
    pub(crate) metallb_config: PathBuf,
    /// Needed from step 7.
    #[serde(default)]
    pub(crate) expected_machines: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct Hosts {
    #[serde(default)]
    pub(crate) jumphost: Option<NamedHost>,
    pub(crate) staging_host: StagingHost,
    #[serde(default)]
    pub(crate) build_host: Option<Host>,
    #[serde(default)]
    pub(crate) registry_host: Option<RegistryHost>,
    /// A host in the BMC segment that can run the probe container.
    #[serde(default)]
    pub(crate) bmc_probe_host: Option<IpAddr>,
    /// Vantage points for the fabric checks on the segments no utility host sits on,
    /// before the site controllers are up.
    #[serde(default)]
    pub(crate) probe_hosts: Option<ProbeHosts>,
    pub(crate) site_controllers: Vec<SiteController>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct ProbeHosts {
    /// A host on the admin segment.
    #[serde(default)]
    pub(crate) admin: Option<IpAddr>,
    /// One host per `hostInband` segment, by the segment's `name`.
    #[serde(default)]
    pub(crate) host_inband: Vec<NamedProbe>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct NamedProbe {
    pub(crate) name: String,
    pub(crate) address: IpAddr,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct NamedHost {
    #[serde(default)]
    pub(crate) name: Option<String>,
    pub(crate) address: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct Host {
    pub(crate) address: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct StagingHost {
    pub(crate) address: String,
    #[serde(default)]
    pub(crate) ssh_user: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct RegistryHost {
    pub(crate) url: String,
    /// The CA that signed the registry certificate, by source; not a secret.
    #[serde(default)]
    pub(crate) ca_source: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct SiteController {
    pub(crate) host_name: String,
    pub(crate) host_serial: String,
    /// Selects the HCL firmware pins in step 3.
    pub(crate) model: String,
    pub(crate) node_id: u32,
    pub(crate) bmc: IpAddr,
    #[serde(default)]
    pub(crate) dpu_bmc: Option<IpAddr>,
    #[serde(default)]
    pub(crate) p0_mac: Option<String>,
    /// Plain-NIC site controllers only: MetalLB peers with the ToRs directly.
    #[serde(default)]
    pub(crate) tor_peers: Option<Vec<BgpPeer>>,
    /// SSH to the control-plane DPU's OS, for the fabric checks that read FRR and
    /// LLDP on it. Without it those checks run only from media on the controller.
    #[serde(default)]
    pub(crate) dpu_ssh: Option<SshTarget>,
    /// The ToR each uplink is cabled to, as LLDP should report it.
    #[serde(default)]
    pub(crate) uplinks: Option<Vec<Uplink>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct SshTarget {
    pub(crate) address: IpAddr,
    #[serde(default)]
    pub(crate) user: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct Uplink {
    /// The local port (`p0`, `p1`).
    pub(crate) port: String,
    /// The switch's LLDP system name.
    pub(crate) tor: String,
    /// The switch port's LLDP port id, when known.
    #[serde(default)]
    pub(crate) tor_port: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct BgpPeer {
    pub(crate) address: IpAddr,
    pub(crate) asn: u32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct Network {
    pub(crate) datacenter_asn: u32,
    /// The site's BGP ASN range, one block: by convention MetalLB advertises from
    /// `bgpAsnStart`, site controller `nodeId` i peers as `bgpAsnStart + i`, and the rest is
    /// the per-DPU pool (`[pools.fnn-asn]`). `asnLayout` overrides the convention.
    pub(crate) bgp_asn_start: u32,
    pub(crate) bgp_asn_end: u32,
    #[serde(default)]
    pub(crate) asn_layout: Option<AsnLayout>,
    pub(crate) site_controller_mtu_size: u32,
    pub(crate) forge_control_plane_prefix: IpNet,
    pub(crate) forge_service_vip_prefix: IpNet,
    pub(crate) forge_dpu_loopback_prefix: IpNet,
    pub(crate) forge_overlay_prefixes: Vec<IpNet>,
    pub(crate) segments: Segments,
    pub(crate) kubernetes: Kubernetes,
    pub(crate) name_server: IpAddr,
    #[serde(default)]
    pub(crate) unbound_forward_addresses: Vec<IpAddr>,
    pub(crate) ntp_servers: Vec<IpAddr>,
    pub(crate) path_mtu: u32,
    /// How the site controllers and DPUs get their default route.
    #[serde(default)]
    pub(crate) default_route: Option<DefaultRoute>,
    pub(crate) pools: Pools,
    #[serde(default)]
    pub(crate) fnn: Option<Fnn>,
    /// Optional pin of a derived value; a pinned value the collateral contradicts is a ✗.
    #[serde(default)]
    pub(crate) deny_prefixes: Option<Vec<IpNet>>,
    #[serde(default)]
    pub(crate) service_vips: Option<ServiceVips>,
}

impl Network {
    /// ASN MetalLB and the site controllers advertise routes from.
    pub(crate) fn site_controller_routes_asn(&self) -> u32 {
        self.asn_layout
            .as_ref()
            .and_then(|l| l.site_controller_routes)
            .unwrap_or(self.bgp_asn_start)
    }

    /// ASN of the site controller with `node_id`.
    pub(crate) fn site_controller_asn(&self, node_id: u32) -> u32 {
        self.asn_layout
            .as_ref()
            .and_then(|l| l.site_controllers.as_ref())
            .and_then(|m| m.get(&node_id).copied())
            .unwrap_or_else(|| self.bgp_asn_start.saturating_add(node_id))
    }
}

/// Overrides for a site that did not lay its ASNs out by the convention; each key is
/// optional and the convention stands for whatever is absent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum DefaultRoute {
    /// Learnt from the ToRs over BGP.
    Bgp,
    /// Configured statically on each host.
    Static,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct AsnLayout {
    #[serde(default)]
    pub(crate) site_controller_routes: Option<u32>,
    /// Site controller `nodeId` to its ASN.
    #[serde(default)]
    pub(crate) site_controllers: Option<IndexMap<u32, u32>>,
}

/// Every segment other than the CP-DPU prefixes, with its gateway and MTU.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct Segments {
    pub(crate) admin: Segment,
    pub(crate) control_plane_oob: Segment,
    #[serde(default)]
    pub(crate) managed_host_bmc: Vec<SwitchSegment>,
    #[serde(default)]
    pub(crate) host_inband: Vec<NamedSegment>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct Segment {
    pub(crate) prefix: IpNet,
    pub(crate) gateway: IpAddr,
    pub(crate) mtu: u32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct SwitchSegment {
    pub(crate) switch_name: String,
    pub(crate) prefix: IpNet,
    pub(crate) gateway: IpAddr,
    pub(crate) mtu: u32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct NamedSegment {
    pub(crate) name: String,
    pub(crate) prefix: IpNet,
    pub(crate) gateway: IpAddr,
    pub(crate) mtu: u32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct Kubernetes {
    pub(crate) pod_cidr: IpNet,
    pub(crate) service_cidr: IpNet,
    pub(crate) api_vip: IpAddr,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct Pools {
    pub(crate) vlan: Range,
    pub(crate) vni: Range,
    #[serde(default)]
    pub(crate) external_vpc_vni: Option<Range>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Range {
    pub(crate) start: u64,
    pub(crate) end: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct Fnn {
    pub(crate) control_plane_vni: u32,
    #[serde(default)]
    pub(crate) admin_vpc_vni: Option<u32>,
    pub(crate) vpc_vrf_loopback_prefix: IpNet,
    #[serde(default)]
    pub(crate) pools_vpc_vni: Vec<Range>,
    pub(crate) route_targets: IndexMap<String, u32>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct ServiceVips {
    pub(crate) api: IpAddr,
    pub(crate) dhcp: IpAddr,
    pub(crate) dns0: IpAddr,
    pub(crate) dns1: IpAddr,
    pub(crate) pxe: IpAddr,
    pub(crate) ssh_console: IpAddr,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct Counts {
    pub(crate) hosts: u32,
    pub(crate) dpus: u32,
    pub(crate) tenants: u32,
    pub(crate) racks: u32,
}

/// What NICo will manage; selects BIOS profiles, firmware thresholds, and seed entries.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct Fleet {
    pub(crate) hosts: Vec<FleetHost>,
    #[serde(default)]
    pub(crate) dpu: Option<FleetDpu>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct FleetHost {
    pub(crate) model: String,
    #[serde(default)]
    pub(crate) gpu: Option<String>,
    pub(crate) count: u32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct FleetDpu {
    pub(crate) model: String,
    pub(crate) generation: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct Versions {
    /// Release tag of the checkout and the Core image.
    pub(crate) infra_controller: String,
    pub(crate) nico_rest: String,
    #[serde(default)]
    pub(crate) nv_rms: Option<String>,
    #[serde(default)]
    pub(crate) doca_platform: Option<String>,
    #[serde(default)]
    pub(crate) bfb: Option<String>,
    #[serde(default)]
    pub(crate) doca: Option<String>,
    #[serde(default)]
    pub(crate) hbn: Option<String>,
    pub(crate) kubernetes_baseline: String,
}

/// Site-wide credentials by source only; read from step 7.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct Credentials {
    pub(crate) bmc_root: SecretSource,
    #[serde(default)]
    pub(crate) uefi: Option<SecretSource>,
}

/// Where a secret lives. Never the value.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct SecretSource {
    /// `env`, `file`, or `vault`.
    pub(crate) from: String,
    #[serde(default)]
    pub(crate) path: Option<PathBuf>,
    #[serde(default)]
    pub(crate) name: Option<String>,
}

/// What the provider will run; `nico-pre-platform` prints it as the command line.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct Setup {
    pub(crate) command: String,
    #[serde(default)]
    pub(crate) env: IndexMap<String, EnvValue>,
}

/// A `setup.sh` variable: a plain value, or a secret named by its source.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub(crate) enum EnvValue {
    Secret(SecretSource),
    Plain(String),
}

/// Reads and parses a site definition. Unknown keys are errors.
pub(crate) fn load(path: &Path) -> Result<SiteDef> {
    let text = std::fs::read_to_string(path)
        .wrap_err_with(|| format!("reading site definition {}", path.display()))?;
    parse(&text).wrap_err_with(|| format!("parsing site definition {}", path.display()))
}

/// The one schema this build reads; `SITE-DEF.md` documents it.
const SCHEMA_VERSION: u32 = 1;

pub(crate) fn parse(text: &str) -> Result<SiteDef> {
    let sd: SiteDef = serde_yaml::from_str(text)?;
    eyre::ensure!(
        sd.schema_version == SCHEMA_VERSION,
        "schemaVersion {} is not supported; this nico-pre reads schemaVersion {SCHEMA_VERSION}",
        sd.schema_version
    );
    Ok(sd)
}

impl SiteDef {
    /// The results directory, resolved against the site definition's location.
    pub(crate) fn results_dir(&self, site_def_path: &Path) -> PathBuf {
        let base = site_def_path.parent().unwrap_or_else(|| Path::new("."));
        base.join(&self.results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) const SAMPLE: &str = include_str!("../examples/site-def.yaml");

    #[test]
    fn sample_parses() {
        let sd = parse(SAMPLE).expect("the sample site definition parses");
        assert_eq!(sd.site.name, "site1");
        assert_eq!(sd.hosts.site_controllers.len(), 3);
        assert_eq!(sd.decisions.len(), 5);
        assert_eq!(
            sd.decisions["managed-host-kinds-in-the-fleet"]
                .values()
                .len(),
            1
        );
        assert!(matches!(sd.site.uuid_source, UuidSource::Setup));
        assert_eq!(sd.network.default_route, Some(DefaultRoute::Bgp));
        assert_eq!(
            sd.hosts.site_controllers[0].uplinks.as_ref().map(Vec::len),
            Some(2)
        );
        assert_eq!(
            sd.hosts.probe_hosts.as_ref().map(|p| p.host_inband.len()),
            Some(1)
        );
        assert!(matches!(
            sd.setup.env.get("REGISTRY_PULL_SECRET"),
            Some(EnvValue::Secret(SecretSource { from, .. })) if from == "env"
        ));
    }

    #[test]
    fn rendered_values_read_back_as_the_same_yaml() {
        for v in ["REST group", "foo: bar", "a # b", "no", "[x]", "Option 1"] {
            let text = format!("k: {}", DecisionValue::One(v.to_string()).render());
            let back: serde_yaml::Value = serde_yaml::from_str(&text).unwrap();
            assert_eq!(back["k"].as_str(), Some(v), "{text}");
        }
    }

    #[test]
    fn another_schema_version_is_rejected() {
        for v in ["0", "2"] {
            let text = SAMPLE.replacen("schemaVersion: 1", &format!("schemaVersion: {v}"), 1);
            let err = parse(&text).expect_err("rejected");
            assert!(format!("{err:#}").contains("schemaVersion"), "{err:#}");
        }
    }

    #[test]
    fn unknown_keys_and_bad_values_are_rejected() {
        let cases: &[(&str, &str, &str)] = &[
            (
                "unknown key",
                "  datacenter: dc1\n",
                "  datacenter: dc1\n  colour: red\n",
            ),
            ("uuid source", "uuidSource: setup", "uuidSource: random"),
            (
                "bad prefix",
                "forgeOverlayPrefixes: [10.10.64.0/22]",
                "forgeOverlayPrefixes: [10.10.64.0/33]",
            ),
        ];
        for (name, from, to) in cases {
            assert!(SAMPLE.contains(from), "{name}: fixture anchor missing");
            let text = SAMPLE.replacen(from, to, 1);
            assert!(parse(&text).is_err(), "{name}: should not parse");
        }
    }
}
