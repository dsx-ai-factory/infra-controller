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

//! Loaders for the collaterals: the configuration files the site definition names under
//! `collaterals`, read the way NICo's own tooling reads them. A loader returns an error for a
//! file that is missing or does not parse; the harness turns that into the row of the check
//! that asked, and `config.collaterals.present` is the row that reports absence on purpose.

use std::net::IpAddr;
use std::path::{Path, PathBuf};

use eyre::{Context as _, ContextCompat as _};
use ipnet::IpNet;
use serde::Deserialize;

use crate::harness::Ctx;

/// The Core values file: the umbrella chart's values carrying the NICo TOML.
pub(crate) struct CoreValues {
    pub(crate) path: PathBuf,
    /// The TOML under `nico-api.siteConfig.nicoApiSiteConfig`, parsed.
    pub(crate) toml: toml::Table,
    /// Every `loadBalancerIPs` annotation (either spelling), by the values path that
    /// carries it (`nico-dhcp.externalService`), one entry per address.
    pub(crate) vips: Vec<(String, IpAddr)>,
    /// Every `metallb.universe.tf/address-pool` annotation, by the values path that
    /// carries it: the pool the Service asks to be served from.
    pub(crate) vip_pools: Vec<(String, String)>,
    /// `loadBalancerIPs` annotations MetalLB would refuse or not read, each with its values
    /// path: the MetalLB row's finding, so the other readers of the file still load it.
    pub(crate) vip_errors: Vec<String>,
    /// The values file as written, for the rules setup.sh applies to its text.
    pub(crate) values_text: String,
    /// The same file as a tree, for the keys beside the TOML, parsed from the same text.
    pub(crate) tree: Values,
}

pub(crate) fn core_values(ctx: &Ctx) -> eyre::Result<CoreValues> {
    let path = ctx.resolve(&ctx.sd.collaterals.core_values);
    let values_text = std::fs::read_to_string(&path)
        .wrap_err_with(|| format!("{}: not readable", path.display()))?;
    // Parsed from the same text the setup rules scan, so the two never disagree.
    let values: serde_yaml::Value = parse_yaml(&path, &values_text)?;
    let toml = core_toml_of(&path, &values)?;
    let mut vips = Vec::new();
    let mut bad = Vec::new();
    collect_vips(&values, String::new(), &mut vips, &mut bad);
    let mut vip_pools = Vec::new();
    collect_pool_requests(&values, String::new(), &mut vip_pools);
    Ok(CoreValues {
        tree: Values {
            path: path.clone(),
            root: values,
        },
        path,
        toml,
        vips,
        vip_pools,
        vip_errors: bad,
        values_text,
    })
}

/// The NICo TOML alone, for a check that compares nothing about the VIPs: a VIP
/// annotation MetalLB would refuse is that check's own row, not a reason to say nothing
/// about the TOML.
pub(crate) fn core_toml(ctx: &Ctx) -> eyre::Result<(PathBuf, toml::Table)> {
    let path = ctx.resolve(&ctx.sd.collaterals.core_values);
    let values = read_yaml(&path)?;
    let toml = core_toml_of(&path, &values)?;
    Ok((path, toml))
}

/// The NICo TOML inside the Core values, the global file merged under the site's.
fn core_toml_of(path: &Path, values: &serde_yaml::Value) -> eyre::Result<toml::Table> {
    // setup.sh hands the file to the umbrella chart, which nests the nico-api subchart's
    // values under `nico-api`. Helm does not hand a top-level `siteConfig` to the subchart:
    // one at the top is the subchart's own layout, whose TOML this deployment never reads.
    let layout = "nico-api.siteConfig.nicoApiSiteConfig";
    let Some(root) = values.get("nico-api") else {
        if values.get("siteConfig").is_some() {
            eyre::bail!(
                "{}: siteConfig is at the top level, the nico-api chart's own layout; setup.sh installs the umbrella chart, which reads the subchart's values under `nico-api:`, so this TOML would not be deployed",
                path.display()
            );
        }
        eyre::bail!("{}: no TOML under {layout}", path.display());
    };
    // The chart renders the TOML only while `siteConfig.enabled` is true (its default).
    let site_config = root.get("siteConfig");
    if site_config
        .and_then(|c| c.get("enabled"))
        .and_then(serde_yaml::Value::as_bool)
        == Some(false)
    {
        eyre::bail!(
            "{}: siteConfig.enabled is false, so the NICo TOML is not deployed",
            path.display()
        );
    }
    let text = site_config
        .and_then(|c| c.get("nicoApiSiteConfig"))
        .and_then(serde_yaml::Value::as_str)
        .wrap_err_with(|| format!("{}: no TOML under {layout}", path.display()))?;
    // nico-api merges the site TOML over the global one, key by key: `configFiles.nicoApiConfig`
    // when the values carry one (the chart's built-in file has no site tables), then the site.
    let mut toml = toml::Table::new();
    if let Some(global) = root
        .get("configFiles")
        .and_then(|c| c.get("nicoApiConfig"))
        .and_then(serde_yaml::Value::as_str)
        .filter(|t| !t.trim().is_empty())
    {
        let parsed = parse_toml(path, "global ", global)?;
        merge_toml(&mut toml, parsed);
    }
    let site = parse_toml(path, "", text)?;
    merge_toml(&mut toml, site);
    Ok(toml)
}

/// `from` merged over `into` as nico-api merges its global and site TOML: tables key by
/// key, any other value replaced.
pub(crate) fn merge_toml(into: &mut toml::Table, from: toml::Table) {
    for (key, value) in from {
        match (into.get_mut(&key), value) {
            (Some(toml::Value::Table(a)), toml::Value::Table(b)) => merge_toml(a, b),
            (_, value) => {
                into.insert(key, value);
            }
        }
    }
}

/// The annotation that pins a Service's VIPs, first in the spelling MetalLB 0.14.5, the
/// release setup.sh installs, reads. The `metallb.io` spelling arrived with MetalLB 0.14.9
/// and pins nothing on the installed release.
pub(crate) const LB_ANNOTATIONS: [&str; 2] = [
    "metallb.universe.tf/loadBalancerIPs",
    "metallb.io/loadBalancerIPs",
];

/// Every `loadBalancerIPs` annotation, one entry per address; an entry that is not an
/// address goes to `bad` with its path, since MetalLB would refuse the whole request.
/// The umbrella chart's conditional subcharts with the default of their `enabled` condition;
/// every other subchart deploys whatever a top-level `enabled` says.
const CONDITIONAL_CHARTS: &[(&str, bool)] = &[
    ("nico-dsx-exchange-consumer", true),
    ("nico-ntp", true),
    ("unbound", false),
    ("nico-machine-a-tron", false),
    ("nico-postgres-exporter", false),
];

