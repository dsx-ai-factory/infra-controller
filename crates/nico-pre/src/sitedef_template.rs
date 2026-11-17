/*
 * SPDX-FileCopyrightText: Copyright (c) 2025-2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *     http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

//! `nico-pre site-def --template`: a complete site definition to fill in, every key of
//! the schema in order with its meaning beside it, the `decisions` section from the
//! register, and `REQUIRED` where the provider has to supply a value. With an existing
//! site definition, its values are carried into the fresh template.

use std::fmt::Write as _;

use indexmap::IndexMap;
use serde_yaml::Value;

use crate::register::Register;
use crate::sitedef::DecisionValue;

/// The word a value to be supplied is written as; `validate` refuses a file that still
/// carries it and names the keys.
pub(crate) const REQUIRED: &str = "REQUIRED";

/// One line, or one key with its value, of the template.
enum Entry {
    /// A line written as is (a comment, or a key that opens a mapping the next entries
    /// fill in).
    Text(&'static str),
    /// A key with a value the provider supplies. `path` is the key's place in the file,
    /// `required` says whether it is written live or commented out until wanted; `sample`
    /// is a value of the right type, for the test that parses a filled template.
    Leaf {
        path: &'static [&'static str],
        comment: &'static str,
        required: bool,
        sample: &'static str,
    },
    /// A key written live with a conventional value the provider may keep: the collateral
    /// paths, which name files written later (`config.collaterals.present` checks they
    /// exist) and follow the helm-prereqs layout unless the site chooses otherwise.
    Preset {
        path: &'static [&'static str],
        comment: &'static str,
        value: &'static str,
    },
    /// A key whose value spans lines (a list, a table): the body is written below the
    /// key, each line indented one level further. Commented out when not required.
    Block {
        path: &'static [&'static str],
        comment: &'static str,
        required: bool,
        body: &'static str,
        sample: &'static str,
    },
}

use Entry::{Block, Leaf, Preset, Text};

/// The schema, in the order the file reads best. `SITE-DEF.md` has the full meaning of
/// each key; the comments here are the one line a provider needs while filling it in.
const TEMPLATE: &[Entry] = &[
    Text("# site-def.yaml: the statement of intent for one NICo site, read only by nico-pre."),
    Text(
        "# setup.sh, the ISO builder and nico-api never read it. Name secret sources, never values.",
    ),
    Text(
        "# REQUIRED marks a value you must supply; a commented key is optional, uncomment it to use it.",
    ),
    Text(
        "# Keys are documented in SITE-DEF.md; `nico-pre validate --site-def <this file>` checks it.",
    ),
    Text("schemaVersion: 1"),
    Text(""),
    Text("site:"),
    Leaf {
        path: &["site", "name"],
        comment: "the site's short name: letters, digits, `.`, `_`, `-`; as the TOML's `sitename`",
        required: true,
        sample: "site1",
    },
    Leaf {
        path: &["site", "datacenter"],
        comment: "the datacenter the site is in",
        required: true,
        sample: "dc1",
    },
    Leaf {
        path: &["site", "dnsZone"],
        comment: "the site's DNS zone, a DNS name",
        required: true,
        sample: "site1.dc1.example.net",
    },
    Leaf {
        path: &["site", "apiHostname"],
        comment: "the external name of the Core API, a DNS name that must resolve to the API VIP",
        required: true,
        sample: "api.site1.dc1.example.net",
    },
    Leaf {
        path: &["site", "uuidSource"],
        comment: "`setup` (setup.sh mints and seeds the site UUID) or the UUID itself, when the site already has one",
        required: true,
        sample: "setup",
    },
    Leaf {
        path: &["site", "description"],
        comment: "free text, never checked",
        required: false,
        sample: "first NICo site in dc1",
    },
    Text(""),
    Text(
        "collaterals:                       # the NICo configuration files, paths relative to this file",
    ),
    Leaf {
        path: &["collaterals", "cpDpuSiteYaml"],
        comment: "the CP-DPU ISO builder's site yaml; omit when the site controllers carry no DPU",
        required: false,
        sample: "./control-plane-dpu/site.yaml",
    },
    Preset {
        path: &["collaterals", "helmPrereqsValues"],
        comment: "helm-prereqs values.yaml; checked for at the config step",
        value: "./helm-prereqs/values.yaml",
    },
    Preset {
        path: &["collaterals", "coreValues"],
        comment: "the Core values file, which carries the NICo TOML; checked for at the config step",
        value: "./helm-prereqs/values/nico-core.yaml",
    },
    Preset {
        path: &["collaterals", "restValues"],
        comment: "the REST values file; checked for at the config step",
        value: "./helm-prereqs/values/nico-rest.yaml",
    },
    Preset {
        path: &["collaterals", "siteAgentValues"],
        comment: "the site-agent values file; checked for at the config step",
        value: "./helm-prereqs/values/nico-site-agent.yaml",
    },
    Leaf {
        path: &["collaterals", "metallbConfig"],
        comment: "the MetalLB manifest or kustomize directory given to setup.sh; omit when another tool renders MetalLB",
        required: false,
        sample: "./helm-prereqs/values/metallb-config.yaml",
    },
    Leaf {
        path: &["collaterals", "expectedMachines"],
        comment: "the expected-machines file; needed from step 7",
        required: false,
        sample: "./expected-machines.yaml",
    },
    Text(
        "results: ./results                 # where nico-pre writes its result files, relative to this file; the only path it writes",
    ),
    Text(""),
    Text("hosts:"),
    Block {
        path: &["hosts", "jumphost"],
        comment: "the jumphost the provider works from",
        required: false,
        body: "name: REQUIRED\naddress: REQUIRED",
        sample: "name: dc1-jump\naddress: 203.0.113.10",
    },
    Block {
        path: &["hosts", "stagingHost"],
        comment: "where setup.sh runs; `sshUser` optional",
        required: true,
        body: "address: REQUIRED\n# sshUser: REQUIRED",
        sample: "address: 10.10.0.5\nsshUser: nico",
    },
    Block {
        path: &["hosts", "buildHost"],
        comment: "where the ISO and images are built, when not the staging host",
        required: false,
        body: "address: REQUIRED",
        sample: "address: build.example.net",
    },
    Block {
        path: &["hosts", "registryHost"],
        comment: "the image registry the site pulls from; `caSource` names the CA, not a secret",
        required: false,
        body: "url: REQUIRED\n# caSource: file:./certs/registry-ca.pem",
        sample: "url: registry.site1.dc1.example.net\ncaSource: file:./certs/registry-ca.pem",
    },
    Leaf {
        path: &["hosts", "bmcProbeHost"],
        comment: "a host in the BMC segment that can run the probe container",
        required: false,
        sample: "10.10.250.20",
    },
    Block {
        path: &["hosts", "probeHosts"],
        comment: "vantage points for the fabric checks before the controllers are up",
        required: false,
        body: "admin: REQUIRED                  # a host on the admin segment\nhostInband:                      # one per hostInband segment, by its name\n  - { name: REQUIRED, address: REQUIRED }",
        sample: "admin: 10.10.60.250\nhostInband:\n  - { name: rack1, address: 10.10.72.250 }",
    },
    Block {
        path: &["hosts", "siteControllers"],
        comment: "one entry per site controller; `model` selects the HCL firmware pins",
        required: true,
        body: "- hostName: REQUIRED\n  hostSerial: REQUIRED\n  model: REQUIRED                # as the HCL names it, `Dell R760`\n  nodeId: REQUIRED               # 1, 2, 3: its ASN is bgpAsnStart + nodeId unless asnLayout says otherwise\n  bmc: REQUIRED                  # the host BMC address\n  # dpuBmc: REQUIRED             # the control-plane DPU's BMC\n  # p0Mac: \"aa:bb:cc:dd:ee:01\"\n  # dpuSsh: { address: REQUIRED, user: ubuntu }     # the DPU's OS over its OOB address, for the fabric checks\n  # uplinks: [{ port: p0, tor: REQUIRED, torPort: Ethernet1/1 }, { port: p1, tor: REQUIRED, torPort: Ethernet1/1 }]   # what LLDP should report\n  # torPeers: [{ address: REQUIRED, asn: REQUIRED }]   # plain-NIC controllers only: MetalLB peers with the ToRs",
        sample: "- hostName: sc1\n  hostSerial: ABC1231\n  model: Dell R760\n  nodeId: 1\n  bmc: 10.10.248.49\n  dpuBmc: 10.10.248.52\n  p0Mac: \"aa:bb:cc:dd:ee:01\"\n  dpuSsh: { address: 10.10.248.55, user: ubuntu }\n  uplinks: [{ port: p0, tor: dc1-tor-01, torPort: Ethernet1/1 }]\n  torPeers: [{ address: 10.10.248.1, asn: 65034 }]",
    },
    Text(""),
    Text("network:"),
    Leaf {
        path: &["network", "datacenterAsn"],
        comment: "the fabric's ASN; every route target carries it",
        required: true,
        sample: "65034",
    },
    Leaf {
        path: &["network", "bgpAsnStart"],
        comment: "the site's BGP ASN range: MetalLB advertises from the start, node i peers as start + i, the rest is the per-DPU pool",
        required: true,
        sample: "4244766860",
    },
    Leaf {
        path: &["network", "bgpAsnEnd"],
        comment: "end of that range, inclusive",
        required: true,
        sample: "4244767060",
    },
    Block {
        path: &["network", "asnLayout"],
        comment: "only for a site that did not follow the convention above",
        required: false,
        body: "siteControllerRoutes: REQUIRED   # the MetalLB ASN\nsiteControllers: { 1: REQUIRED }  # nodeId to ASN",
        sample: "siteControllerRoutes: 4268030000\nsiteControllers: { 1: 4268030002 }",
    },
    Leaf {
        path: &["network", "siteControllerMtuSize"],
        comment: "MTU of the site controller uplinks",
        required: true,
        sample: "9000",
    },
    Leaf {
        path: &["network", "forgeControlPlanePrefix"],
        comment: "the control-plane /31s to the DPUs",
        required: true,
        sample: "10.10.248.88/29",
    },
    Leaf {
        path: &["network", "forgeServiceVipPrefix"],
        comment: "the service VIP pool",
        required: true,
        sample: "10.10.126.192/27",
    },
    Leaf {
        path: &["network", "forgeDpuLoopbackPrefix"],
        comment: "loopbacks of the control-plane DPUs; managed DPUs draw from the TOML's [pools.lo-ip]",
        required: true,
        sample: "10.10.63.128/26",
    },
    Leaf {
        path: &["network", "forgeOverlayPrefixes"],
        comment: "tenant overlay space, a list",
        required: true,
        sample: "[10.10.64.0/22]",
    },
    Text(
        "  segments:                        # every other segment: prefix, gateway inside it, MTU",
    ),
    Block {
        path: &["network", "segments", "admin"],
        comment: "the admin segment the DPU-booting hosts take addresses from",
        required: true,
        body: "prefix: REQUIRED\ngateway: REQUIRED\nmtu: REQUIRED",
        sample: "prefix: 10.10.60.0/24\ngateway: 10.10.60.1\nmtu: 9000",
    },
    Block {
        path: &["network", "segments", "controlPlaneOob"],
        comment: "the site controllers' OOB segment",
        required: true,
        body: "prefix: REQUIRED\ngateway: REQUIRED\nmtu: REQUIRED",
        sample: "prefix: 10.10.248.48/28\ngateway: 10.10.248.62\nmtu: 1500",
    },
    Block {
        path: &["network", "segments", "managedHostBmc"],
        comment: "the managed hosts' BMC segments, one per switch",
        required: false,
        body: "- { switchName: REQUIRED, prefix: REQUIRED, gateway: REQUIRED, mtu: REQUIRED }",
        sample: "- { switchName: dc1-m01-ipmitor-01, prefix: 10.10.250.0/25, gateway: 10.10.250.1, mtu: 1500 }",
    },
    Block {
        path: &["network", "segments", "hostInband"],
        comment: "one per rack of hosts booting on HostInband (DPU policy nic or ignore)",
        required: false,
        body: "- { name: REQUIRED, prefix: REQUIRED, gateway: REQUIRED, mtu: REQUIRED }",
        sample: "- { name: rack1, prefix: 10.10.72.0/23, gateway: 10.10.72.1, mtu: 9000 }",
    },
    Block {
        path: &["network", "kubernetes"],
        comment: "the cluster's CIDRs and API VIP",
        required: true,
        body: "podCidr: REQUIRED\nserviceCidr: REQUIRED\napiVip: REQUIRED",
        sample: "podCidr: 10.233.64.0/18\nserviceCidr: 10.233.0.0/18\napiVip: 10.10.126.193",
    },
    Leaf {
        path: &["network", "nameServer"],
        comment: "the upstream resolver",
        required: true,
        sample: "10.10.0.11",
    },
    Leaf {
        path: &["network", "unboundForwardAddresses"],
        comment: "forwarders of the built-in unbound, a list",
        required: false,
        sample: "[10.10.0.11, 10.10.0.12]",
    },
    Leaf {
        path: &["network", "ntpServers"],
        comment: "site NTP, a list",
        required: true,
        sample: "[10.10.0.21, 10.10.0.22]",
    },
    Leaf {
        path: &["network", "pathMtu"],
        comment: "the path MTU every segment must respect",
        required: true,
        sample: "9000",
    },
    Leaf {
        path: &["network", "defaultRoute"],
        comment: "`bgp` (learnt from the ToRs) or `static`",
        required: false,
        sample: "bgp",
    },
    Text("  pools:"),
    Block {
        path: &["network", "pools", "vlan"],
        comment: "the VLAN id pool",
        required: true,
        body: "start: REQUIRED\nend: REQUIRED",
        sample: "start: 100\nend: 501",
    },
    Block {
        path: &["network", "pools", "vni"],
        comment: "the VNI pool",
        required: true,
        body: "start: REQUIRED\nend: REQUIRED",
        sample: "start: 1024500\nend: 1024800",
    },
    Block {
        path: &["network", "pools", "externalVpcVni"],
        comment: "the external VPC VNI pool",
        required: false,
        body: "start: REQUIRED\nend: REQUIRED",
        sample: "start: 51000\nend: 51008",
    },
    Block {
        path: &["network", "pools", "dpuLoopback"],
        comment: "the managed DPUs' loopback pool, the TOML's [pools.lo-ip]: a prefix, or `ranges: [{ start: ..., end: ... }]`; clear of the control-plane DPUs' loopbacks",
        required: false,
        body: "prefix: REQUIRED",
        sample: "prefix: 10.10.62.0/24",
    },
    Block {
        path: &["network", "fnn"],
        comment: "the FNN plan, when the site runs it",
        required: false,
        body: "controlPlaneVni: REQUIRED\n# adminVpcVni: REQUIRED\nvpcVrfLoopbackPrefix: REQUIRED\npoolsVpcVni: [{ start: REQUIRED, end: REQUIRED }]\nrouteTargets: { managedNodeBmc: 900, siteController: 50100, adminNetwork: 50400 }   # name to number",
        sample: "controlPlaneVni: 60020\nadminVpcVni: 60100\nvpcVrfLoopbackPrefix: 10.10.255.0/24\npoolsVpcVni: [{ start: 60101, end: 61124 }]\nrouteTargets: { managedNodeBmc: 900, siteController: 50100 }",
    },
    Text(
        "  # Pins of values the tool derives from the collaterals; a pinned value a collateral contradicts fails.",
    ),
    Leaf {
        path: &["network", "denyPrefixes"],
        comment: "the TOML's deny_prefixes, a list",
        required: false,
        sample: "[10.10.248.48/28, 10.10.250.0/25]",
    },
    Block {
        path: &["network", "serviceVips"],
        comment: "the VIP of every LoadBalancer Service setup.sh deploys, inside forgeServiceVipPrefix; one per pod for dns and ntp",
        required: true,
        body: "api: REQUIRED\ndhcp: REQUIRED\ndns: [REQUIRED]\nntp: [REQUIRED]\npxe: REQUIRED\nsshConsole: REQUIRED\nunbound: REQUIRED   # omit only with the resolver decision External DNS\n# dhcpV6: REQUIRED   # optional: the DHCPv6 relay VIP, when nico-dhcp.v6ExternalService is enabled",
        sample: "api: 10.10.126.208\ndhcp: 10.10.126.194\ndns: [10.10.126.195, 10.10.126.196]\nntp: [10.10.126.199, 10.10.126.200, 10.10.126.201]\npxe: 10.10.126.197\nsshConsole: 10.10.126.198\nunbound: 10.10.126.202",
    },
    Text(""),
    Block {
        path: &["counts"],
        comment: "what the prefixes, pools and Postgres are sized against",
        required: true,
        body: "hosts: REQUIRED\ndpus: REQUIRED\ntenants: REQUIRED\nracks: REQUIRED",
        sample: "hosts: 128\ndpus: 128\ntenants: 8\nracks: 4",
    },
    Text(""),
    Block {
        path: &["fleet"],
        comment: "what NICo will manage; selects BIOS profiles, firmware thresholds and seed entries",
        required: false,
        body: "hosts:\n  - { model: REQUIRED, gpu: REQUIRED, count: REQUIRED }\ndpu: { model: REQUIRED, generation: REQUIRED }   # generation BF3 | BF4",
        sample: "hosts:\n  - { model: Dell XE9680, gpu: H100, count: 128 }\ndpu: { model: BlueField-3 B3220, generation: BF3 }",
    },
    Text(""),
    Text("versions:"),
    Leaf {
        path: &["versions", "infraController"],
        comment: "release tag of the checkout and the Core image",
        required: true,
        sample: "v2026.10.15",
    },
    Leaf {
        path: &["versions", "nicoRest"],
        comment: "the REST release tag",
        required: true,
        sample: "v1.2.0",
    },
    Leaf {
        path: &["versions", "nvRms"],
        comment: "when RMS is installed",
        required: false,
        sample: "v0.8.0",
    },
    Leaf {
        path: &["versions", "bootArtifacts"],
        comment: "the tag of the boot-artifact images nico-pxe copies in, when they are not the Core tag",
        required: false,
        sample: "v2026.10.15",
    },
    Leaf {
        path: &["versions", "docaPlatform"],
        comment: "the DPF release, when DPF is installed",
        required: false,
        sample: "v26.4.0",
    },
    Leaf {
        path: &["versions", "bfb"],
        comment: "the BFB file name, BF3 fleets",
        required: false,
        sample: "bf-bundle-3.2.2-125_26.02_ubuntu-22.04_prod.bfb",
    },
    Leaf {
        path: &["versions", "doca"],
        comment: "the DOCA version on the DPUs",
        required: false,
        sample: "3.2.2",
    },
    Leaf {
        path: &["versions", "hbn"],
        comment: "the HBN version on the DPUs",
        required: false,
        sample: "3.2.2",
    },
    Leaf {
        path: &["versions", "kubernetesBaseline"],
        comment: "the Kubernetes version the cluster runs",
        required: true,
        sample: "v1.30.4",
    },
    Text(""),
    Block {
        path: &["credentials"],
        comment: "site-wide credentials by source only (`from: env|file|vault` with `name` or `path`); read from step 7",
        required: false,
        body: "bmcRoot: { from: REQUIRED, path: REQUIRED }   # the BMC root password NICo pushes to every BMC\n# uefi: { from: env, name: REQUIRED }",
        sample: "bmcRoot: { from: file, path: ~/.nico/site1/bmc-root }\nuefi: { from: env, name: NICO_SITE_UEFI_PASSWORD }",
    },
    Text(""),
    Text(
        "setup:                             # what you will run; secrets by source, never by value",
    ),
    Leaf {
        path: &["setup", "command"],
        comment: "the exact setup.sh command line, or the command of the tool that deploys the site",
        required: true,
        sample: "./setup.sh -y --core-values values/nico-core.yaml --skip-rms",
    },
    Block {
        path: &["setup", "env"],
        comment: "the variables setup.sh reads; a secret as `{ from: env }` or `{ from: file, path: ... }`",
        required: true,
        body: "# KUBECONFIG: ~/.kube/config   # optional: the kubeconfig the platform stage reads once the cluster is up; unset means kubectl's default on the staging host\nNICO_IMAGE_REGISTRY: REQUIRED\nNICO_CORE_IMAGE_TAG: REQUIRED\nNICO_REST_IMAGE_TAG: REQUIRED\n# REGISTRY_PULL_USERNAME: REQUIRED\n# REGISTRY_PULL_SECRET: { from: env }",
        sample: "KUBECONFIG: ~/.kube/site1\nNICO_IMAGE_REGISTRY: registry.site1.dc1.example.net/nico\nNICO_CORE_IMAGE_TAG: v2026.10.15\nNICO_REST_IMAGE_TAG: v1.2.0\nREGISTRY_PULL_SECRET: { from: env }",
    },
];

/// The template, with `existing`'s values carried into it where it has them and the
/// `decisions` section written from `decisions` and the register.
pub(crate) fn site_def(
    existing: Option<&Value>,
    decisions: &IndexMap<String, DecisionValue>,
    reg: &Register,
) -> String {
    render(existing, decisions, reg, false)
}

fn render(
    existing: Option<&Value>,
    decisions: &IndexMap<String, DecisionValue>,
    reg: &Register,
    samples: bool,
) -> String {
    let mut out = String::new();
    let mut decisions_written = false;
    for e in TEMPLATE {
        match e {
            Text(line) => {
                // A separator line after an entry's own blank line would double it.
                if line.is_empty() && out.ends_with("\n\n") {
                    continue;
                }
                let _ = writeln!(out, "{line}");
                // The decisions section follows `results`, as the example lays the file out.
                if line.starts_with("results:") && !decisions_written {
                    out.push('\n');
                    out.push_str(&crate::template::decisions_section(decisions, reg));
                    decisions_written = true;
                }
            }
            Leaf {
                path,
                comment,
                required,
                sample,
            } => {
                let indent = "  ".repeat(path.len() - 1);
                let key = path[path.len() - 1];
                match existing.and_then(|v| at(v, path)) {
                    Some(v) => {
                        let _ = writeln!(out, "{indent}{key}: {}   # {comment}", scalar(v));
                    }
                    None if samples => {
                        let _ = writeln!(out, "{indent}{key}: {sample}   # {comment}");
                    }
                    None if *required => {
                        let _ = writeln!(out, "{indent}{key}: {REQUIRED}   # {comment}");
                    }
                    None => {
                        let _ = writeln!(out, "{indent}# {key}: {sample}   # optional: {comment}");
                    }
                }
                out.push('\n');
            }
            Preset {
                path,
                comment,
                value,
            } => {
                let indent = "  ".repeat(path.len() - 1);
                let key = path[path.len() - 1];
                let shown = existing
                    .and_then(|v| at(v, path))
                    .map_or_else(|| (*value).to_string(), scalar);
                let _ = writeln!(out, "{indent}{key}: {shown}   # {comment}");
                out.push('\n');
            }
            Block {
                path,
                comment,
                required,
                body,
                sample,
            } => {
                let indent = "  ".repeat(path.len() - 1);
                let key = path[path.len() - 1];
                match existing.and_then(|v| at(v, path)) {
                    Some(v) => {
                        let _ = writeln!(out, "{indent}{key}:   # {comment}");
                        for line in block(v).lines() {
                            let _ = writeln!(out, "{indent}  {line}");
                        }
                    }
                    None if samples => {
                        let _ = writeln!(out, "{indent}{key}:   # {comment}");
                        for line in sample.lines() {
                            let _ = writeln!(out, "{indent}  {line}");
                        }
                    }
                    None if *required => {
                        let _ = writeln!(out, "{indent}{key}:   # {comment}");
                        for line in body.lines() {
                            let _ = writeln!(out, "{indent}  {line}");
                        }
                    }
                    None => {
                        let _ = writeln!(out, "{indent}# {key}:   # optional: {comment}");
                        for line in body.lines() {
                            let _ = writeln!(out, "{indent}#   {line}");
                        }
                    }
                }
                out.push('\n');
            }
        }
    }
    out
}

/// The value at `path` in a YAML tree, when every step is a mapping that has the key.
fn at<'a>(v: &'a Value, path: &[&str]) -> Option<&'a Value> {
    path.iter().try_fold(v, |cur, key| cur.get(key))
}

/// A value as YAML writes it on one line: a scalar as is, a list or table in flow style.
fn scalar(v: &Value) -> String {
    match v {
        Value::Sequence(items) => format!(
            "[{}]",
            items.iter().map(scalar).collect::<Vec<_>>().join(", ")
        ),
        Value::Mapping(m) => format!(
            "{{ {} }}",
            m.iter()
                .map(|(k, v)| format!("{}: {}", scalar(k), scalar(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        _ => serde_yaml::to_string(v)
            .unwrap_or_default()
            .trim_end()
            .to_string(),
    }
}

/// A list or table as YAML writes it in block style, no trailing newline.
fn block(v: &Value) -> String {
    serde_yaml::to_string(v)
        .unwrap_or_default()
        .trim_end()
        .to_string()
}

/// The values of a site definition still written as `REQUIRED`, each as the dotted path
/// of its key (`network.segments.admin.prefix`; list items without an index) with its
/// line number, in file order. Comments are not read.
pub(crate) fn unfilled(text: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    // The mapping keys open above the current line, by indentation.
    let mut stack: Vec<(usize, String)> = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let code = if line.trim_start().starts_with('#') {
            ""
        } else {
            line.find(" #").map_or(line, |p| &line[..p])
        };
        if code.trim().is_empty() {
            continue;
        }
        let indent = code.len() - code.trim_start().len();
        stack.retain(|(at, _)| *at < indent);
        let item = code.trim_start().strip_prefix("- ");
        let body = item.unwrap_or_else(|| code.trim_start());
        // `key:` opens a mapping; `key: value` is a leaf, read for placeholders (a flow
        // map's inner keys included). A list item's key sits one level inside the list's.
        let Some((key, value)) = body.split_once(':') else {
            continue;
        };
        if key.contains(['{', '[', ' ']) {
            // A flow map or list as a list item (`- { hostName: x, bmc: REQUIRED }`): its
            // placeholders belong to the keys inside it, under the list's key.
            let mut rest = code;
            while let Some(p) = rest.find(REQUIRED) {
                let before = &rest[..p];
                let after = &rest[p + REQUIRED.len()..];
                let ends = after.is_empty() || after.starts_with([' ', ',', '}', ']']);
                if ends && let Some(inner) = key_before(before) {
                    let mut path: Vec<&str> = stack.iter().map(|(_, k)| k.as_str()).collect();
                    path.push(inner);
                    out.push((i + 1, path.join(".")));
                }
                rest = after;
            }
            continue;
        }
        let key_indent = if item.is_some() { indent + 2 } else { indent };
        stack.push((key_indent, key.to_string()));
        if value.trim().is_empty() {
            continue;
        }
        let mut rest = code;
        while let Some(p) = rest.find(REQUIRED) {
            let before = &rest[..p];
            let after = &rest[p + REQUIRED.len()..];
            let ends = after.is_empty() || after.starts_with([' ', ',', '}', ']']);
            if ends && let Some(inner) = key_before(before) {
                let mut path: Vec<&str> = stack.iter().map(|(_, k)| k.as_str()).collect();
                if inner != key {
                    path.push(inner);
                }
                out.push((i + 1, path.join(".")));
            }
            rest = after;
        }
        stack.pop();
    }
    out
}

/// The key a value belongs to: the last `key:` before it on the line.
fn key_before(before: &str) -> Option<&str> {
    let trimmed = before.trim_end();
    let colon = trimmed.strip_suffix(':')?;
    let start = colon
        .rfind(|c: char| !(c.is_alphanumeric() || c == '_' || c == '-'))
        .map_or(0, |p| p + 1);
    let key = &colon[start..];
    (!key.is_empty()).then_some(key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sitedef::parse;

    const SAMPLE: &str = include_str!("../examples/site-def.yaml");

    /// Every key the template writes is one the parser accepts, with a value of the right
    /// type: the template filled with its own samples parses strictly, optional keys
    /// included.
    #[test]
    fn the_template_filled_with_its_samples_parses() {
        let sd = parse(SAMPLE).expect("sample parses");
        let text = render(None, &sd.decisions, Register::builtin(), true);
        let parsed = parse(&text).unwrap_or_else(|e| panic!("{e:#}\n{text}"));
        assert_eq!(parsed.hosts.site_controllers.len(), 1);
        assert!(parsed.fleet.is_some() && parsed.credentials.is_some());
    }

    /// The template carries every key the example has, so a provider misses nothing the
    /// example shows.
    #[test]
    fn the_template_names_every_key_of_the_example() {
        let example: Value = serde_yaml::from_str(SAMPLE).expect("sample is yaml");
        let mut missing = Vec::new();
        for path in top_level_paths(&example) {
            // Known when a template entry's path covers it (the entry itself, or a key
            // inside a block) or when it is a mapping some entry sits in.
            let known = TEMPLATE.iter().any(|e| match e {
                Leaf { path: p, .. } | Block { path: p, .. } | Preset { path: p, .. } => {
                    let shorter = p.len().min(path.len());
                    p[..shorter] == path[..shorter]
                }
                Text(_) => false,
            });
            if !known && path != ["schemaVersion"] && path != ["results"] && path[0] != "decisions"
            {
                missing.push(path.join("."));
            }
        }
        assert!(missing.is_empty(), "{missing:?}");
    }

    /// Key paths of the mappings in a YAML tree, down to the leaves.
    fn top_level_paths(v: &Value) -> Vec<Vec<String>> {
        fn walk(v: &Value, prefix: &[String], out: &mut Vec<Vec<String>>) {
            if let Some(m) = v.as_mapping() {
                for (k, child) in m {
                    let mut p = prefix.to_vec();
                    p.push(k.as_str().unwrap_or_default().to_string());
                    out.push(p.clone());
                    walk(child, &p, out);
                }
            }
        }
        let mut out = Vec::new();
        walk(v, &[], &mut out);
        out
    }

    /// A fresh template says REQUIRED where values are due, and `unfilled` finds each one
    /// by key.
    #[test]
    fn a_fresh_template_is_refused_until_filled() {
        let text = render(None, &IndexMap::new(), Register::builtin(), false);
        let found = unfilled(&text);
        let keys: Vec<&str> = found.iter().map(|(_, k)| k.as_str()).collect();
        for key in [
            "site.name",
            "network.datacenterAsn",
            "network.forgeServiceVipPrefix",
            "network.segments.admin.prefix",
            "hosts.siteControllers.hostName",
            "setup.env.NICO_IMAGE_REGISTRY",
            "setup.command",
        ] {
            assert!(keys.contains(&key), "{key} not reported in {keys:?}");
        }
        // A commented placeholder is not a value.
        assert!(!keys.iter().any(|k| k.ends_with("description")), "{keys:?}");
        assert!(parse(&text).is_err(), "an unfilled template must not parse");
    }

    /// An existing site definition's values are carried into the fresh template, block
    /// values included, and the result parses to the same definition.
    #[test]
    fn an_existing_definition_is_carried_into_the_template() {
        let existing: Value = serde_yaml::from_str(SAMPLE).expect("sample is yaml");
        let sd = parse(SAMPLE).expect("sample parses");
        let text = site_def(Some(&existing), &sd.decisions, Register::builtin());
        assert!(unfilled(&text).is_empty(), "{:?}", unfilled(&text));
        let carried = parse(&text).unwrap_or_else(|e| panic!("{e:#}\n{text}"));
        assert_eq!(carried.site.name, sd.site.name);
        assert_eq!(carried.hosts.site_controllers.len(), 3);
        assert_eq!(carried.network.bgp_asn_end, sd.network.bgp_asn_end);
        assert_eq!(carried.setup.env.len(), sd.setup.env.len());
        assert!(carried.network.fnn.is_some());
    }

    #[test]
    fn unfilled_reads_flow_maps_and_list_items() {
        let text = "a:\n  b: { prefix: REQUIRED, gateway: 1.2.3.4 }\n  c:\n    - name: REQUIRED   # REQUIRED in a comment\n    - { name: x, bmc: REQUIRED }\n  # d: REQUIRED\n  e: REQUIRED_NOT\n";
        let found = unfilled(text);
        assert_eq!(
            found,
            vec![
                (2, "a.b.prefix".to_string()),
                (4, "a.c.name".to_string()),
                (5, "a.c.bmc".to_string())
            ]
        );
    }
}
