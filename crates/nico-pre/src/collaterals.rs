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
}

pub(crate) fn core_values(ctx: &Ctx) -> eyre::Result<CoreValues> {
    let path = ctx.resolve(&ctx.sd.collaterals.core_values);
    let values: serde_yaml::Value = read_yaml(&path)?;
    // The umbrella chart nests the nico-api subchart's values under `nico-api`; the
    // subchart's own values file has `siteConfig` at the top. The layout is chosen first:
    // Helm does not hand a top-level `siteConfig` to the subchart, so an umbrella file
    // without `nico-api.siteConfig` has no TOML.
    let (layout, root) = match values.get("nico-api") {
        Some(sub) => ("nico-api.siteConfig.nicoApiSiteConfig", sub),
        None => ("siteConfig.nicoApiSiteConfig", &values),
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
        let parsed: toml::Table = global
            .parse()
            .wrap_err_with(|| format!("{}: the global NICo TOML does not parse", path.display()))?;
        merge_toml(&mut toml, parsed);
    }
    let site: toml::Table = text
        .parse()
        .wrap_err_with(|| format!("{}: the NICo TOML does not parse", path.display()))?;
    merge_toml(&mut toml, site);
    let mut vips = Vec::new();
    let mut bad = Vec::new();
    collect_vips(&values, String::new(), &mut vips, &mut bad);
    if !bad.is_empty() {
        eyre::bail!(
            "{}: loadBalancerIPs that are not addresses: {}",
            path.display(),
            bad.join(", ")
        );
    }
    Ok(CoreValues { path, toml, vips })
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

/// The two spellings of the annotation that pins a Service's VIPs; MetalLB honours both.
pub(crate) const LB_ANNOTATIONS: [&str; 2] = [
    "metallb.io/loadBalancerIPs",
    "metallb.universe.tf/loadBalancerIPs",
];

/// Every `loadBalancerIPs` annotation, one entry per address; an entry that is not an
/// address goes to `bad` with its path, since MetalLB would refuse the whole request.
fn collect_vips(
    v: &serde_yaml::Value,
    at: String,
    out: &mut Vec<(String, IpAddr)>,
    bad: &mut Vec<String>,
) {
    match v {
        serde_yaml::Value::Mapping(m) => {
            // A Service switched off is not rendered, so MetalLB never reads its annotation.
            if m.get("enabled").and_then(serde_yaml::Value::as_bool) == Some(false) {
                return;
            }
            for (k, v) in m {
                let Some(key) = k.as_str() else { continue };
                // MetalLB reads the stable annotation when both spellings are present.
                if key == LB_ANNOTATIONS[1] && m.contains_key(LB_ANNOTATIONS[0]) {
                    continue;
                }
                if LB_ANNOTATIONS.contains(&key) {
                    let here = at.trim_end_matches(".annotations").to_string();
                    let Some(text) = v.as_str() else {
                        bad.push(format!("{here} (not a string)"));
                        continue;
                    };
                    // MetalLB parses every comma-separated entry and rejects an empty one
                    // (`10.0.0.1,`); an entirely empty value is the charts' placeholder.
                    if !text.trim().is_empty() && text.split(',').any(|s| s.trim().is_empty()) {
                        bad.push(format!("{here} (an empty entry in `{text}`)"));
                        continue;
                    }
                    for ip in text.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                        match ip.parse() {
                            Ok(ip) => out.push((here.clone(), ip)),
                            Err(_) => bad.push(format!("{here} `{ip}`")),
                        }
                    }
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
}

impl Pool {
    pub(crate) fn contains(&self, ip: IpAddr) -> bool {
        self.addresses.iter().any(|a| a.contains(ip))
    }
}

/// One entry of `IPAddressPool.spec.addresses`: a CIDR, or `first-last`.
pub(crate) enum Addresses {
    Net(IpNet),
    Range(IpAddr, IpAddr),
}

impl Addresses {
    fn parse(text: &str) -> eyre::Result<Self> {
        if let Some((a, b)) = text.split_once('-') {
            Ok(Addresses::Range(a.trim().parse()?, b.trim().parse()?))
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
                        cfg.pools.push(Pool {
                            name: obj.metadata.name,
                            addresses,
                        });
                    }
                    "BGPPeer" => {
                        let mut unsupported = Vec::new();
                        let spec: PeerSpec =
                            serde_yaml::from_value(obj.spec).wrap_err_with(|| {
                                format!("{}: BGPPeer {}", path.display(), obj.metadata.name)
                            })?;
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
                            restricted: ["nodeSelectors", "peers"].iter().any(|k| {
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
                    if r.contains("://") || !file.is_file() {
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
            "apiVersion" | "kind" | "metadata" | "namespace" | "commonLabels" | "labels"
            | "commonAnnotations" | "generatorOptions" | "buildMetadata" => {}
            other => eyre::bail!(
                "{}: `{other}` cannot be read directly; render the kustomization with `kubectl kustomize` and name the output",
                kustomization.display()
            ),
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
    // A placeholder file passes `config.collaterals.present`; read as an empty tree it would
    // fail every rule about its keys, so it is named here instead.
    eyre::ensure!(
        !text.trim().is_empty(),
        "{} is empty; write the values file before this check",
        path.display()
    );
    serde_yaml::from_str(&text).wrap_err_with(|| format!("parsing {}", path.display()))
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
        assert_eq!(read("10.0.0.1,10.0.0.2"), (2, vec![]));
        let both: serde_yaml::Value = serde_yaml::from_str(
            "svc:\n  annotations:\n    metallb.io/loadBalancerIPs: 10.0.0.1\n    metallb.universe.tf/loadBalancerIPs: nonsense\n",
        )
        .unwrap();
        let (mut out, mut bad) = (Vec::new(), Vec::new());
        collect_vips(&both, String::new(), &mut out, &mut bad);
        assert_eq!(
            (out.len(), bad.len()),
            (1, 0),
            "the stable annotation is the one MetalLB reads"
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
        assert_eq!(read(""), (0, vec![]));
        let (n, bad) = read("10.0.0.1,");
        assert_eq!(n, 0);
        assert_eq!(bad, vec!["svc (an empty entry in `10.0.0.1,`)".to_string()]);
    }
}