/// Whether a mapping is a block the deployment leaves unrendered: a conditional subchart
/// whose `enabled` (or the umbrella chart's default for it) is false, or an
/// `externalService` / `v6ExternalService` block without `enabled: true`, since every chart
/// defaults that key to false. Any other `enabled` changes nothing Helm renders.
fn service_off(m: &serde_yaml::Mapping, at: &str) -> bool {
    let enabled = m.get("enabled").and_then(serde_yaml::Value::as_bool);
    if !at.contains('.') && !at.contains('[') {
        return CONDITIONAL_CHARTS
            .iter()
            .any(|(chart, default)| *chart == at && !enabled.unwrap_or(*default));
    }
    let service_block = at
        .rsplit('.')
        .next()
        .is_some_and(|k| k == "externalService" || k == "v6ExternalService");
    service_block && enabled != Some(true)
}

fn collect_vips(
    v: &serde_yaml::Value,
    at: String,
    out: &mut Vec<(String, IpAddr)>,
    bad: &mut Vec<String>,
) {
    match v {
        serde_yaml::Value::Mapping(m) => {
            // A Service switched off is not rendered, so MetalLB never reads its annotation;
            // one of another type than LoadBalancer is rendered but never allocated a VIP.
            if service_off(m, &at)
                || m.get("type")
                    .and_then(serde_yaml::Value::as_str)
                    .is_some_and(|t| t != "LoadBalancer")
            {
                return;
            }
            for (k, v) in m {
                let Some(key) = k.as_str() else { continue };
                // The installed MetalLB reads one spelling; the other pins nothing, which
                // is a defect unless the read spelling is there too.
                if key == LB_ANNOTATIONS[1] {
                    // Empty, it is the charts' placeholder in either spelling: no VIP.
                    // (Whitespace alone is a request MetalLB trims and refuses.)
                    if v.as_str().is_some_and(str::is_empty) {
                        continue;
                    }
                    if !m.contains_key(LB_ANNOTATIONS[0]) {
                        bad.push(format!(
                            "{} ({key} is not read by MetalLB 0.14.5; spell it {})",
                            at.trim_end_matches(".annotations"),
                            LB_ANNOTATIONS[0]
                        ));
                    }
                    continue;
                }
                if key == LB_ANNOTATIONS[0] {
                    let here = at.trim_end_matches(".annotations").to_string();
                    let Some(text) = v.as_str() else {
                        bad.push(format!("{here} (not a string)"));
                        continue;
                    };
                    // MetalLB parses every comma-separated entry and rejects an empty one
                    // (`10.0.0.1,`); an entirely empty value is the charts' placeholder.
                    if !text.is_empty() && text.split(',').any(|s| s.trim().is_empty()) {
                        bad.push(format!("{here} (an empty entry in `{text}`)"));
                        continue;
                    }
                    let mut ips: Vec<IpAddr> = Vec::new();
                    let mut unparsed = false;
                    for ip in text.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                        match ip.parse() {
                            Ok(ip) => ips.push(ip),
                            Err(_) => {
                                unparsed = true;
                                bad.push(format!("{here} `{ip}`"));
                            }
                        }
                    }
                    // One address per family: MetalLB refuses a request for two of one.
                    let v4 = ips.iter().filter(|ip| ip.is_ipv4()).count();
                    if !unparsed && (v4 > 1 || ips.len() - v4 > 1) {
                        bad.push(format!(
                            "{here} (`{text}` asks for two addresses of one family; one per family at most)"
                        ));
                        continue;
                    }
                    out.extend(ips.into_iter().map(|ip| (here.clone(), ip)));
                } else {
                    let next = if at.is_empty() {
                        key.to_string()
                    } else {
                        format!("{at}.{key}")
                    };
                    collect_vips(v, next, out, bad);
                }
            }
        }
        serde_yaml::Value::Sequence(items) => {
            for (i, item) in items.iter().enumerate() {
                collect_vips(item, format!("{at}[{i}]"), out, bad);
            }
        }
        _ => {}
    }
}

/// Every `metallb.universe.tf/address-pool` annotation on a Service MetalLB would serve, by
/// the values path that carries it, as `collect_vips` walks the same tree.
fn collect_pool_requests(v: &serde_yaml::Value, at: String, out: &mut Vec<(String, String)>) {
    match v {
        serde_yaml::Value::Mapping(m) => {
            if service_off(m, &at)
                || m.get("type")
                    .and_then(serde_yaml::Value::as_str)
                    .is_some_and(|t| t != "LoadBalancer")
            {
                return;
            }
            for (k, v) in m {
                let Some(key) = k.as_str() else { continue };
                if key == "metallb.universe.tf/address-pool" {
                    // MetalLB 0.14.5 applies the pool constraint only when the annotation
                    // is non-empty; an empty one is no request.
                    if let Some(pool) = v.as_str().filter(|p| !p.is_empty()) {
                        out.push((
                            at.trim_end_matches(".annotations").to_string(),
                            pool.to_string(),
                        ));
                    }
                    continue;
                }
                let next = if at.is_empty() {
                    key.to_string()
                } else {
                    format!("{at}.{key}")
                };
                collect_pool_requests(v, next, out);
            }
        }
        serde_yaml::Value::Sequence(items) => {
            for (i, item) in items.iter().enumerate() {
                collect_pool_requests(item, format!("{at}[{i}]"), out);
            }
        }
        _ => {}
    }
}

/// The MetalLB site configuration: a multi-document YAML of `IPAddressPool`, `BGPPeer`,
/// `BGPAdvertisement` and `L2Advertisement` objects.
pub(crate) struct MetallbConfig {
    pub(crate) path: PathBuf,
    pub(crate) pools: Vec<Pool>,
    pub(crate) peers: Vec<Peer>,
    /// The `BGPAdvertisement`s.
    pub(crate) advertisements: Vec<Advertisement>,
    /// The `L2Advertisement`s; MetalLB announces a pool over L2 only when one covers it.
    pub(crate) l2_advertisements: Vec<Advertisement>,
}

/// A `BGPAdvertisement`: without it MetalLB announces nothing over BGP.
pub(crate) struct Advertisement {
    pub(crate) name: String,
    /// `spec.ipAddressPools`; `None` means every pool.
    pub(crate) pools: Option<Vec<String>>,
    /// `spec.ipAddressPoolSelectors` present: which pools it adds is not evaluated here.
    pub(crate) has_selectors: bool,
    /// `spec.nodeSelectors` or `spec.peers` present: from which nodes, or to which peers,
    /// it announces is not evaluated here, so its coverage of any pool is unknown.
    pub(crate) restricted: bool,
}

