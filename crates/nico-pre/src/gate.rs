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

//! The site-definition gate every checklist runs first. It proves the site
//! definition is usable before any stage check reads it: the decisions are
//! known register entries with valid options, the stated ones are present,
//! the implication and conflict rules hold, and the arithmetic invariants
//! of the network and host sections are met. A failing gate stops the
//! checklist with exit code 3 and the offending rows.

use std::collections::{BTreeSet, HashSet};
use std::str::FromStr;

use indexmap::IndexMap;
use ipnet::IpNet;
use mac_address::MacAddress;

use crate::check::Row;
use crate::register::Register;
use crate::sitedef::SiteDef;

/// Runs every gate check and returns one row per invariant, or one row per
/// offending value when an invariant fails.
pub(crate) fn run(sd: &SiteDef, reg: &Register) -> Vec<Row> {
    let mut rows = Vec::new();
    decisions(sd, reg, &mut rows);
    setup(sd, &mut rows, reg);
    network(sd, &mut rows);
    hosts(sd, &mut rows);
    rows
}

/// `setup.command` is a command line the shell can parse; the gate reads it for the
/// decisions the command makes, so a quote the shell would refuse stops the run here.
fn setup(sd: &SiteDef, rows: &mut Vec<Row>, reg: &Register) {
    const ROW: &str = "sitedef.setup.command";
    const EXPECTED: &str =
        "a command line the shell can parse, agreeing with the pinned and implied decisions";
    let cmd = crate::setup_cmd::SetupCommand::parse(&sd.setup);
    let mut problems: Vec<(String, String)> = Vec::new();
    if let Some(e) = &cmd.quoting_error {
        problems.push((e.clone(), "close the quote in setup.command".to_string()));
    }
    if let Some(e) = &cmd.env_error {
        problems.push((
            e.clone(),
            "set the variable in setup.env to true or false".to_string(),
        ));
    }
    // What the command will do, against what the site definition pins or its rules imply:
    // a disagreement there is the first thing setup.sh would get wrong.
    let without_command = effective_decisions_from(&sd.decisions, reg);
    let from_command = if cmd.runs_setup_sh() {
        command_decisions(&cmd).to_vec()
    } else {
        Vec::new()
    };
    for (id, option) in from_command {
        // `--with-observability` decides only whether the local stack is installed; a site
        // on an external stack runs without the switch and is not contradicted by it.
        // `--skip-rms` decides only whether the in-cluster RMS is installed; an external RMS
        // runs with the switch and is not contradicted by it.
        let disagrees = |values: &Vec<String>| match id {
            "observability" => {
                values.iter().any(|v| v == "Full local stack") != cmd.with_observability()
            }
            "rms" => values.iter().any(|v| v == "In-cluster nv-rms") != cmd.installs_rms(),
            _ => values.iter().all(|v| v != option),
        };
        if let Some(values) = without_command.values.get(id)
            && disagrees(values)
        {
            let how = without_command
                .implied_by
                .get(id)
                .map_or("pinned".to_string(), |by| format!("implied by {by}"));
            problems.push((
                format!(
                    "setup.command makes {id} `{option}`, the site definition has `{}` ({how})",
                    values.join(" + ")
                ),
                "change the command or the decision".to_string(),
            ));
        }
    }
    if problems.is_empty() {
        let observed = if cmd.runs_setup_sh() {
            format!("{} switches", cmd.switches.len())
        } else {
            "the command does not run setup.sh; no decision to compare".to_string()
        };
        rows.push(Row::pass(ROW, EXPECTED, observed));
    }
    for (observed, fix) in problems {
        rows.push(Row::fail(ROW, EXPECTED, observed).with_fix(fix));
    }
}

/// The register entries `setup.command` decides, with the option it selects.
fn command_decisions(cmd: &crate::setup_cmd::SetupCommand) -> [(&'static str, &'static str); 3] {
    [
        (
            "dpf",
            if cmd.installs_dpf() {
                "Install"
            } else {
                "Skip"
            },
        ),
        (
            "rms",
            if cmd.installs_rms() {
                "In-cluster nv-rms"
            } else {
                "Not applicable"
            },
        ),
        (
            "observability",
            if cmd.with_observability() {
                "Full local stack"
            } else {
                "No pipeline"
            },
        ),
    ]
}

/// The decisions in effect after the implication rules: stated ones first,
/// then what the rules imply from them. The second map says which decision
/// implied each implied entry.
pub(crate) struct EffectiveDecisions {
    /// Every value is a list: one entry for a single-select, any number for an "Any of".
    pub(crate) values: IndexMap<String, Vec<String>>,
    pub(crate) implied_by: IndexMap<String, String>,
}

/// True when a stated value is usable: every listed option exists, and a
/// single-select carries exactly one.
fn valid_value(d: &crate::register::Decision, vs: &[String]) -> bool {
    (d.multiple || vs.len() == 1) && vs.iter().all(|v| d.option(v).is_some())
}

/// Applies the register's implication rules to the stated decisions until
/// nothing changes. Stated entries with an unknown id or option are left
/// out; `decisions` reports those.
pub(crate) fn effective_decisions(sd: &SiteDef, reg: &Register) -> EffectiveDecisions {
    // The entries `setup.command` and its variables decide are read from them when the
    // site definition does not pin them, so the conflict rules see what setup.sh will do
    // (a BF4 fleet with `--skip-dpf`, for one) before any checklist runs.
    let cmd = crate::setup_cmd::SetupCommand::parse(&sd.setup);
    let mut stated = sd.decisions.clone();
    let from_command = if cmd.runs_setup_sh() {
        command_decisions(&cmd).to_vec()
    } else {
        Vec::new()
    };
    let mut effective = effective_decisions_from(&stated, reg);
    let mut added = Vec::new();
    for (id, option) in from_command {
        if !effective.values.contains_key(id) {
            stated.insert(
                id.to_string(),
                crate::sitedef::DecisionValue::One(option.to_string()),
            );
            added.push(id.to_string());
        }
    }
    if !added.is_empty() {
        effective = effective_decisions_from(&stated, reg);
        for id in added {
            effective.implied_by.insert(id, "setup.command".to_string());
        }
    }
    effective
}