/// What an advertisement says about one pool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Coverage {
    /// Named in `ipAddressPools`, or every pool because the advertisement names none and
    /// has no selectors.
    Named,
    /// Selected by label, which this tool does not evaluate.
    Unknown,
    No,
}

impl Advertisement {
    pub(crate) fn covers(&self, pool: &str) -> Coverage {
        // Whether the advertisement can select the pool at all comes first: one that
        // names other pools says nothing about this one, restricted or not.
        let eligible = match &self.pools {
            Some(list) if list.iter().any(|n| n == pool) => Coverage::Named,
            None if !self.has_selectors => Coverage::Named,
            _ if self.has_selectors => Coverage::Unknown,
            _ => return Coverage::No,
        };
        if self.restricted {
            Coverage::Unknown
        } else {
            eligible
        }
    }
}

pub(crate) struct Pool {
    pub(crate) name: String,
    pub(crate) addresses: Vec<Addresses>,
    /// `spec.avoidBuggyIPs`: MetalLB then withholds IPv4 addresses ending in .0 or .255.
    pub(crate) avoid_buggy_ips: bool,
    /// `spec.serviceAllocation` is stated: MetalLB then serves the pool to the namespaces
    /// and Services it selects alone, which the checks do not evaluate.
    pub(crate) service_allocation: bool,
}

impl Pool {
    /// Whether `ip` lies in the pool's spans, withheld or not.
    pub(crate) fn contains(&self, ip: IpAddr) -> bool {
        self.addresses.iter().any(|a| a.contains(ip))
    }

    /// Whether MetalLB hands `ip` out from this pool: in a span and not withheld.
    pub(crate) fn hands_out(&self, ip: IpAddr) -> bool {
        let buggy = match ip {
            IpAddr::V4(v4) => matches!(v4.octets()[3], 0 | 255),
            IpAddr::V6(_) => false,
        };
        self.contains(ip) && !(self.avoid_buggy_ips && buggy)
    }
}

/// One entry of `IPAddressPool.spec.addresses`: a CIDR, or `first-last`.
pub(crate) enum Addresses {
    Net(IpNet),
    Range(IpAddr, IpAddr),
}

impl std::fmt::Display for Addresses {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Addresses::Net(n) => write!(f, "{n}"),
            Addresses::Range(a, b) => write!(f, "{a}-{b}"),
        }
    }
}

impl Addresses {
    /// The entry as a closed interval over one family's address space.
    fn span(&self) -> (bool, u128, u128) {
        let key = |ip: IpAddr| match ip {
            IpAddr::V4(v4) => u128::from(u32::from(v4)),
            IpAddr::V6(v6) => u128::from(v6),
        };
        match self {
            Addresses::Net(n) => (
                matches!(n, IpNet::V4(_)),
                key(n.network()),
                key(n.broadcast()),
            ),
            Addresses::Range(a, b) => (a.is_ipv4(), key(*a), key(*b)),
        }
    }

    /// Whether the two entries share an address; adjacent entries do not.
    pub(crate) fn intersects(&self, other: &Addresses) -> bool {
        let (f1, a1, b1) = self.span();
        let (f2, a2, b2) = other.span();
        f1 == f2 && a1.max(a2) <= b1.min(b2)
    }

    fn parse(text: &str) -> eyre::Result<Self> {
        if let Some((a, b)) = text.split_once('-') {
            let (a, b): (IpAddr, IpAddr) = (a.trim().parse()?, b.trim().parse()?);
            // MetalLB refuses a range whose first address follows its last; equal ends are
            // one address.
            if a.is_ipv4() != b.is_ipv4() || a > b {
                eyre::bail!("range {text} is reversed or mixes families");
            }
            Ok(Addresses::Range(a, b))
        } else {
            Ok(Addresses::Net(text.trim().parse()?))
        }
    }

    fn contains(&self, ip: IpAddr) -> bool {
        match self {
            Addresses::Net(n) => n.contains(&ip),
            Addresses::Range(a, b) => {
                if a.is_ipv4() != ip.is_ipv4() || b.is_ipv4() != ip.is_ipv4() {
                    return false;
                }
                let key = |ip: IpAddr| match ip {
                    IpAddr::V4(v4) => u128::from(u32::from(v4)),
                    IpAddr::V6(v6) => u128::from(v6),
                };
                key(*a) <= key(ip) && key(ip) <= key(*b)
            }
        }
    }
}

pub(crate) struct Peer {
    pub(crate) name: String,
    pub(crate) my_asn: u32,
    pub(crate) peer_asn: u32,
    pub(crate) peer_address: IpAddr,
    /// Hostnames the `nodeSelectors` pin the peer to; empty means every node.
    pub(crate) hostnames: Vec<String>,
    /// Selector forms this tool does not evaluate (`NotIn`, `Exists`, other keys);
    /// a check reports them rather than guessing which nodes they select.
    pub(crate) unsupported_selectors: Vec<String>,
}

#[derive(Deserialize)]
struct Object {
    #[serde(rename = "apiVersion", default)]
    api_version: Option<String>,
    kind: String,
    #[serde(default)]
    metadata: Metadata,
    #[serde(default)]
    spec: serde_yaml::Value,
}

/// The API versions MetalLB serves each kind under; another version cannot be applied.
fn metallb_versions(kind: &str) -> &'static [&'static str] {
    match kind {
        "BGPPeer" => &["metallb.io/v1beta2", "metallb.io/v1beta1"],
        _ => &["metallb.io/v1beta1"],
    }
}

#[derive(Deserialize, Default)]
struct Metadata {
    #[serde(default)]
    name: String,
    #[serde(default)]
    namespace: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PeerSpec {
    #[serde(rename = "myASN")]
    my_asn: u32,
    #[serde(rename = "peerASN")]
    peer_asn: u32,
    peer_address: IpAddr,
    #[serde(default)]
    node_selectors: Vec<serde_yaml::Value>,
}

/// `None` when the site definition names no `metallbConfig`: another tool renders MetalLB.
pub(crate) fn metallb_config(ctx: &Ctx) -> eyre::Result<Option<MetallbConfig>> {
    let Some(rel) = &ctx.sd.collaterals.metallb_config else {
        return Ok(None);
    };
    let path = ctx.resolve(rel);
    let mut cfg = MetallbConfig {
        path: path.clone(),
        pools: Vec::new(),
        peers: Vec::new(),
        advertisements: Vec::new(),
        l2_advertisements: Vec::new(),
    };
    for path in metallb_files(&path)? {
        let text = std::fs::read_to_string(&path)
            .wrap_err_with(|| format!("reading {}", path.display()))?;
        for doc in serde_yaml::Deserializer::from_str(&text) {
            let value = serde_yaml::Value::deserialize(doc)
                .wrap_err_with(|| format!("parsing {}", path.display()))?;
            if value.is_null() {
                continue;
            }
            // `kubectl apply` takes a `kind: List` too; its items are documents of their own.
            let items = match value.get("kind").and_then(serde_yaml::Value::as_str) {
                Some("List") => value
                    .get("items")
                    .and_then(serde_yaml::Value::as_sequence)
                    .cloned()
                    .unwrap_or_default(),
                _ => vec![value],
            };
            for value in items {
                let obj: Object = serde_yaml::from_value(value)
                    .wrap_err_with(|| format!("{}: a document has no kind", path.display()))?;
                if matches!(
                    obj.kind.as_str(),
                    "IPAddressPool" | "BGPPeer" | "BGPAdvertisement" | "L2Advertisement"
                ) && !obj
                    .api_version
                    .as_deref()
                    .is_some_and(|v| metallb_versions(&obj.kind).contains(&v))
                {
                    eyre::bail!(
                        "{}: {} {} has apiVersion {}, MetalLB serves it as {}",
                        path.display(),
                        obj.kind,
                        obj.metadata.name,
                        obj.api_version.as_deref().unwrap_or("(absent)"),
                        metallb_versions(&obj.kind).join(" or ")
                    );
                }
                // setup.sh installs MetalLB in metallb-system, the only namespace its
                // controller reads; a resource stating another one is never seen. One
                // stating none takes the namespace kustomize or `kubectl -n` gives it.
                if let Some(ns) = obj.metadata.namespace.as_deref()
                    && matches!(
                        obj.kind.as_str(),
                        "IPAddressPool" | "BGPPeer" | "BGPAdvertisement" | "L2Advertisement"
                    )
                    && ns != "metallb-system"
                {
                    eyre::bail!(
                        "{}: {} {} is in namespace {ns}; MetalLB reads metallb-system only",
                        path.display(),
                        obj.kind,
                        obj.metadata.name
                    );
                }
                match obj.kind.as_str() {
                    "IPAddressPool" => {
                        let listed: Vec<String> = obj
                            .spec
                            .get("addresses")
                            .cloned()
                            .map(serde_yaml::from_value)
                            .transpose()?
                            .unwrap_or_default();
                        let addresses = listed
                            .iter()
                            .map(|a| Addresses::parse(a))
                            .collect::<eyre::Result<Vec<_>>>()
                            .wrap_err_with(|| {
                                format!(
                                    "{}: IPAddressPool {} addresses",
                                    path.display(),
                                    obj.metadata.name
                                )
                            })?;
                        // setup.sh's preflight refuses a pool with nothing to hand out.
                        eyre::ensure!(
                            !addresses.is_empty(),
                            "{}: IPAddressPool {} has no addresses",
                            path.display(),
                            obj.metadata.name
                        );
                        let avoid_buggy_ips = obj
                            .spec
                            .get("avoidBuggyIPs")
                            .and_then(serde_yaml::Value::as_bool)
                            .unwrap_or(false);
                        let service_allocation = obj
                            .spec
                            .get("serviceAllocation")
                            .is_some_and(|v| !v.is_null());
                        // Two documents of one kind and name are one Kubernetes object:
                        // the second replaces the first when applied, so a VIP in the
                        // first range would pass here and stay unassigned.
                        eyre::ensure!(
                            !cfg.pools.iter().any(|p| p.name == obj.metadata.name),
                            "{}: IPAddressPool {} is defined twice",
                            path.display(),
                            obj.metadata.name
                        );
                        cfg.pools.push(Pool {
                            name: obj.metadata.name,
                            addresses,
                            avoid_buggy_ips,
                            service_allocation,
                        });
                    }
                    "BGPPeer" => {
                        let mut unsupported = Vec::new();
                        let spec: PeerSpec =
                            serde_yaml::from_value(obj.spec).wrap_err_with(|| {
                                format!("{}: BGPPeer {}", path.display(), obj.metadata.name)
                            })?;
                        // Two documents of one kind and name are one object: the second
                        // replaces the first when applied.
                        eyre::ensure!(
                            !cfg.peers.iter().any(|p| p.name == obj.metadata.name),
                            "{}: BGPPeer {} is defined twice",
                            path.display(),
                            obj.metadata.name
                        );
                        cfg.peers.push(Peer {
                            name: obj.metadata.name,
                            my_asn: spec.my_asn,
                            peer_asn: spec.peer_asn,
                            peer_address: spec.peer_address,
                            hostnames: selector_hostnames(&spec.node_selectors, &mut unsupported),
                            unsupported_selectors: unsupported,
                        });
                    }
                    kind @ ("BGPAdvertisement" | "L2Advertisement") => {
                        let pools: Option<Vec<String>> = obj
                            .spec
                            .get("ipAddressPools")
                            .cloned()
                            .map(serde_yaml::from_value)
                            .transpose()
                            .wrap_err_with(|| {
                                format!(
                                    "{}: {kind} {} ipAddressPools",
                                    path.display(),
                                    obj.metadata.name
                                )
                            })?;
                        let list = if kind == "BGPAdvertisement" {
                            &mut cfg.advertisements
                        } else {
                            &mut cfg.l2_advertisements
                        };
                        // Two documents of one kind and name are one object: the second
                        // replaces the first when applied, so coverage the first states
                        // would be read here and gone from the cluster.
                        eyre::ensure!(
                            !list.iter().any(|a| a.name == obj.metadata.name),
                            "{}: {kind} {} is defined twice",
                            path.display(),
                            obj.metadata.name
                        );
                        list.push(Advertisement {
                            name: obj.metadata.name,
                            pools: pools.filter(|p| !p.is_empty()),
                            // An empty list is the same as none: MetalLB then applies the
                            // advertisement to every pool.
                            has_selectors: obj
                                .spec
                                .get("ipAddressPoolSelectors")
                                .and_then(serde_yaml::Value::as_sequence)
                                .is_some_and(|v| !v.is_empty()),
                            // `interfaces` narrows an L2 announcement to named interfaces,
                            // which a node may lack: coverage is then not evaluated.
                            restricted: ["nodeSelectors", "peers", "interfaces"]
                                .iter()
                                .filter(|k| **k != "interfaces" || kind == "L2Advertisement")
                                .any(|k| {
                                    obj.spec
                                        .get(k)
                                        .and_then(serde_yaml::Value::as_sequence)
                                        .is_some_and(|v| !v.is_empty())
                                }),
                        });
                    }
                    _ => {}
                }
            }
        }
    }
    Ok(Some(cfg))
}

/// The manifest files a `metallbConfig` names: the file itself, or the plain `resources`
/// of a kustomize directory, which `setup.sh --metallb-config` accepts too. A kustomization
/// that transforms its resources (patches, generators, name prefixes) has to be rendered
/// first (`kubectl kustomize <dir> > <file>`) and the output named instead.
fn metallb_files(path: &Path) -> eyre::Result<Vec<PathBuf>> {
    if !path.is_dir() {
        return Ok(vec![path.to_path_buf()]);
    }
    let kustomization = ["kustomization.yaml", "kustomization.yml", "Kustomization"]
        .iter()
        .map(|f| path.join(f))
        .find(|p| p.is_file())
        .ok_or_else(|| {
            eyre::eyre!(
                "{}: a directory without a kustomization file",
                path.display()
            )
        })?;
    let k: serde_yaml::Mapping = serde_yaml::from_value(read_yaml(&kustomization)?)
        .wrap_err_with(|| format!("parsing {}", kustomization.display()))?;
    let mut files = Vec::new();
    for (key, value) in &k {
        match key.as_str().unwrap_or_default() {
            "resources" => {
                for r in value.as_sequence().into_iter().flatten() {
                    let Some(r) = r.as_str() else {
                        eyre::bail!(
                            "{}: a resource entry is not a string",
                            kustomization.display()
                        );
                    };
                    let file = path.join(r);
                    // kubectl's default load restrictor refuses a resource outside the
                    // kustomization directory, so the apply would fail where this passes.
                    let inside = match (file.canonicalize(), path.canonicalize()) {
                        (Ok(f), Ok(d)) => f.starts_with(d),
                        _ => false,
                    };
                    if r.contains("://") || !file.is_file() || !inside {
                        eyre::bail!(
                            "{}: resource {r} is not a file in the directory; render the kustomization with `kubectl kustomize` and name the output",
                            kustomization.display()
                        );
                    }
                    files.push(file);
                }
            }
            // Keys that change nothing in what the resources say about pools, peers and
            // advertisements: generator settings without generators, and build annotations.
            "apiVersion" | "kind" | "metadata" | "commonLabels" | "labels"
            | "commonAnnotations" | "generatorOptions" | "buildMetadata" => {}
            // `namespace` moves every resource, so the pools and peers land where the raw
            // files do not say.
            "namespace" => {
                eyre::bail!(
                    "{}: `namespace` renames every resource's namespace; render the kustomization with `kubectl kustomize` and name the output",
                    kustomization.display()
                );
            }
            other => {
                eyre::bail!(
                    "{}: `{other}` cannot be read directly; render the kustomization with `kubectl kustomize` and name the output",
                    kustomization.display()
                );
            }
        }
    }
    Ok(files)
}

/// The hostnames a MetalLB `nodeSelectors` list can select: within one selector every
/// requirement must hold (so `matchLabels` and `matchExpressions` on the hostname are
/// intersected), and the selectors are alternatives (their results are united). Any label or
/// operator other than `kubernetes.io/hostname` `In` is recorded in `unsupported` for the
/// check to report, as is a selector whose requirements exclude every node.
fn selector_hostnames(
    selectors: &[serde_yaml::Value],
    unsupported: &mut Vec<String>,
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    // A selector with no requirement matches every node; the list then selects every node
    // whatever the others say, which the empty result expresses.
    let mut unrestricted = false;
    for (i, sel) in selectors.iter().enumerate() {
        // Each requirement is a set of hostnames; the selector matches their intersection.
        let mut requirements: Vec<Vec<String>> = Vec::new();
        // A field in the wrong shape is reported, not read as "no requirement".
        if sel.get("matchLabels").is_some_and(|l| !l.is_mapping()) {
            unsupported.push(format!("selector {i} matchLabels is not a mapping"));
            continue;
        }
        if sel
            .get("matchExpressions")
            .is_some_and(|e| !e.is_sequence())
        {
            unsupported.push(format!("selector {i} matchExpressions is not a list"));
            continue;
        }
        if let Some(labels) = sel
            .get("matchLabels")
            .and_then(serde_yaml::Value::as_mapping)
        {
            for (k, v) in labels {
                match (k.as_str(), v.as_str()) {
                    (Some("kubernetes.io/hostname"), Some(h)) => {
                        requirements.push(vec![h.to_string()])
                    }
                    (Some(k), _) => unsupported.push(format!("matchLabels {k}")),
                    _ => {}
                }
            }
        }
        for e in sel
            .get("matchExpressions")
            .and_then(serde_yaml::Value::as_sequence)
            .into_iter()
            .flatten()
        {
            let key = e
                .get("key")
                .and_then(serde_yaml::Value::as_str)
                .unwrap_or("?");
            // Kubernetes requires the operator; an expression without one is invalid.
            let Some(op) = e.get("operator").and_then(serde_yaml::Value::as_str) else {
                unsupported.push(format!("matchExpressions {key} without operator"));
                continue;
            };
            if key != "kubernetes.io/hostname" || op != "In" {
                unsupported.push(format!("matchExpressions {key} {op}"));
                continue;
            }
            requirements.push(
                e.get("values")
                    .and_then(serde_yaml::Value::as_sequence)
                    .into_iter()
                    .flatten()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect(),
            );
        }
        let Some(first) = requirements.first() else {
            // No requirement at all (an empty selector, or empty matchLabels and
            // matchExpressions) matches every node; a selector whose only terms were
            // unsupported stays unknown.
            let labels_n = sel
                .get("matchLabels")
                .and_then(serde_yaml::Value::as_mapping)
                .map_or(0, |m| m.len());
            let exprs_n = sel
                .get("matchExpressions")
                .and_then(serde_yaml::Value::as_sequence)
                .map_or(0, |v| v.len());
            if labels_n == 0 && exprs_n == 0 {
                unrestricted = true;
            }
            continue;
        };
        let selected: Vec<String> = first
            .iter()
            .filter(|h| requirements.iter().all(|r| r.contains(h)))
            .cloned()
            .collect();
        if selected.is_empty() {
            unsupported.push(format!("nodeSelectors[{i}] selects no node"));
        }
        for h in selected {
            if !out.contains(&h) {
                out.push(h);
            }
        }
    }
    if unrestricted { Vec::new() } else { out }
}

fn read_yaml(path: &Path) -> eyre::Result<serde_yaml::Value> {
    let text =
        std::fs::read_to_string(path).wrap_err_with(|| format!("reading {}", path.display()))?;
    parse_yaml(path, &text)
}

/// The text of a collateral as a YAML tree, named by its path in the diagnostics.
pub(crate) fn parse_yaml(path: &Path, text: &str) -> eyre::Result<serde_yaml::Value> {
    // A placeholder file passes `config.collaterals.present`; read as an empty tree it would
    // fail every rule about its keys, so it is named here instead.
    eyre::ensure!(
        !text.trim().is_empty(),
        "{} is empty; write the values file before this check",
        path.display()
    );
    let mut value: serde_yaml::Value =
        serde_yaml::from_str(text).wrap_err_with(|| format!("parsing {}", path.display()))?;
    // Helm resolves `<<: *anchor` merge keys; resolved here too, so a key a merge supplies
    // is read at its path.
    value
        .apply_merge()
        .wrap_err_with(|| format!("parsing {} (merge keys)", path.display()))?;
    Ok(value)
}

/// A NICo TOML parsed, with a failure that names the place and the parser's message only:
/// the source line is not echoed, since a malformed value (a `database_url` with a
/// password) would otherwise reach the report.
pub(crate) fn parse_toml(path: &Path, which: &str, text: &str) -> eyre::Result<toml::Table> {
    text.parse::<toml::Table>().map_err(|e| {
        let at = e
            .span()
            .map(|s| {
                let (line, col) = line_col(text, s.start);
                format!(" at line {line}, column {col}")
            })
            .unwrap_or_default();
        eyre::eyre!(
            "{}: the {which}NICo TOML does not parse{at}: {}",
            path.display(),
            e.message()
        )
    })
}

/// One-based line and column of byte offset `at` in `text`.
fn line_col(text: &str, at: usize) -> (usize, usize) {
    let before = &text[..at.min(text.len())];
    let line = before.matches('\n').count() + 1;
    let col = before.rsplit('\n').next().map_or(0, str::len) + 1;
    (line, col)
}

/// A string at a dotted path in a TOML table (`dpf.services.dpu_agent.helm_repo_url`).
pub(crate) fn toml_str<'a>(table: &'a toml::Table, path: &str) -> Option<&'a str> {
    let mut keys = path.split('.');
    let mut v: &toml::Value = table.get(keys.next()?)?;
    for key in keys {
        v = v.get(key)?;
    }
    v.as_str()
}