/// [`effective_decisions`] on a bare decisions map, for the template writer.
pub(crate) fn effective_decisions_from(
    stated: &IndexMap<String, crate::sitedef::DecisionValue>,
    reg: &Register,
) -> EffectiveDecisions {
    // Stored under the register's spelling of each option, so a check comparing names
    // sees what `validate` accepted, whatever case or spacing the site definition used.
    let mut values: IndexMap<String, Vec<String>> = stated
        .iter()
        .filter_map(|(id, value)| {
            let d = reg.get(id)?;
            let vs = value.values();
            valid_value(d, &vs).then(|| {
                let canonical = vs
                    .iter()
                    .map(|v| d.option(v).map_or_else(|| v.clone(), |o| o.name.clone()))
                    .collect();
                (id.clone(), canonical)
            })
        })
        .collect();
    let mut implied_by = IndexMap::new();
    loop {
        let mut changed = false;
        for rule in &reg.rules {
            let held = values.get(&rule.when.id).map(Vec::as_slice);
            if !rule.when.holds(held) {
                continue;
            }
            for implied in &rule.set {
                if !values.contains_key(&implied.id) {
                    values.insert(implied.id.clone(), vec![implied.option.clone()]);
                    implied_by.insert(implied.id.clone(), rule.when.id.clone());
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
    EffectiveDecisions { values, implied_by }
}

fn decisions(sd: &SiteDef, reg: &Register, rows: &mut Vec<Row>) {
    let before = rows.len();

    for (id, value) in &sd.decisions {
        let vs = value.values();
        match reg.get(id) {
            None => rows.push(
                Row::fail(
                    format!("sitedef.decisions.{id}"),
                    "a register entry",
                    format!("`{id}` is not in the register"),
                )
                .with_fix("remove it, or run `nico-pre decisions` to list the entries"),
            ),
            Some(d) if !d.multiple && vs.len() != 1 => rows.push(
                Row::fail(
                    format!("sitedef.decisions.{id}"),
                    format!("exactly one of: {}", d.option_names()),
                    format!("{} values: {}", vs.len(), quoted(&vs)),
                )
                .with_fix("state one option; only `Any of` entries take a list"),
            ),
            Some(d) => {
                for v in vs.iter().filter(|v| d.option(v).is_none()) {
                    rows.push(
                        Row::fail(
                            format!("sitedef.decisions.{id}"),
                            format!("one of: {}", d.option_names()),
                            format!("`{v}`"),
                        )
                        .with_fix("state the option by its short name"),
                    );
                }
            }
        }
    }

    let effective = effective_decisions(sd, reg);

    // A stated option that contradicts what another stated option implies.
    let mut reported = BTreeSet::new();
    for rule in &reg.rules {
        let held = effective.values.get(&rule.when.id).map(Vec::as_slice);
        if !rule.when.holds(held) {
            continue;
        }
        for implied in &rule.set {
            let Some(current) = effective.values.get(&implied.id) else {
                continue;
            };
            let agrees = current
                .iter()
                .any(|c| c.eq_ignore_ascii_case(&implied.option));
            if agrees || !reported.insert(implied.id.clone()) {
                continue;
            }
            rows.push(
                Row::fail(
                    format!("sitedef.decisions.{}", implied.id),
                    format!(
                        "`{}`, implied by {} = {}",
                        implied.option,
                        rule.when.id,
                        quoted(held.unwrap_or_default())
                    ),
                    quoted(current),
                )
                .with_fix("change one of the two decisions so they agree"),
            );
        }
    }

    // Present-but-invalid entries were reported above; only truly absent ones here.
    for d in reg.stated() {
        if !effective.values.contains_key(&d.id) && !sd.decisions.contains_key(&d.id) {
            rows.push(
                Row::fail(
                    format!("sitedef.decisions.{}", d.id),
                    "stated in the site definition; no collateral records it",
                    "absent",
                )
                .with_fix(format!(
                    "add `{}: <{}>` under `decisions`",
                    d.id,
                    d.option_names()
                )),
            );
        }
    }

    for conflict in &reg.conflicts {
        let held: Vec<_> = conflict
            .between
            .iter()
            .map(|t| (t, effective.values.get(&t.id).map(Vec::as_slice)))
            .collect();
        if held.iter().all(|(t, v)| t.holds(*v)) {
            let observed = held
                .iter()
                .map(|(t, v)| {
                    let shown = quoted(v.unwrap_or_default());
                    if t.lacks {
                        format!(
                            "{} = {} (without `{}`)",
                            t.id,
                            shown,
                            t.options.join("`, `")
                        )
                    } else {
                        format!("{} = {shown}", t.id)
                    }
                })
                .collect::<Vec<_>>()
                .join(" with ");
            rows.push(
                Row::fail(
                    "sitedef.decisions.conflict",
                    "no two held options exclude each other",
                    observed,
                )
                .with_fix(conflict.reason.clone()),
            );
        }
    }

    if rows.len() == before {
        rows.push(Row::pass(
            "sitedef.decisions",
            "known entries, valid options, stated ones present, rules hold",
            format!(
                "{} stated, {} implied, {} in effect",
                sd.decisions.len(),
                effective.implied_by.len(),
                effective.values.len()
            ),
        ));
    }
}

/// `` `a` `` for one value, `` `a`, `b` `` for several, `(none)` for an empty list.
fn quoted(values: &[String]) -> String {
    if values.is_empty() {
        return "(none)".to_string();
    }
    values
        .iter()
        .map(|v| format!("`{v}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Every prefix the site definition carries, with the key it came from.
fn prefixes(sd: &SiteDef) -> Vec<(String, IpNet)> {
    let n = &sd.network;
    let mut out = vec![
        (
            "network.forgeControlPlanePrefix".to_string(),
            n.forge_control_plane_prefix,
        ),
        (
            "network.forgeServiceVipPrefix".to_string(),
            n.forge_service_vip_prefix,
        ),
        (
            "network.forgeDpuLoopbackPrefix".to_string(),
            n.forge_dpu_loopback_prefix,
        ),
        (
            "network.kubernetes.podCidr".to_string(),
            n.kubernetes.pod_cidr,
        ),
        (
            "network.kubernetes.serviceCidr".to_string(),
            n.kubernetes.service_cidr,
        ),
    ];
    for (i, p) in n.forge_overlay_prefixes.iter().enumerate() {
        out.push((format!("network.forgeOverlayPrefixes[{i}]"), *p));
    }
    for (key, seg) in segments(sd) {
        out.push((format!("{key}.prefix"), seg.prefix));
    }
    if let Some(fnn) = &n.fnn {
        out.push((
            "network.fnn.vpcVrfLoopbackPrefix".to_string(),
            fnn.vpc_vrf_loopback_prefix,
        ));
    }
    out
}

struct SegmentRef {
    prefix: IpNet,
    gateway: std::net::IpAddr,
}

fn segments(sd: &SiteDef) -> Vec<(String, SegmentRef)> {
    let s = &sd.network.segments;
    let mut out = vec![
        (
            "network.segments.admin".to_string(),
            SegmentRef {
                prefix: s.admin.prefix,
                gateway: s.admin.gateway,
            },
        ),
        (
            "network.segments.controlPlaneOob".to_string(),
            SegmentRef {
                prefix: s.control_plane_oob.prefix,
                gateway: s.control_plane_oob.gateway,
            },
        ),
    ];
    for seg in &s.managed_host_bmc {
        out.push((
            format!("network.segments.managedHostBmc[{}]", seg.switch_name),
            SegmentRef {
                prefix: seg.prefix,
                gateway: seg.gateway,
            },
        ));
    }
    for seg in &s.host_inband {
        out.push((
            format!("network.segments.hostInband[{}]", seg.name),
            SegmentRef {
                prefix: seg.prefix,
                gateway: seg.gateway,
            },
        ));
    }
    out
}

fn network(sd: &SiteDef, rows: &mut Vec<Row>) {
    let n = &sd.network;
    let prefixes = prefixes(sd);

    let misaligned: Vec<_> = prefixes.iter().filter(|(_, p)| p.trunc() != *p).collect();
    if misaligned.is_empty() {
        rows.push(Row::pass(
            "sitedef.network.prefix-aligned",
            "every prefix is aligned to its mask",
            format!("{} prefixes", prefixes.len()),
        ));
    }
    for (key, p) in misaligned {
        rows.push(
            Row::fail(
                "sitedef.network.prefix-aligned",
                "a prefix aligned to its mask",
                format!("{key} = {p}"),
            )
            .with_fix(format!("use {}", p.trunc())),
        );
    }

    let mut overlaps = Vec::new();
    for (i, (ka, a)) in prefixes.iter().enumerate() {
        for (kb, b) in &prefixes[i + 1..] {
            if a.contains(b) || b.contains(a) {
                overlaps.push(format!("{ka} = {a} and {kb} = {b}"));
            }
        }
    }
    if overlaps.is_empty() {
        rows.push(Row::pass(
            "sitedef.network.no-overlap",
            "no two prefixes overlap",
            "disjoint",
        ));
    }
    for observed in overlaps {
        rows.push(
            Row::fail(
                "sitedef.network.no-overlap",
                "no two prefixes overlap",
                observed,
            )
            .with_fix("move one of the two prefixes"),
        );
    }

    // Outside the prefix, or on its network or broadcast address (prefixes wide enough to
    // have them), neither of which a host can use as a gateway.
    let unusable = |seg: &SegmentRef| {
        if !seg.prefix.contains(&seg.gateway) {
            return Some("outside its prefix");
        }
        let has_ends = match seg.prefix {
            IpNet::V4(p) => p.prefix_len() < 31,
            IpNet::V6(p) => p.prefix_len() < 127,
        };
        // IPv4 only: the IPv6 all-zero host address is the subnet-router anycast, a
        // legitimate gateway.
        if has_ends && seg.prefix.addr().is_ipv4() && seg.gateway == seg.prefix.network() {
            return Some("the network address");
        }
        // IPv6 has no broadcast address; its last address is an ordinary one.
        if has_ends && seg.prefix.addr().is_ipv4() && seg.gateway == seg.prefix.broadcast() {
            return Some("the broadcast address");
        }
        None
    };
    let outside: Vec<_> = segments(sd)
        .into_iter()
        .filter_map(|(key, seg)| unusable(&seg).map(|why| (key, seg, why)))
        .collect();
    if outside.is_empty() {
        rows.push(Row::pass(
            "sitedef.network.gateway-inside",
            "every segment gateway lies inside its prefix",
            format!("{} segments", segments(sd).len()),
        ));
    }
    for (key, seg, why) in outside {
        rows.push(
            Row::fail(
                "sitedef.network.gateway-inside",
                format!("a usable gateway inside {}", seg.prefix),
                format!("{key}.gateway = {}, {why}", seg.gateway),
            )
            .with_fix("fix the gateway or the prefix"),
        );
    }

    let mut pools: Vec<(String, u64, u64)> = vec![
        (
            "network.pools.vlan".to_string(),
            n.pools.vlan.start,
            n.pools.vlan.end,
        ),
        (
            "network.pools.vni".to_string(),
            n.pools.vni.start,
            n.pools.vni.end,
        ),
        (
            "network.bgpAsnStart..bgpAsnEnd".to_string(),
            u64::from(n.bgp_asn_start),
            u64::from(n.bgp_asn_end),
        ),
    ];
    if let Some(r) = n.pools.external_vpc_vni {
        pools.push(("network.pools.externalVpcVni".to_string(), r.start, r.end));
    }
    if let Some(fnn) = &n.fnn {
        for (i, r) in fnn.pools_vpc_vni.iter().enumerate() {
            pools.push((format!("network.fnn.poolsVpcVni[{i}]"), r.start, r.end));
        }
    }
    let inverted: Vec<_> = pools.iter().filter(|(_, s, e)| s > e).collect();
    if inverted.is_empty() {
        rows.push(Row::pass(
            "sitedef.network.pool-order",
            "every pool has start at or below end",
            format!("{} pools", pools.len()),
        ));
    }
    for (key, s, e) in inverted {
        rows.push(
            Row::fail(
                "sitedef.network.pool-order",
                "start at or below end",
                format!("{key} = {s}..{e}"),
            )
            .with_fix("swap or correct the bounds"),
        );
    }

    // Identifier spaces: VLAN ids 1..=4094 (0 and 4095 are reserved), VNIs 24 bits.
    const VLAN_MAX: u64 = 4094;
    const VNI_MAX: u64 = (1 << 24) - 1;
    let out_of_space: Vec<String> = pools
        .iter()
        .filter_map(|(key, s, e)| {
            // The pools list also carries the ASN range, which has its own row.
            let (lo, hi, what) = if key == "network.pools.vlan" {
                (1, VLAN_MAX, "VLAN ids 1..4094")
            } else if key.to_ascii_lowercase().contains("vni") {
                (0, VNI_MAX, "VNIs 0..16777215")
            } else {
                return None;
            };
            (*s < lo || *e > hi).then(|| format!("{key} = {s}..{e}, outside {what}"))
        })
        .collect();
    if out_of_space.is_empty() {
        rows.push(Row::pass(
            "sitedef.network.pool-range",
            "every pool within its identifier space",
            format!("{} pools", pools.len()),
        ));
    }
    for observed in out_of_space {
        rows.push(
            Row::fail(
                "sitedef.network.pool-range",
                "VLAN ids 1..4094, VNIs 0..16777215",
                observed,
            )
            .with_fix("correct the bounds"),
        );
    }

    asn_layout(sd, rows);
}

/// The ASN range holds the site controller ASNs and leaves a per-DPU pool that the
/// datacenter ASN does not fall into. Derived by convention (routes = start, node i =
/// start + i) unless `asnLayout` overrides it.
fn asn_layout(sd: &SiteDef, rows: &mut Vec<Row>) {
    const ROW: &str = "sitedef.network.asn-layout";
    let n = &sd.network;
    let range = n.bgp_asn_start..=n.bgp_asn_end;
    let mut problems: Vec<(String, String)> = Vec::new();

    if range.contains(&n.datacenter_asn) {
        problems.push((
            format!("network.datacenterAsn = {}", n.datacenter_asn),
            "move the per-DPU ASN range or the datacenter ASN".to_string(),
        ));
    }
    let ids: Vec<u32> = sd
        .hosts
        .site_controllers
        .iter()
        .map(|c| c.node_id)
        .collect();
    if let Some(m) = n
        .asn_layout
        .as_ref()
        .and_then(|l| l.site_controllers.as_ref())
    {
        for id in m.keys().filter(|id| !ids.contains(id)) {
            problems.push((
                format!("network.asnLayout.siteControllers names nodeId {id}, which no site controller has"),
                "key the overrides by the site controllers' nodeId".to_string(),
            ));
        }
    }
    let routes = n.site_controller_routes_asn();
    let overrides = n
        .asn_layout
        .as_ref()
        .and_then(|l| l.site_controllers.as_ref());
    let mut reserved: Vec<u32> = vec![routes];
    reserved.extend(ids.iter().map(|&id| n.site_controller_asn(id)));
    // A node ASN the convention derives has to lie in the range; only an explicit
    // override may sit outside it.
    for &id in &ids {
        let explicit = overrides.is_some_and(|m| m.contains_key(&id));
        let asn = n.site_controller_asn(id);
        if !explicit && !range.contains(&asn) {
            problems.push((
                format!(
                    "nodeId {id} would take {asn}, outside {}..{}",
                    n.bgp_asn_start, n.bgp_asn_end
                ),
                "widen bgpAsnEnd, or state asnLayout".to_string(),
            ));
        }
    }
    // Two controllers on one ASN, or a controller or the routes ASN on the datacenter
    // ASN, is a broken layout whatever the range says.
    for (i, &id) in ids.iter().enumerate() {
        let asn = n.site_controller_asn(id);
        if asn == n.datacenter_asn {
            problems.push((
                format!("nodeId {id} takes {asn}, the datacenter ASN"),
                "give the site controller another ASN".to_string(),
            ));
        }
        for &other in &ids[i + 1..] {
            if n.site_controller_asn(other) == asn {
                problems.push((
                    format!("nodeId {id} and nodeId {other} both take {asn}"),
                    "give each site controller its own ASN".to_string(),
                ));
            }
        }
    }
    if routes == n.datacenter_asn {
        problems.push((
            format!("the routes ASN {routes} is the datacenter ASN"),
            "MetalLB and the site controllers peer with the datacenter; they cannot share its ASN"
                .to_string(),
        ));
    }
    // Counted, not enumerated: the range may span the whole 32-bit space.
    let length = if n.bgp_asn_end >= n.bgp_asn_start {
        u64::from(n.bgp_asn_end - n.bgp_asn_start) + 1
    } else {
        0
    };
    reserved.sort_unstable();
    reserved.dedup();
    let in_range = reserved.iter().filter(|a| range.contains(a)).count() as u64;
    let dpu_pool = length.saturating_sub(in_range);
    if dpu_pool == 0 {
        problems.push((
            format!(
                "no ASN left for DPUs in {}..{}",
                n.bgp_asn_start, n.bgp_asn_end
            ),
            "widen bgpAsnEnd".to_string(),
        ));
    }

    if problems.is_empty() {
        let nodes: Vec<String> = ids
            .iter()
            .map(|&id| n.site_controller_asn(id).to_string())
            .collect();
        rows.push(Row::pass(
            ROW,
            "the ASN range holds the site controller ASNs and leaves a per-DPU pool clear of the datacenter ASN",
            format!(
                "routes {routes}, nodes {}, {dpu_pool} for DPUs in {}..{}",
                nodes.join(" "),
                n.bgp_asn_start,
                n.bgp_asn_end
            ),
        ));
    }
    for (observed, fix) in problems {
        rows.push(Row::fail(ROW, "a consistent ASN layout", observed).with_fix(fix));
    }
}

fn hosts(sd: &SiteDef, rows: &mut Vec<Row>) {
    let controllers = &sd.hosts.site_controllers;

    let mut ids: Vec<u32> = controllers.iter().map(|c| c.node_id).collect();
    ids.sort_unstable();
    let expected: Vec<u32> = (1..=u32::try_from(controllers.len()).unwrap_or(u32::MAX)).collect();
    if ids == expected {
        rows.push(Row::pass(
            "sitedef.hosts.node-ids",
            "node ids run from one without gaps",
            format!("1..{}", controllers.len()),
        ));
    } else {
        rows.push(
            Row::fail(
                "sitedef.hosts.node-ids",
                format!("1..{} without gaps or repeats", controllers.len()),
                format!("{ids:?}"),
            )
            .with_fix("renumber the site controllers"),
        );
    }

    let mut duplicates = Vec::new();
    let mut names = HashSet::new();
    let mut serials = HashSet::new();
    for c in controllers {
        if !names.insert(c.host_name.as_str()) {
            duplicates.push(format!("hostName `{}`", c.host_name));
        }
        if !serials.insert(c.host_serial.as_str()) {
            duplicates.push(format!("hostSerial `{}`", c.host_serial));
        }
    }
    if duplicates.is_empty() {
        rows.push(Row::pass(
            "sitedef.hosts.unique",
            "unique hostnames and serials",
            format!("{} site controllers", controllers.len()),
        ));
    }
    for observed in duplicates {
        rows.push(
            Row::fail(
                "sitedef.hosts.unique",
                "unique hostnames and serials",
                format!("{observed} repeats"),
            )
            .with_fix("give every site controller its own hostname and serial"),
        );
    }

    let bad_macs: Vec<_> = controllers
        .iter()
        .filter_map(|c| c.p0_mac.as_deref().map(|m| (c.host_name.as_str(), m)))
        .filter(|(_, m)| MacAddress::from_str(m).is_err())
        .collect();
    if bad_macs.is_empty() {
        rows.push(Row::pass(
            "sitedef.hosts.p0-mac",
            "every stated p0 MAC parses",
            format!(
                "{} stated",
                controllers.iter().filter(|c| c.p0_mac.is_some()).count()
            ),
        ));
    }
    for (host, mac) in bad_macs {
        rows.push(
            Row::fail(
                "sitedef.hosts.p0-mac",
                "a MAC address like aa:bb:cc:dd:ee:ff",
                format!("{host}: `{mac}`"),
            )
            .with_fix("correct the MAC or leave p0Mac out until post-power-cycle.sh fills it in"),
        );
    }

    // Optional vantage points: when stated, every hostInband probe names a segment.
    if let Some(probes) = &sd.hosts.probe_hosts {
        let segments: HashSet<&str> = sd
            .network
            .segments
            .host_inband
            .iter()
            .map(|s| s.name.as_str())
            .collect();
        let unknown: Vec<&str> = probes
            .host_inband
            .iter()
            .map(|p| p.name.as_str())
            .filter(|n| !segments.contains(n))
            .collect();
        if unknown.is_empty() {
            rows.push(Row::pass(
                "sitedef.hosts.probe-hosts",
                "every hostInband probe names a hostInband segment",
                format!(
                    "{} hostInband probes{}",
                    probes.host_inband.len(),
                    if probes.admin.is_some() {
                        ", one admin probe"
                    } else {
                        ""
                    }
                ),
            ));
        } else {
            rows.push(
                Row::fail(
                    "sitedef.hosts.probe-hosts",
                    "every hostInband probe names a hostInband segment",
                    format!("no segment named {}", unknown.join(", ")),
                )
                .with_fix("use the segment's `name` from network.segments.hostInband"),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check::Status;
    use crate::sitedef::parse;

    const SAMPLE: &str = include_str!("../examples/site-def.yaml");

    /// The developer-site example is deployed by other scripts: its command makes no
    /// decision, so the implied `rms: Not applicable` is not contradicted.
    #[test]
    fn dev1_example_passes_the_gate() {
        let sd = parse(include_str!("../examples/site-def-dev1.yaml")).expect("dev1 parses");
        let rows = run(&sd, Register::builtin());
        let failures: Vec<_> = rows.iter().filter(|r| r.status != Status::Pass).collect();
        assert!(failures.is_empty(), "{failures:#?}");
        let setup = rows
            .iter()
            .find(|r| r.id == "sitedef.setup.command")
            .unwrap();
        assert!(
            setup.observed.contains("does not run setup.sh"),
            "{}",
            setup.observed
        );
    }

    #[test]
    fn sample_passes_the_gate() {
        let sd = parse(SAMPLE).expect("sample parses");
        let rows = run(&sd, Register::builtin());
        let failures: Vec<_> = rows.iter().filter(|r| r.status != Status::Pass).collect();
        assert!(failures.is_empty(), "{failures:#?}");
        assert_eq!(rows.len(), 12, "one pass row per invariant: {rows:#?}");
    }

    /// Combinations the register supports must pass the gate: a reviewer found the
    /// generated rules firing for `Skip` where only `Install` applies, and an NGC-only
    /// site being denied its pull secret.
    #[test]
    fn supported_combinations_pass_the_gate() {
        let pins: &[(&str, &str)] = &[
            (
                "DPF skipped with the iPXE bootstrap CA pinned",
                "  dpf: Skip\n  dpu-bootstrap-ca-non-dpf-path: legacy_download\n",
            ),
            (
                "DPF skipped with BMC rotation",
                "  dpf: Skip\n  autonomous-bmc-and-uefi-rotation: [BMC rotation]\n",
            ),
            (
                "DPF installed with a host still on the iPXE path",
                "  dpf: Install\n  dpu-bootstrap-ca-non-dpf-path: legacy_download\n",
            ),
            (
                "NVLink partitioning through explicit NMX-C mappings, no RMS",
                "  optional-components: [REST group, NVLink partitioning]\n",
            ),
            (
                "NGC-only site pulling with a secret",
                "  staging-host: Connected\n  external-registry-access: Direct\n  registry-host: None\n  registry-authentication: Credentialed pulls\n",
            ),
        ];
        for (name, pin) in pins {
            let text = SAMPLE
                .replacen("  staging-host: Air-gapped with a transfer path\n", "", 1)
                .replacen(
                    "  build-host: Yes\n",
                    &format!("  build-host: Yes\n{pin}"),
                    1,
                );
            let text = if pin.contains("staging-host") {
                text
            } else {
                text.replacen(
                    "  build-host: Yes\n",
                    "  staging-host: Air-gapped with a transfer path\n  build-host: Yes\n",
                    1,
                )
            };
            // A pinned `dpf: Skip` needs the command to skip DPF too; the gate now checks that.
            let text = if pin.contains("dpf: Skip") {
                text.replacen("--skip-rms", "--skip-rms --skip-dpf", 1)
            } else {
                text
            };
            let text = if pin.contains("optional-components") {
                text.replacen(
                    "  optional-components: [REST group]",
                    "  # optional-components: [REST group]",
                    1,
                )
            } else {
                text
            };
            let sd = parse(&text).expect("parses");
            let rows = run(&sd, Register::builtin());
            let failures: Vec<_> = rows.iter().filter(|r| r.status == Status::Fail).collect();
            assert!(failures.is_empty(), "{name}: {failures:#?}");
        }
    }

    #[test]
    fn stated_options_are_stored_under_the_register_spelling() {
        let text = SAMPLE.replacen(
            "dpu-in-the-site-controllers: BF3 DPU in DPU mode",
            "dpu-in-the-site-controllers: \"no dpu \"",
            1,
        );
        let sd = parse(&text).expect("parses");
        let effective = effective_decisions(&sd, Register::builtin());
        assert_eq!(
            effective.values.get("dpu-in-the-site-controllers"),
            Some(&vec!["no DPU".to_string()])
        );
    }

    #[test]
    fn a_node_asn_by_convention_is_range_checked_even_with_an_asn_layout() {
        let text = SAMPLE.replacen(
            "bgpAsnEnd: 4244767060",
            "bgpAsnEnd: 4244766862\n  asnLayout: { siteControllerRoutes: 4244766860 }",
            1,
        );
        assert_ne!(text, SAMPLE, "splice target missing");
        let sd = parse(&text).expect("parses");
        let rows = run(&sd, Register::builtin());
        let row = rows
            .iter()
            .find(|r| r.id == "sitedef.network.asn-layout" && r.status == Status::Fail)
            .expect("a failing asn-layout row");
        assert!(
            row.observed.contains("nodeId 3 would take 4244766863"),
            "{}",
            row.observed
        );
    }

    /// `dpf` is read from the command when it is not pinned, so a conflict that depends
    /// on it fires at the gate.
    /// An external RMS runs with `--skip-rms`.
    #[test]
    fn an_external_rms_passes_the_gate_with_skip_rms() {
        let text = SAMPLE
            .replacen(
                "  optional-components: [REST group]",
                "  optional-components: [REST group, RMS]",
                1,
            )
            .replacen(
                "  build-host: Yes\n",
                "  build-host: Yes\n  rms: External RMS\n",
                1,
            );
        let sd = parse(&text).expect("parses");
        let rows = run(&sd, Register::builtin());
        let failing: Vec<_> = rows
            .iter()
            .filter(|r| r.id == "sitedef.setup.command" && r.status == Status::Fail)
            .collect();
        assert!(failing.is_empty(), "{failing:#?}");
    }

    /// An external observability stack runs without `--with-observability`.
    #[test]
    fn an_external_observability_stack_passes_the_gate_without_the_switch() {
        let text = SAMPLE.replacen(
            "  build-host: Yes\n",
            "  build-host: Yes\n  observability: External stack\n",
            1,
        );
        let sd = parse(&text).expect("parses");
        let rows = run(&sd, Register::builtin());
        let failing: Vec<_> = rows
            .iter()
            .filter(|r| r.id == "sitedef.setup.command" && r.status == Status::Fail)
            .collect();
        assert!(failing.is_empty(), "{failing:#?}");
    }

    /// The example implies `rms: Not applicable` from its component list; a command that
    /// would install RMS anyway is caught at the gate.
    #[test]
    fn a_command_against_an_implied_decision_fails_the_gate() {
        let text = SAMPLE.replacen(
            "--metallb-config values/metallb-config.yaml --skip-rms",
            "--metallb-config values/metallb-config.yaml",
            1,
        );
        assert_ne!(text, SAMPLE, "splice target missing");
        let sd = parse(&text).expect("parses");
        let rows = run(&sd, Register::builtin());
        let row = rows
            .iter()
            .find(|r| r.id == "sitedef.setup.command" && r.status == Status::Fail)
            .expect("a failing setup row");
        assert!(
            row.observed.contains("rms `In-cluster nv-rms`")
                && row.observed.contains("implied by optional-components"),
            "{}",
            row.observed
        );
    }

    #[test]
    fn an_ipv6_gateway_on_the_last_or_zero_address_is_accepted() {
        for gateway in ["fd00:1::3", "fd00:1::"] {
            let text = SAMPLE.replacen(
                "controlPlaneOob: { prefix: 10.10.248.48/28, gateway: 10.10.248.62, mtu: 1500 }",
                &format!("controlPlaneOob: {{ prefix: \"fd00:1::/126\", gateway: \"{gateway}\", mtu: 1500 }}"),
                1,
            );
            assert_ne!(text, SAMPLE, "splice target missing");
            let sd = parse(&text).expect("parses");
            let rows = run(&sd, Register::builtin());
            let gateway_rows: Vec<_> = rows
                .iter()
                .filter(|r| r.id == "sitedef.network.gateway-inside" && r.status == Status::Fail)
                .collect();
            assert!(gateway_rows.is_empty(), "{gateway}: {gateway_rows:#?}");
        }
    }

    #[test]
    fn a_skip_dpf_command_makes_a_bf4_fleet_fail_the_gate() {
        let text = SAMPLE
            .replacen(
                "--metallb-config values/metallb-config.yaml --skip-rms",
                "--metallb-config values/metallb-config.yaml --skip-rms --skip-dpf",
                1,
            )
            .replacen(
                "  build-host: Yes\n",
                "  build-host: Yes\n  managed-host-dpu-generation: BF4 generic\n",
                1,
            );
        assert_ne!(text, SAMPLE, "splice targets missing");
        let sd = parse(&text).expect("parses");
        let effective = effective_decisions(&sd, Register::builtin());
        assert_eq!(effective.values.get("dpf"), Some(&vec!["Skip".to_string()]));
        assert_eq!(
            effective.implied_by.get("dpf").map(String::as_str),
            Some("setup.command")
        );
        let rows = run(&sd, Register::builtin());
        assert!(
            rows.iter()
                .any(|r| r.status == Status::Fail && r.observed.contains("BF4 generic")),
            "{rows:#?}"
        );
    }

    #[test]
    fn duplicate_or_datacenter_controller_asns_fail_the_gate() {
        for (name, layout, needle) in [
            (
                "two controllers on one ASN",
                "  asnLayout: { siteControllers: { 2: 4244766861 } }",
                "nodeId 1 and nodeId 2 both take 4244766861",
            ),
            (
                "a controller on the datacenter ASN",
                "  asnLayout: { siteControllers: { 3: 65034 } }",
                "nodeId 3 takes 65034, the datacenter ASN",
            ),
        ] {
            let text = SAMPLE.replacen(
                "bgpAsnEnd: 4244767060",
                &format!("bgpAsnEnd: 4244767060\n{layout}"),
                1,
            );
            let sd = parse(&text).expect("parses");
            let rows = run(&sd, Register::builtin());
            let hit = rows.iter().any(|r| {
                r.id == "sitedef.network.asn-layout"
                    && r.status == Status::Fail
                    && r.observed.contains(needle)
            });
            assert!(hit, "{name}: {rows:#?}");
        }
    }

    #[test]
    fn a_probe_on_an_unknown_segment_fails_the_gate() {
        let text = SAMPLE.replacen(
            "{ name: rack1, address: 10.10.72.250 }",
            "{ name: rack9, address: 10.10.72.250 }",
            1,
        );
        assert_ne!(text, SAMPLE, "splice target missing");
        let sd = parse(&text).expect("parses");
        let rows = run(&sd, Register::builtin());
        let row = rows
            .iter()
            .find(|r| r.id == "sitedef.hosts.probe-hosts")
            .expect("row");
        assert_eq!(row.status, Status::Fail);
        assert!(row.observed.contains("rack9"), "{}", row.observed);
    }

    #[test]
    fn implications_fill_in_what_the_stated_decisions_imply() {
        let sd = parse(SAMPLE).expect("sample parses");
        let effective = effective_decisions(&sd, Register::builtin());
        let one = |s: &str| vec![s.to_string()];
        // An air-gapped staging host implies the external registry answer.
        assert_eq!(
            effective.values.get("external-registry-access"),
            Some(&one("Not applicable"))
        );
        assert_eq!(
            effective
                .implied_by
                .get("external-registry-access")
                .map(String::as_str),
            Some("staging-host")
        );
        // A `lacks` rule: the pinned Optional components list has no RMS box, so
        // rms is implied Not applicable, and through it rack-profiles.
        assert_eq!(effective.values.get("rms"), Some(&one("Not applicable")));
        assert_eq!(
            effective
                .implied_by
                .get("rack-profiles")
                .map(String::as_str),
            Some("rms")
        );
        // Stated entries are never overwritten by an implication.
        assert_eq!(effective.values.get("build-host"), Some(&one("Yes")));
        assert!(!effective.implied_by.contains_key("build-host"));
    }

    /// One broken value per invariant; each case names the row id that must fail
    /// and a fragment the row's observed text must carry.
    #[test]
    fn each_invariant_names_its_offender() {
        struct Case {
            name: &'static str,
            from: &'static str,
            to: &'static str,
            row: &'static str,
            observed: &'static str,
        }
        let cases = [
            Case {
                name: "unknown decision",
                from: "  build-host: Yes\n",
                to: "  build-host: Yes\n  colour-scheme: blue\n",
                row: "sitedef.decisions.colour-scheme",
                observed: "not in the register",
            },
            Case {
                name: "invalid option",
                from: "staging-host: Air-gapped with a transfer path",
                to: "staging-host: Sometimes connected",
                row: "sitedef.decisions.staging-host",
                observed: "Sometimes connected",
            },
            Case {
                name: "stated decision absent",
                from: "  build-host: Yes\n",
                to: "",
                row: "sitedef.decisions.build-host",
                observed: "absent",
            },
            Case {
                name: "implied decision may be stated when it agrees",
                from: "  build-host: Yes\n",
                to: "  build-host: Yes\n  external-registry-access: Not applicable\n",
                row: "sitedef.decisions",
                observed: "6 stated",
            },
            Case {
                name: "stated value contradicts an implication",
                from: "  build-host: Yes\n",
                to: "  build-host: Yes\n  external-registry-access: Direct\n",
                row: "sitedef.decisions.external-registry-access",
                observed: "`Direct`",
            },
            Case {
                name: "conflict between held options",
                from: "  build-host: Yes\n",
                to: "  build-host: Yes\n  source-of-the-nico-images: Pulled from NGC\n",
                row: "sitedef.decisions.conflict",
                observed: "source-of-the-nico-images = `Pulled from NGC`",
            },
            Case {
                name: "a Postgres store with an empty KEK-provider list",
                from: "  build-host: Yes\n",
                to: "  build-host: Yes\n  credential-store: Postgres envelope-encrypted\n  key-wrapping-for-the-postgres-store: []\n",
                row: "sitedef.decisions.conflict",
                observed: "key-wrapping-for-the-postgres-store",
            },
            Case {
                name: "conflict with an unticked box",
                from: "  build-host: Yes\n",
                to: "  build-host: Yes\n  rack-profiles: Built-in profiles\n",
                row: "sitedef.decisions.conflict",
                observed: "rack-profiles = `Built-in profiles`",
            },
            Case {
                name: "list given to a single-select",
                from: "  build-host: Yes\n",
                to: "  build-host: [Yes, No]\n",
                row: "sitedef.decisions.build-host",
                observed: "2 values",
            },
            Case {
                name: "misaligned prefix",
                from: "forgeServiceVipPrefix: 10.10.126.192/27",
                to: "forgeServiceVipPrefix: 10.10.126.193/27",
                row: "sitedef.network.prefix-aligned",
                observed: "network.forgeServiceVipPrefix",
            },
            Case {
                name: "overlapping prefixes",
                from: "admin:           { prefix: 10.10.60.0/24, gateway: 10.10.60.1",
                to: "admin:           { prefix: 10.10.63.128/26, gateway: 10.10.63.129",
                row: "sitedef.network.no-overlap",
                observed: "network.segments.admin.prefix",
            },
            Case {
                name: "gateway outside its prefix",
                from: "gateway: 10.10.248.62, mtu: 1500 }",
                to: "gateway: 10.10.248.1, mtu: 1500 }",
                row: "sitedef.network.gateway-inside",
                observed: "network.segments.controlPlaneOob.gateway",
            },
            Case {
                name: "an unterminated quote in the setup command",
                from: "--skip-rms",
                to: "--skip-rms \"--skip-dpf",
                row: "sitedef.setup.command",
                observed: "unterminated double quote",
            },
            Case {
                name: "an install variable setup.sh would reject",
                from: "command: ./setup.sh",
                to: "command: NICO_INSTALL_DPF=yes ./setup.sh",
                row: "sitedef.setup.command",
                observed: "NICO_INSTALL_DPF must be true or false",
            },
            Case {
                name: "gateway on the network address",
                from: "gateway: 10.10.248.62, mtu: 1500 }",
                to: "gateway: 10.10.248.48, mtu: 1500 }",
                row: "sitedef.network.gateway-inside",
                observed: "the network address",
            },
            Case {
                name: "VLAN pool beyond 4094",
                from: "vlan: { start: 100, end: 501 }",
                to: "vlan: { start: 100, end: 4095 }",
                row: "sitedef.network.pool-range",
                observed: "network.pools.vlan",
            },
            Case {
                name: "inverted pool",
                from: "vlan: { start: 100, end: 501 }",
                to: "vlan: { start: 501, end: 100 }",
                row: "sitedef.network.pool-order",
                observed: "network.pools.vlan",
            },
            Case {
                name: "datacenter ASN inside the per-DPU pool",
                from: "datacenterAsn: 65034",
                to: "datacenterAsn: 4244766865",
                row: "sitedef.network.asn-layout",
                observed: "network.datacenterAsn",
            },
            Case {
                name: "ASN range too small for the site controllers and a DPU",
                from: "bgpAsnEnd: 4244767060",
                to: "bgpAsnEnd: 4244766863",
                row: "sitedef.network.asn-layout",
                observed: "no ASN left for DPUs",
            },
            Case {
                name: "ASN override for a node that does not exist",
                from: "bgpAsnEnd: 4244767060",
                to: "bgpAsnEnd: 4244767060\n  asnLayout: { siteControllers: { 7: 4244766867 } }",
                row: "sitedef.network.asn-layout",
                observed: "nodeId 7",
            },
            Case {
                name: "node id gap",
                from: "nodeId: 3, bmc: 10.10.248.51",
                to: "nodeId: 4, bmc: 10.10.248.51",
                row: "sitedef.hosts.node-ids",
                observed: "[1, 2, 4]",
            },
            Case {
                name: "duplicate hostname",
                from: "hostName: sc3,",
                to: "hostName: sc1,",
                row: "sitedef.hosts.unique",
                observed: "hostName `sc1`",
            },
            Case {
                name: "unparsable MAC",
                from: "p0Mac: \"aa:bb:cc:dd:ee:03\"",
                to: "p0Mac: \"not-a-mac\"",
                row: "sitedef.hosts.p0-mac",
                observed: "sc3: `not-a-mac`",
            },
        ];
        for case in &cases {
            assert!(
                SAMPLE.contains(case.from),
                "{}: fixture anchor missing",
                case.name
            );
            let sd = parse(&SAMPLE.replacen(case.from, case.to, 1))
                .unwrap_or_else(|e| panic!("{}: fixture must parse: {e}", case.name));
            let rows = run(&sd, Register::builtin());
            let hit = rows
                .iter()
                .find(|r| r.id == case.row && r.observed.contains(case.observed));
            assert!(
                hit.is_some(),
                "{}: no row {} with `{}` in {:#?}",
                case.name,
                case.row,
                case.observed,
                rows
            );
            let expect_pass = case.row == "sitedef.decisions";
            assert_eq!(
                hit.map(|r| r.status == Status::Pass),
                Some(expect_pass),
                "{}: status",
                case.name
            );
        }
    }
}