/// A values file read as a tree, with dotted-path access.
pub(crate) struct Values {
    pub(crate) path: PathBuf,
    pub(crate) root: serde_yaml::Value,
}

impl Values {
    pub(crate) fn get(&self, path: &str) -> Option<&serde_yaml::Value> {
        yaml_get(&self.root, path)
    }

    pub(crate) fn str(&self, path: &str) -> Option<&str> {
        self.get(path).and_then(serde_yaml::Value::as_str)
    }

    pub(crate) fn u64(&self, path: &str) -> Option<u64> {
        self.get(path).and_then(|v| {
            v.as_u64()
                .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        })
    }

    pub(crate) fn bool(&self, path: &str) -> Option<bool> {
        self.get(path).and_then(serde_yaml::Value::as_bool)
    }

    /// The file name, for observed text.
    pub(crate) fn name(&self) -> String {
        self.path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.path.display().to_string())
    }
}

/// The value at a dotted path (`postgresql.resources.limits.memory`); a `[n]` suffix
/// indexes a sequence (`localData[2].addresses`).
pub(crate) fn yaml_get<'a>(
    root: &'a serde_yaml::Value,
    path: &str,
) -> Option<&'a serde_yaml::Value> {
    let mut v = root;
    for part in path.split('.') {
        let (key, index) = match part.split_once('[') {
            Some((k, rest)) => (
                k,
                rest.strip_suffix(']').and_then(|i| i.parse::<usize>().ok()),
            ),
            None => (part, None),
        };
        if !key.is_empty() {
            v = v.get(key)?;
        }
        if let Some(i) = index {
            v = v.get(i)?;
        }
    }
    Some(v)
}

pub(crate) fn helm_prereqs_values(ctx: &Ctx) -> eyre::Result<Values> {
    values(ctx, &ctx.sd.collaterals.helm_prereqs_values)
}

pub(crate) fn rest_values(ctx: &Ctx) -> eyre::Result<Values> {
    values(ctx, &ctx.sd.collaterals.rest_values)
}

pub(crate) fn site_agent_values(ctx: &Ctx) -> eyre::Result<Values> {
    values(ctx, &ctx.sd.collaterals.site_agent_values)
}

/// The Core values as a tree, for the keys beside the TOML.
pub(crate) fn core_values_tree(ctx: &Ctx) -> eyre::Result<Values> {
    values(ctx, &ctx.sd.collaterals.core_values)
}

fn values(ctx: &Ctx, rel: &Path) -> eyre::Result<Values> {
    let path = ctx.resolve(rel);
    let root = read_yaml(&path)?;
    Ok(Values { path, root })
}

/// The control-plane DPU site yaml, in the schema `build-dpu-install-iso.sh` accepts: the
/// builder's known keys and nothing else. Every field is optional here so that a missing
/// required field is the rules check's row, not a parse error.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct CpDpuSite {
    pub(crate) datacenter_asn: Option<u32>,
    pub(crate) site_controller_routes_asn: Option<u32>,
    pub(crate) bgp_asn_start: Option<u32>,
    pub(crate) site_controller_mtu_size: Option<u32>,
    pub(crate) name_server: Option<IpAddr>,
    pub(crate) ubuntu_password_hash: Option<String>,
    pub(crate) forge_dpu_loopback_prefix: Option<IpNet>,
    pub(crate) forge_service_vip_prefix: Option<IpNet>,
    pub(crate) forge_control_plane_prefix: Option<IpNet>,
    pub(crate) install_with_leaf_password: Option<serde_yaml::Value>,
    pub(crate) fnn: Option<CpDpuFnn>,
    #[serde(default)]
    pub(crate) site_controller_nodes: Vec<CpDpuNode>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct CpDpuFnn {
    pub(crate) control_plane_vni: Option<u64>,
    pub(crate) vpc_vrf_loopback_prefix: Option<IpNet>,
    pub(crate) common_managed_node_bmc_route_target: Option<u64>,
    pub(crate) common_site_controller_route_target: Option<u64>,
    pub(crate) common_admin_network_target: Option<u64>,
    /// Kept raw: the builder requires a map keyed `<asn>:<n>`, and a sequence is the
    /// mistake it guards against.
    pub(crate) route_targets_to_import: Option<serde_yaml::Value>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct CpDpuNode {
    pub(crate) host_name: Option<String>,
    pub(crate) mac: Option<String>,
    pub(crate) node_id: Option<u32>,
}

pub(crate) struct CpDpuSiteFile {
    pub(crate) path: PathBuf,
    pub(crate) site: CpDpuSite,
}

/// The CP-DPU site yaml when the site controllers carry a DPU: a plain-NIC site builds no
/// ISO, so a stale or broken yaml it still names stops no check that is not about it
/// (`config.collaterals.present` warns about the file, `config.cp-dpu-yaml.rules` skips).
pub(crate) fn cp_dpu_site_yaml_if_dpu(ctx: &Ctx) -> eyre::Result<Option<CpDpuSiteFile>> {
    if ctx.decided("dpu-in-the-site-controllers", "no DPU") {
        return Ok(None);
    }
    cp_dpu_site_yaml(ctx)
}

/// `None` when the site definition names no CP-DPU site yaml.
pub(crate) fn cp_dpu_site_yaml(ctx: &Ctx) -> eyre::Result<Option<CpDpuSiteFile>> {
    let Some(rel) = &ctx.sd.collaterals.cp_dpu_site_yaml else {
        return Ok(None);
    };
    let path = ctx.resolve(rel);
    let text =
        std::fs::read_to_string(&path).wrap_err_with(|| format!("reading {}", path.display()))?;
    let site = serde_yaml::from_str(&text)
        .wrap_err_with(|| format!("{}: not a control-plane DPU site yaml", path.display()))?;
    Ok(Some(CpDpuSiteFile { path, site }))
}

#[cfg(test)]
mod toml_path_tests {
    use super::toml_str;

    /// A reversed or mixed-family range is refused as MetalLB refuses it; equal ends are
    /// one address.
    #[test]
    fn a_reversed_pool_range_is_refused() {
        assert!(super::Addresses::parse("10.10.126.206-10.10.126.205").is_err());
        assert!(super::Addresses::parse("10.10.126.205-fd00::1").is_err());
        assert!(super::Addresses::parse("10.10.126.205-10.10.126.205").is_ok());
        assert!(super::Addresses::parse("10.10.126.200-10.10.126.205").is_ok());
    }

    /// `avoidBuggyIPs` keeps .0 and .255 inside the span but out of allocation.
    #[test]
    fn a_pool_avoiding_buggy_ips_withholds_dot_zero_and_dot_255() {
        let pool = super::Pool {
            name: "p".to_string(),
            addresses: vec![super::Addresses::parse("10.10.126.0/24").unwrap()],
            avoid_buggy_ips: true,
            service_allocation: false,
        };
        let ip = |s: &str| s.parse::<std::net::IpAddr>().unwrap();
        assert!(pool.contains(ip("10.10.126.255")));
        assert!(!pool.hands_out(ip("10.10.126.255")));
        assert!(!pool.hands_out(ip("10.10.126.0")));
        assert!(pool.hands_out(ip("10.10.126.1")));
        let plain = super::Pool {
            avoid_buggy_ips: false,
            service_allocation: false,
            ..pool
        };
        assert!(plain.hands_out(ip("10.10.126.255")));
    }

    #[test]
    fn a_top_level_key_is_read_like_a_dotted_path() {
        let t: toml::Table = toml::from_str("sitename = \"s1\"\n[dpf]\nmode = \"x\"\n").unwrap();
        assert_eq!(toml_str(&t, "sitename"), Some("s1"));
        assert_eq!(toml_str(&t, "dpf.mode"), Some("x"));
        assert_eq!(toml_str(&t, "dpf"), None);
    }
}

#[cfg(test)]
mod vip_tests {
    use super::collect_vips;

    #[test]
    fn an_empty_entry_in_a_list_is_invalid_while_an_empty_value_is_unset() {
        let read = |text: &str| {
            let v: serde_yaml::Value = serde_yaml::from_str(&format!(
                "svc:\n  annotations:\n    metallb.universe.tf/loadBalancerIPs: \"{text}\"\n"
            ))
            .unwrap();
            let (mut out, mut bad) = (Vec::new(), Vec::new());
            collect_vips(&v, String::new(), &mut out, &mut bad);
            (out.len(), bad)
        };
        // One address per family: a dual-stack request passes, two of one family do not.
        assert_eq!(read("10.0.0.1,2001:db8::1"), (2, vec![]));
        let (n, bad) = read("10.0.0.1,10.0.0.2");
        assert_eq!(n, 0);
        assert_eq!(
            bad,
            vec![
                "svc (`10.0.0.1,10.0.0.2` asks for two addresses of one family; one per family at most)"
                    .to_string()
            ]
        );
        // MetalLB 0.14.5 reads the `metallb.universe.tf` spelling only.
        let both: serde_yaml::Value = serde_yaml::from_str(
            "svc:\n  annotations:\n    metallb.io/loadBalancerIPs: 10.0.0.1\n    metallb.universe.tf/loadBalancerIPs: nonsense\n",
        )
        .unwrap();
        let (mut out, mut bad) = (Vec::new(), Vec::new());
        collect_vips(&both, String::new(), &mut out, &mut bad);
        assert_eq!(
            (out.len(), bad),
            (0, vec!["svc `nonsense`".to_string()]),
            "the legacy spelling is the one the installed MetalLB reads"
        );
        let new_only: serde_yaml::Value = serde_yaml::from_str(
            "svc:\n  annotations:\n    metallb.io/loadBalancerIPs: 10.0.0.1\n",
        )
        .unwrap();
        let (mut out, mut bad) = (Vec::new(), Vec::new());
        collect_vips(&new_only, String::new(), &mut out, &mut bad);
        assert_eq!(out.len(), 0);
        assert_eq!(
            bad,
            vec![
                "svc (metallb.io/loadBalancerIPs is not read by MetalLB 0.14.5; spell it metallb.universe.tf/loadBalancerIPs)"
                    .to_string()
            ]
        );
        let off: serde_yaml::Value = serde_yaml::from_str(
            "svc:\n  externalService:\n    enabled: false\n    annotations:\n      metallb.io/loadBalancerIPs: nonsense\n",
        )
        .unwrap();
        let (mut out, mut bad) = (Vec::new(), Vec::new());
        collect_vips(&off, String::new(), &mut out, &mut bad);
        assert_eq!(
            (out.len(), bad.len()),
            (0, 0),
            "a disabled Service is not rendered"
        );
        let omitted: serde_yaml::Value = serde_yaml::from_str(
            "svc:\n  externalService:\n    annotations:\n      metallb.universe.tf/loadBalancerIPs: 10.0.0.1\n      metallb.universe.tf/address-pool: p\n",
        )
        .unwrap();
        let (mut out, mut bad) = (Vec::new(), Vec::new());
        collect_vips(&omitted, String::new(), &mut out, &mut bad);
        assert_eq!(
            (out.len(), bad.len()),
            (0, 0),
            "a Service block without `enabled: true` is not rendered: the charts default it to false"
        );
        let mut pools = Vec::new();
        super::collect_pool_requests(&omitted, String::new(), &mut pools);
        assert!(pools.is_empty(), "nor is its pool request read");
        assert_eq!(read(""), (0, vec![]));
        // Whitespace alone is a request to MetalLB, whose one entry trims to nothing.
        let (n, bad) = read(" ");
        assert_eq!(n, 0);
        assert_eq!(bad, vec!["svc (an empty entry in ` `)".to_string()]);
        let (n, bad) = read("10.0.0.1,");
        assert_eq!(n, 0);
        assert_eq!(bad, vec!["svc (an empty entry in `10.0.0.1,`)".to_string()]);
    }

    /// A top-level `enabled` is read by Helm for the umbrella chart's conditional subcharts
    /// only, with the chart's default where omitted; an unconditional chart deploys
    /// whatever it says.
    #[test]
    fn a_top_level_enabled_is_read_for_conditional_subcharts_only() {
        let pins = |text: &str| {
            let v: serde_yaml::Value = serde_yaml::from_str(text).unwrap();
            let (mut out, mut bad) = (Vec::new(), Vec::new());
            collect_vips(&v, String::new(), &mut out, &mut bad);
            assert!(bad.is_empty(), "{bad:?}");
            out.len()
        };
        let svc = "  externalService:\n    enabled: true\n    annotations:\n      metallb.universe.tf/loadBalancerIPs: 10.0.0.1\n";
        assert_eq!(
            pins(&format!("nico-dhcp:\n  enabled: false\n{svc}")),
            1,
            "nico-dhcp is not conditional"
        );
        assert_eq!(
            pins(&format!("unbound:\n{svc}")),
            0,
            "unbound defaults to off"
        );
        assert_eq!(pins(&format!("unbound:\n  enabled: true\n{svc}")), 1);
        assert_eq!(
            pins(&format!("nico-ntp:\n{svc}")),
            1,
            "nico-ntp defaults to on"
        );
        assert_eq!(pins(&format!("nico-ntp:\n  enabled: false\n{svc}")), 0);
    }

    /// A top-level `siteConfig` is the nico-api chart's own layout: the umbrella chart
    /// setup.sh installs never reads it, so the file is refused with the reason.
    #[test]
    fn a_top_level_site_config_is_refused_for_the_umbrella_chart() {
        let v: serde_yaml::Value = serde_yaml::from_str(
            "siteConfig:\n  nicoApiSiteConfig: |\n    [dpf]\n    enabled = true\n",
        )
        .unwrap();
        let err = super::core_toml_of(std::path::Path::new("nico-core.yaml"), &v).unwrap_err();
        assert!(
            format!("{err:#}").contains("siteConfig is at the top level"),
            "{err:#}"
        );
        let v: serde_yaml::Value = serde_yaml::from_str("nico-dhcp: {}\n").unwrap();
        let err = super::core_toml_of(std::path::Path::new("nico-core.yaml"), &v).unwrap_err();
        assert!(
            format!("{err:#}").contains("no TOML under nico-api.siteConfig.nicoApiSiteConfig"),
            "{err:#}"
        );
    }

    /// A TOML that does not parse is reported by place and message, never by its source
    /// line, which may hold a secret.
    #[test]
    fn a_toml_parse_error_does_not_echo_the_source_line() {
        let text = "sitename = \"s1\"\ndatabase_url = \"postgres://nico:hunter2@db/nico\n";
        let err = super::parse_toml(std::path::Path::new("nico-core.yaml"), "", text).unwrap_err();
        let shown = format!("{err:#}");
        assert!(
            shown.contains("the NICo TOML does not parse at line 2"),
            "{shown}"
        );
        assert!(!shown.contains("hunter2"), "{shown}");
    }

    /// A merge key (`<<: *anchor`) is resolved as Helm resolves it, so a value a merge
    /// supplies is read at its path.
    #[test]
    fn merge_keys_are_resolved() {
        let v = super::parse_yaml(
            std::path::Path::new("values.yaml"),
            "base: &b\n  enabled: true\nsvc:\n  <<: *b\n  name: s\n",
        )
        .unwrap();
        assert_eq!(v["svc"]["enabled"].as_bool(), Some(true));
        assert_eq!(v["svc"]["name"].as_str(), Some("s"));
    }

    /// An empty value is the charts' placeholder in either spelling; a Service of another
    /// type than LoadBalancer pins nothing, and its pool request is not read either.
    #[test]
    fn empty_values_and_non_load_balancer_services_pin_nothing() {
        let read = |text: &str| {
            let v: serde_yaml::Value = serde_yaml::from_str(text).unwrap();
            let (mut out, mut bad) = (Vec::new(), Vec::new());
            collect_vips(&v, String::new(), &mut out, &mut bad);
            let mut pools = Vec::new();
            super::collect_pool_requests(&v, String::new(), &mut pools);
            (out.len(), bad, pools)
        };
        assert_eq!(
            read("svc:\n  annotations:\n    metallb.io/loadBalancerIPs: \"\"\n"),
            (0, vec![], vec![])
        );
        assert_eq!(
            read(
                "svc:\n  type: ClusterIP\n  annotations:\n    metallb.universe.tf/loadBalancerIPs: 10.0.0.1\n    metallb.universe.tf/address-pool: p\n"
            ),
            (0, vec![], vec![])
        );
        assert_eq!(
            read(
                "svc:\n  type: LoadBalancer\n  annotations:\n    metallb.universe.tf/loadBalancerIPs: 10.0.0.1\n    metallb.universe.tf/address-pool: p\n"
            ),
            (1, vec![], vec![("svc".to_string(), "p".to_string())])
        );
        // An empty pool annotation is no request: MetalLB applies the constraint only when
        // the annotation is non-empty.
        assert_eq!(
            read(
                "svc:\n  annotations:\n    metallb.universe.tf/loadBalancerIPs: 10.0.0.1\n    metallb.universe.tf/address-pool: \"\"\n"
            ),
            (1, vec![], vec![])
        );
    }
}
