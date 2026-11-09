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

//! `nico-pre-config`: the collaterals against each other and against the site definition, from a laptop.

use std::net::IpAddr;

use ipnet::IpNet;

use crate::harness::{Check, Ctx, Outcome, todo};

pub(crate) const CHECKS: &[Check] = &[
    Check {
        id: "config.collaterals.present",
        expected: "every collateral the site definition names exists",
        detail: "Reads: every path under `collaterals`, resolved against the site definition's \
                 directory; the `dpu-in-the-site-controllers` decision. Reads no file contents. \
                 `metallbConfig` may be a kustomize directory, as `setup.sh --metallb-config` \
                 accepts: a directory holding a `kustomization.yaml`.\n\
                 Pass: every named path exists; observed `7 files present`.\n\
                 Warn: `cpDpuSiteYaml` named while the site controllers carry no DPU; fix: drop \
                 it or correct the decision. `expectedMachines` named but not present yet: it is \
                 read from step 7, so before then its absence is a warning, not a failure.\n\
                 Fail: `cpDpuSiteYaml` absent while the decision is a DPU, or any named path \
                 missing; observed names the key and the path (`coreValues: \
                 ./helm-prereqs/values/nico-core.yaml`); fix: correct the path or put the file \
                 there.\n\
                 Skip: never.\n\
                 Test: the example site-def against a directory of empty files; cases: all \
                 present passes; one missing fails naming key and path; `cpDpuSiteYaml` absent \
                 with a DPU fails; named with `no DPU` warns; only `expectedMachines` absent \
                 warns.",
        run: collaterals_present,
    },
    Check {
        id: "config.segments.admin",
        expected: "the admin segment fits the fleet: usable addresses beyond the reserved block cover counts.hosts with headroom, and its MTU is within pathMtu",
        detail: "Reads: `network.segments.admin`, `network.pathMtu`, `counts.hosts`, the \
                 `managed-host-kinds-in-the-fleet` decision; `reserve_first` from the admin \
                 table in the TOML inside `collaterals.coreValues` whose `prefix` or \
                 `prefix_v6` is the planned admin prefix, or the only admin table when none \
                 names it (5, the default, when the file is absent or names no \
                 `reserve_first`; a file that does not parse, several admin tables none with \
                 the planned prefix, or a `reserve_first` that is not a non-negative integer, \
                 is a tool error for this row).\n\
                 Pass: usable addresses, counted as Core's allocator does (the prefix less the \
                 first `reserve_first` addresses from the network address, 5 by default, less \
                 the broadcast address, less the gateway when it lies outside the reserved \
                 block), at or above `counts.hosts` with 20% spare, and the segment MTU within \
                 `pathMtu`; observed `10.10.60.0/24: 250 usable for 128 hosts, mtu 9000 within \
                 pathMtu 9000`.\n\
                 Warn: under 20% spare; observed `leaves 9 spare addresses for 240 hosts`; fix: \
                 widen the prefix before the fleet grows. A mixed fleet (Zero-DPU hosts among \
                 the kinds) that does not fit: `counts.hosts` is only an upper bound on the \
                 admin demand, so the shortfall is a warning naming both numbers; fix: confirm \
                 the DPU hosts fit or widen the prefix.\n\
                 Fail: fewer usable addresses than hosts, or MTU above `pathMtu`; observed names \
                 both numbers; fix: widen the prefix or lower `counts.hosts`, keep the MTU within \
                 `pathMtu`.\n\
                 Skip: the address demand (not the MTU) when the fleet is zero-DPU hosts only, \
                 which boot on HostInband and take no admin address. Alignment and the gateway \
                 are the gate's rows and are not repeated.\n\
                 Test: the example spliced one line at a time; cases: 128 hosts passes; 300 \
                 fails; 240 warns; `pathMtu: 1500` fails naming both MTUs; a zero-DPU fleet on \
                 a /29 admin segment passes; a mixed fleet on a /29 warns.",
        run: admin_segment,
    },
    Check {
        id: "config.consistency.prefixes",
        expected: "prefixes across all collaterals aligned and disjoint, gateways inside",
        detail: "Reads: every prefix in the collaterals: the CP-DPU site yaml (control plane, \
                 service VIP, DPU loopback, VPC VRF loopback), the TOML's `[networks.*]` and \
                 `[pools.*]` prefixes with their gateways, the MetalLB pool CIDRs; the Kubernetes \
                 pod and service CIDRs from the site definition, which no collateral repeats.\n\
                 Pass: every prefix aligned to its mask, every pair of distinct prefixes \
                 disjoint, every gateway inside its prefix; observed `N prefixes from 4 files, \
                 disjoint`.\n\
                 Fail: a misaligned prefix, an overlap, or a gateway outside; observed names both \
                 prefixes and the files that hold them (`[networks.rack1] 10.10.60.0/23 \
                 (nico-core.yaml) overlaps [networks.admin] 10.10.60.0/24 (nico-core.yaml)`); \
                 fix: correct the file named.\n\
                 Skip: never. The gate proves the same for site-def alone; this check proves it \
                 for the files. The same prefix in two files is one prefix here; whether it \
                 should be the same is `config.sitedef.agreement`'s row; the same prefix twice in \
                 one file is an overlap. MetalLB pools are carved \
                 from `forgeServiceVipPrefix`: each CIDR or `first-last` range must lie inside it \
                 and none joins the disjointness matrix. The TOML's `deny_prefixes` and `site_fabric_prefixes` are \
                 aggregates and are not listed.\n\
                 Test: the example's collaterals copied and spliced; cases: the consistent set \
                 passes; a TOML segment shifted into another's range fails naming both; a \
                 gateway moved outside its prefix fails; a misaligned prefix in the site yaml \
                 fails.",
        run: todo,
    },
    Check {
        id: "config.consistency.pools",
        expected: "pools sized to counts; the per-DPU ASN range clear",
        detail: "Reads: `forgeControlPlanePrefix`, `forgeServiceVipPrefix`, \
                 `forgeDpuLoopbackPrefix`, the BMC and hostInband segments, `kubernetes.apiVip`, \
                 `counts`, the ASN layout (`bgpAsnStart`, `bgpAsnEnd`, `asnLayout`, \
                 `datacenterAsn`), the decisions `optional-components`, \
                 `recursive-resolver-for-managed-machines` and `dpu-in-the-site-controllers` for \
                 the VIP count, and `nico-ntp.replicaCount` and `nico-dns.replicaCount` in \
                 `collaterals.coreValues` when it loads (3 and 2 otherwise). Where the collaterals repeat the pool values, \
                 `config.sitedef.agreement` compares them.\n\
                 Pass: the control-plane prefix holds a /31 per site controller and the DPU \
                 loopback prefix more addresses than site controllers, as the ISO builder \
                 requires; the service VIP prefix holds the VIPs the ticked components need (api, \
                 dhcp, pxe, sshConsole; one per `nico-dns` replica; REST and Keycloak with the \
                 REST group; one for observability; one per `nico-ntp` replica; the built-in unbound unless the \
                 resolver is external DNS; the Kubernetes API VIP when it lies inside); the per-DPU ASN \
                 pool (the range less the routes ASN and the site-controller ASNs) holds \
                 `counts.dpus`; the BMC segments together hold `counts.hosts`, and so do the \
                 hostInband segments when any is stated; the per-DPU pool does not contain \
                 `datacenterAsn`; observed one clause per pool with capacity and demand.\n\
                 Warn: a pool with under 20% spare.\n\
                 Fail: a pool too small or the datacenter ASN inside the per-DPU pool; observed \
                 the pool, its capacity and the demand (`DPU ASNs: 6 for 128 dpus`); fix names \
                 the field to widen.\n\
                 Skip: the DPU loopback row when `dpu-in-the-site-controllers` is `no DPU`; the \
                 hostInband row when no hostInband segment is stated; never the check. The DPU \
                 rows pass with `0 dpus` observed when `counts.dpus` is 0. The tenant \
                 routing-profile ASN lives in the TOML and is `config.toml.rules`' row.\n\
                 Test: the example spliced; cases: the example passes; `counts.dpus: 200` fails \
                 the ASN row; `datacenterAsn` set inside the per-DPU range fails; a hostInband \
                 prefix narrowed to /26 for 128 hosts fails; `counts.dpus: 0` passes; the \
                 control-plane prefix narrowed to /30 fails its row.",
        run: todo,
    },
    Check {
        id: "config.consistency.site-controllers",
        expected: "at least three site controllers and Postgres sized to the host count",
        detail: "Reads: `hosts.siteControllers`, `counts.hosts`, the Postgres connection limit \
                 and PVC size in the helm-prereqs values, the sizing table (a built-in \
                 placeholder until the provider guide lands).\n\
                 Pass: three or more site controllers; connection limit and PVC size at or above \
                 the table row for `counts.hosts`; observed `3 controllers; postgres 500 \
                 connections, 200Gi for 128 hosts`.\n\
                 Warn: the sizing table is still the placeholder (reported on every run until the provider \
                 guide lands).\n\
                 Fail: fewer than three controllers; a Postgres value below the row; observed \
                 names the value and the minimum; fix: add a controller or raise the value in the \
                 helm-prereqs values.\n\
                 Skip: the Postgres rows when `core-postgres` is `External Postgres`; the \
                 controller count never.\n\
                 Test: the example spliced plus a helm-prereqs values fixture; cases: the example \
                 passes with the placeholder warning; two controllers fails; a PVC below the row \
                 fails naming both sizes; external Postgres skips the Postgres rows only.",
        run: todo,
    },
    Check {
        id: "config.sitedef.agreement",
        expected: "every quantity present in both equal, key for key",
        detail: "Reads: every quantity that appears both in site-def and in a collateral: \
                 segments and prefixes, VLANs and VNIs, ASNs, `apiHostname`, name and NTP \
                 servers, VIPs, `versions` as image tags, controller names and BMC addresses; \
                 through the collateral loaders.\n\
                 Pass: every pair equal; observed `N keys compared across 5 files, all equal`.\n\
                 Fail: any pair differs; observed one clause per mismatch with the key, both \
                 values and the file (`network.nameServer 10.10.0.11 vs nico-core.yaml [dns] \
                 upstream 10.10.0.13`); fix: pick one as the source and copy it.\n\
                 Skip: never. A key absent from a file is not reported here; that is the file's \
                 `.rules` check.\n\
                 Test: a fixture directory of collaterals agreeing with the example; cases: the \
                 agreeing set passes; one value changed in the TOML fails naming key, both values \
                 and file; the same value changed in site-def fails identically; a key absent \
                 from one file is not a mismatch.",
        run: todo,
    },
    Check {
        id: "config.cp-dpu-yaml.rules",
        expected: "the CP-DPU site yaml rows of the register",
        detail: "Reads: `collaterals.cpDpuSiteYaml`, parsed with the schema of \
                 `scripts/control-plane-dpu`; the `smn` decision; the site-def network values.\n\
                 Pass: required fields present, the `fnn` block present when SMN is on the \
                 control-plane DPU, values equal to site-def, and the builder's own validations \
                 hold (schema, ASN and prefix sizes, `routeTargetsToImport` shape, leaf-password \
                 rules), re-expressed in Rust with the builder unchanged; observed `site.yaml: N \
                 fields, builder rules hold`.\n\
                 Warn: a placeholder `p0` MAC; observed names the controller.\n\
                 Fail: a required field missing, a builder rule violated, or a value differing \
                 from site-def; observed quotes the rule as the builder would print it; fix: edit \
                 the field named.\n\
                 Skip: when `dpu-in-the-site-controllers` is `no DPU`.\n\
                 Test: `scripts/control-plane-dpu/site-sample.yaml` adapted to the example; \
                 cases: the sample passes; `fnn` removed with SMN on the DPU fails; a \
                 `routeTargetsToImport` entry in the wrong shape fails with the builder's \
                 message; a placeholder MAC warns; `no DPU` skips.",
        run: todo,
    },
    Check {
        id: "config.toml.rules",
        expected: "the NICo TOML inside the Core values parses with the real config types and meets its rows",
        detail: "Reads: the TOML string under `nicoApiSiteConfig` in `collaterals.coreValues`, \
                 parsed with the config types of `crates/api-core/src/cfg`; the decisions \
                 `managed-host-kinds-in-the-fleet`, `credential-store`, `evpn-overlay-peering`, \
                 `measured-boot-attestation`; the site-def network values.\n\
                 Pass: the TOML parses with the real types; site identity equals `site`; one \
                 network segment per managed host kind and address family, each MTU within \
                 `pathMtu`; `dpu_policy` equals the decision; routing profiles and route targets \
                 equal `network.fnn`; the network security prefixes include every BMC and \
                 control-plane prefix; the deprecated anycast key absent; observed \
                 `nico-core.yaml: TOML parses, N rules hold`.\n\
                 Warn: the deprecated anycast key present; an optional table absent that a ticked \
                 component needs.\n\
                 Fail: a parse error (quoted with its line), a missing segment for a host kind, a \
                 segment MTU above `pathMtu`, `dpu_policy` not matching the decision, a route \
                 target differing; observed names the table and key; fix names the key to edit.\n\
                 Skip: never.\n\
                 Test: a nico-core values fixture carrying a TOML known to parse (the nico-dev \
                 one); cases: the fixture passes; a misspelled key fails with the parser's line; \
                 `dpu_policy` flipped fails naming both values; a segment MTU above `pathMtu` \
                 fails; the anycast key added warns.",
        run: todo,
    },
    Check {
        id: "config.helm-prereqs.rules",
        expected: "the helm-prereqs values rows",
        detail: "Reads: `collaterals.helmPrereqsValues`, `restValues`, `siteAgentValues`; the \
                 decisions `recursive-resolver-for-managed-machines`, `credential-store`, \
                 `managed-host-dpu-generation`, `rest-api-auth`, `core-postgres`, `root-ca`.\n\
                 Pass: MetalLB mode and BGPPeer node selectors as the site controllers; every VIP \
                 the pods need inside the pools with the per-pod VIP counts; the DHCP server \
                 identifier and relay placeholders set; unbound local data or the external \
                 resolver per the decision; NTP equal to `ntpServers`; Kea rendering, hook \
                 architecture and memfile set; the Vault KV seed list with the BF4 entry when \
                 that generation is chosen; REST common values, Keycloak and Temporal flags per \
                 `rest-api-auth`; site-agent values pointing at the Core API; observed \
                 `values.yaml: N rules hold`.\n\
                 Warn: an optional value left at the chart default where the site differs (NTP, \
                 resolver).\n\
                 Fail: a rule violated; observed names the key, the value and the expectation; \
                 fix names the key.\n\
                 Skip: the REST rows when `optional-components` lacks `REST group`; the Vault \
                 rows when `credential-store` is `Postgres envelope-encrypted`.\n\
                 Test: the nico-dev values files as fixture; cases: the fixture passes; a VIP \
                 moved outside the pool fails; NTP differing from site-def fails; `BF4 generic` \
                 chosen without its seed entry fails; REST unticked skips the REST rows.",
        run: todo,
    },
    Check {
        id: "config.metallb.rules",
        expected: "MetalLB pools hold every service VIP; peers match the site controllers",
        detail: "Reads: `collaterals.metallbConfig` (IPAddressPools, BGPPeers, \
                 BGPAdvertisements); `hosts.siteControllers` with `torPeers` when present, the \
                 ASN layout, `network.serviceVips` when pinned.\n\
                 Pass: every service VIP inside a pool; one BGPPeer per site controller (or per \
                 ToR with `no DPU`) with `myASN`, `peerASN` and `peerAddress` as the layout \
                 states; observed `6 VIPs in 1 pool; 3 peers match`.\n\
                 Fail: a VIP outside every pool; a peer missing, or with a wrong ASN or address; \
                 observed names the VIP or the peer and both values; fix: edit the pool or the \
                 peer.\n\
                 Skip: never.\n\
                 Test: a metallb-config fixture matching the example; cases: the fixture passes; \
                 a pool narrowed so a VIP falls outside fails naming it; one peer's `peerASN` off \
                 by one fails naming both; with `no DPU` and `torPeers` the peers are compared \
                 with the ToRs.",
        run: todo,
    },
    Check {
        id: "config.setup.command",
        expected: "--skip-* flags agree with the decisions",
        detail: "Reads: `setup.command`; the decisions `optional-components`, `dpf`, `rms`, \
                 `observability`; `collaterals.coreValues` and `metallbConfig`.\n\
                 Pass: every `--skip-*` and `--with-*` flag agrees with the decisions \
                 (`--skip-rms` with RMS unticked, no `--skip-dpf` with `dpf: Install`); \
                 `--core-values` and `--metallb-config` name the collaterals; observed the flags \
                 found.\n\
                 Fail: a flag contradicting a decision, or missing for an unticked component; a \
                 values flag naming another file; observed names the flag and the decision; fix: \
                 edit the command or the decision.\n\
                 Skip: never.\n\
                 Test: `setup.command` spliced in the example; cases: the example passes; \
                 `--skip-rms` removed with RMS unticked fails; `--skip-dpf` added with `Install` \
                 fails; `--core-values other.yaml` fails naming both paths.",
        run: todo,
    },
    Check {
        id: "config.setup.env",
        expected: "required NICO_* variables set for the ticked components; image tags equal versions",
        detail: "Reads: `setup.env`; `versions`; the decisions `dpf`, `registry-authentication`, \
                 `rms`, `observability`; the TOML's `[dpf] helm_repo_url`.\n\
                 Pass: every `NICO_*` variable the ticked components need present by name (a `{ \
                 from: ... }` source counts as present); `NICO_CORE_IMAGE_TAG` equals \
                 `versions.infraController`, `NICO_REST_IMAGE_TAG` equals `versions.nicoRest`; \
                 `NICO_DPF_HELM_REPO_CARBIDE` equals the TOML's `helm_repo_url`; observed `12 \
                 variables; tags equal versions`.\n\
                 Fail: a required variable absent, a tag differing from `versions`, the DPF \
                 repository differing from the TOML; observed names the variable and both values; \
                 fix: set it.\n\
                 Skip: the DPF variables when `dpf` is `Skip`.\n\
                 Test: `setup.env` spliced; cases: the example passes; `NICO_CORE_IMAGE_TAG` \
                 changed fails naming both tags; `NICO_DPF_DPU_INTERFACE` removed with `Install` \
                 fails; the same with `Skip` passes.",
        run: todo,
    },
    Check {
        id: "config.derived.decisions",
        expected: "derived decisions read from the files agree with any pinned value",
        detail: "Reads: every register entry with `stated: false`, read from its source (the \
                 setup command, `setup.env`, the TOML tables, the helm values) through the \
                 collateral loaders; the pinned values under `decisions`.\n\
                 Pass: every derived entry resolves to one option and every pinned value equals \
                 what the files say; observed `N derived, M pinned, all agree`.\n\
                 Fail: a pinned value the files contradict (`dpf: pinned Skip, setup.command says \
                 Install`), or a derived entry the files leave ambiguous; fix: drop the pin or \
                 change the file.\n\
                 Skip: never.\n\
                 Test: `decisions` spliced with a fixture directory; cases: the example passes; \
                 `dpf: Skip` pinned against the example command fails naming both; \
                 `credential-store` pinned to the value the TOML holds passes; a TOML without \
                 `[secrets]` leaves `credential-store` ambiguous and fails.",
        run: todo,
    },
];

/// Every path under `collaterals` resolves to a file, and the CP-DPU site yaml
/// is named exactly when the site controllers carry a DPU. Written first because
/// every other row of this checklist reads one of them, and as the worked example
/// of a check: look at `ctx`, return what you observed, the harness does the rest.
fn collaterals_present(ctx: &Ctx) -> eyre::Result<Outcome> {
    let c = &ctx.sd.collaterals;
    let no_dpu = ctx.decided("dpu-in-the-site-controllers", "no DPU");
    let named: Vec<(&str, &std::path::Path)> = [
        ("cpDpuSiteYaml", c.cp_dpu_site_yaml.as_deref()),
        ("helmPrereqsValues", Some(c.helm_prereqs_values.as_path())),
        ("coreValues", Some(c.core_values.as_path())),
        ("restValues", Some(c.rest_values.as_path())),
        ("siteAgentValues", Some(c.site_agent_values.as_path())),
        ("metallbConfig", Some(c.metallb_config.as_path())),
        ("expectedMachines", c.expected_machines.as_deref()),
    ]
    .into_iter()
    .filter_map(|(k, p)| p.map(|p| (k, p)))
    .collect();
    // Every problem in one row, so one run shows everything to fix.
    let mut missing: Vec<String> = Vec::new();
    if !no_dpu && c.cp_dpu_site_yaml.is_none() {
        missing.push(
            "cpDpuSiteYaml: not named, while dpu-in-the-site-controllers is a DPU".to_string(),
        );
    }
    // `expectedMachines` is read from step 7; a provider may name it before writing it.
    let mut later: Vec<String> = Vec::new();
    let present = |k: &str, p: &std::path::Path| {
        let path = ctx.resolve(p);
        path.is_file()
            || (k == "metallbConfig"
                && ["kustomization.yaml", "kustomization.yml", "Kustomization"]
                    .iter()
                    .any(|f| path.join(f).is_file()))
    };
    for (k, p) in named.iter().filter(|(k, p)| !present(k, p)) {
        if *k == "expectedMachines" {
            later.push(format!("{k}: {}", p.display()));
        } else {
            missing.push(format!("{k}: {}", p.display()));
        }
    }
    if !missing.is_empty() {
        return Ok(Outcome::Fail {
            observed: format!("missing {}", missing.join(", ")),
            fix: "fix the path in site-def.yaml or put the file there; name the \
                  scripts/control-plane-dpu site yaml when the controllers carry a DPU"
                .to_string(),
        });
    }
    if no_dpu && c.cp_dpu_site_yaml.is_some() {
        return Ok(Outcome::Warn {
            observed: "cpDpuSiteYaml named although the site controllers carry no DPU".to_string(),
            fix: "drop it, or correct dpu-in-the-site-controllers".to_string(),
        });
    }
    if !later.is_empty() {
        return Ok(Outcome::Warn {
            observed: format!("not yet present, needed from step 7: {}", later.join(", ")),
            fix: "write the expected-machines file before the ingest checklist".to_string(),
        });
    }
    Ok(Outcome::Pass(format!("{} files present", named.len())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::register::Register;

    #[test]
    fn a_missing_collateral_names_the_key_and_path() {
        let sd = crate::sitedef::parse(include_str!("../../examples/site-def.yaml")).unwrap();
        let dec = crate::gate::effective_decisions(&sd, Register::builtin());
        let ctx = Ctx {
            sd: &sd,
            site_def_path: std::path::Path::new("/nonexistent/site-def.yaml"),
            decisions: &dec,
            evidence: None,
        };
        match collaterals_present(&ctx).unwrap() {
            Outcome::Fail { observed, .. } => {
                assert!(observed.contains("coreValues:"), "{observed}");
            }
            other => panic!("expected a failure, got {other:?}"),
        }
    }

    /// `setup.sh --metallb-config` takes a file or a kustomize directory; so does the row.
    #[test]
    fn a_kustomize_directory_is_a_present_metallb_config() {
        let dir = tempfile::tempdir().unwrap();
        let sd = crate::sitedef::parse(include_str!("../../examples/site-def.yaml")).unwrap();
        let c = &sd.collaterals;
        for rel in [
            c.cp_dpu_site_yaml.as_deref().unwrap(),
            &c.helm_prereqs_values,
            &c.core_values,
            &c.rest_values,
            &c.site_agent_values,
            c.expected_machines.as_deref().unwrap(),
        ] {
            let path = dir.path().join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, "").unwrap();
        }
        let kustomize = dir.path().join(&c.metallb_config);
        std::fs::create_dir_all(&kustomize).unwrap();
        std::fs::write(kustomize.join("kustomization.yaml"), "resources: []\n").unwrap();
        let dec = crate::gate::effective_decisions(&sd, Register::builtin());
        let site_def_path = dir.path().join("site-def.yaml");
        let ctx = Ctx {
            sd: &sd,
            site_def_path: &site_def_path,
            decisions: &dec,
            evidence: None,
        };
        assert!(
            matches!(collaterals_present(&ctx).unwrap(), Outcome::Pass(_)),
            "{:?}",
            collaterals_present(&ctx).unwrap()
        );
        std::fs::remove_file(kustomize.join("kustomization.yaml")).unwrap();
        match collaterals_present(&ctx).unwrap() {
            Outcome::Fail { observed, .. } => {
                assert!(observed.contains("metallbConfig:"), "{observed}")
            }
            other => panic!("a bare directory is not a kustomize directory: {other:?}"),
        }
    }

    /// The expected-machines file is a step-7 input; naming it early is not a failure.
    #[test]
    fn an_absent_expected_machines_file_only_warns() {
        let dir = tempfile::tempdir().unwrap();
        let sd = crate::sitedef::parse(include_str!("../../examples/site-def.yaml")).unwrap();
        let c = &sd.collaterals;
        for rel in [
            c.cp_dpu_site_yaml.as_deref().unwrap(),
            &c.helm_prereqs_values,
            &c.core_values,
            &c.rest_values,
            &c.site_agent_values,
            &c.metallb_config,
        ] {
            let path = dir.path().join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, "").unwrap();
        }
        let dec = crate::gate::effective_decisions(&sd, Register::builtin());
        let site_def_path = dir.path().join("site-def.yaml");
        let ctx = Ctx {
            sd: &sd,
            site_def_path: &site_def_path,
            decisions: &dec,
            evidence: None,
        };
        match collaterals_present(&ctx).unwrap() {
            Outcome::Warn { observed, .. } => {
                assert!(observed.contains("expectedMachines:"), "{observed}");
            }
            other => panic!("expected a warning, got {other:?}"),
        }
    }
}

/// Every address of a prefix, network and broadcast included; an IPv6 prefix wider
/// than 64 bits of hosts saturates, which is "more than any fleet".
fn addresses(p: &IpNet) -> u64 {
    match p {
        IpNet::V4(p) => 1u64 << (32 - u32::from(p.prefix_len())),
        IpNet::V6(p) => {
            let host_bits = 128 - u32::from(p.prefix_len());
            if host_bits >= 64 {
                u64::MAX
            } else {
                1u64 << host_bits
            }
        }
    }
}

/// Addresses hosts can take, counted as Core's allocator (`ip_allocator.rs`) reserves
/// them: the first `reserved` addresses from the network address, the broadcast address
/// (both only when the prefix is wide enough to have them), and the gateway when it lies
/// outside the reserved block; duplicates count once.
fn usable(p: &IpNet, reserved: u64, gateway: Option<IpAddr>) -> u64 {
    let size = addresses(p);
    let has_ends = match p {
        IpNet::V4(v4) => v4.prefix_len() < 31,
        IpNet::V6(v6) => v6.prefix_len() < 127,
    };
    let mut taken = reserved.min(size);
    if has_ends {
        taken = taken.max(1); // the network address is the first reserved one
        taken += 1; // broadcast
    }
    // Addresses from the start of the prefix that are already counted above.
    let head = if has_ends { reserved.max(1) } else { reserved };
    let gateway_index = gateway.and_then(|g| match (p, g) {
        (IpNet::V4(p), IpAddr::V4(g)) if p.contains(&g) => {
            Some(u64::from(u32::from(g) - u32::from(p.network())))
        }
        (IpNet::V6(p), IpAddr::V6(g)) if p.contains(&g) => {
            u64::try_from(u128::from(g) - u128::from(p.network())).ok()
        }
        _ => None,
    });
    if let Some(i) = gateway_index
        && i >= head
        && !(has_ends && i == size - 1)
    {
        taken += 1;
    }
    size.saturating_sub(taken)
}

#[cfg(test)]
mod usable_tests {
    use super::*;

    /// The counts Core's allocator leaves, on the boundaries a reviewer asked about.
    #[test]
    fn usable_counts_match_the_allocator() {
        let cases: &[(&str, u64, Option<&str>, u64)] = &[
            ("10.10.60.0/24", 5, Some("10.10.60.1"), 250), // gateway inside the block
            ("10.10.60.0/24", 5, Some("10.10.60.254"), 249), // gateway outside it
            ("10.10.60.0/24", 0, Some("10.10.60.0"), 254), // gateway on the network address
            ("fd00:1::/124", 5, Some("fd00:1::1"), 10),
            ("fd00:1::/124", 0, Some("fd00:1::"), 14),
            ("10.10.60.0/31", 0, Some("10.10.60.0"), 1), // point to point: no ends
        ];
        for (prefix, reserved, gateway, want) in cases {
            let p: IpNet = prefix.parse().unwrap();
            let g = gateway.map(|g| g.parse().unwrap());
            assert_eq!(
                usable(&p, *reserved, g),
                *want,
                "{prefix} reserve {reserved} gateway {gateway:?}"
            );
        }
    }
}

/// The admin table's `reserve_first` from the NICo TOML inside the Core values, or the
/// default when the file does not load: Core passes this count to its allocator as the
/// first addresses it never hands out.
fn admin_reserve_first(ctx: &Ctx) -> eyre::Result<u64> {
    use eyre::Context as _;
    let path = ctx.resolve(&ctx.sd.collaterals.core_values);
    if !path.is_file() {
        // Absence is `config.collaterals.present`'s row; this check assumes the default.
        return Ok(ADMIN_RESERVED);
    }
    let text =
        std::fs::read_to_string(&path).wrap_err_with(|| format!("reading {}", path.display()))?;
    let values: serde_yaml::Value =
        serde_yaml::from_str(&text).wrap_err_with(|| format!("parsing {}", path.display()))?;
    // The umbrella chart nests the nico-api subchart's values under `nico-api`.
    let root = values.get("nico-api").unwrap_or(&values);
    // No TOML in the file names no `reserve_first`, so Core's default applies; whether
    // the TOML has to be there is the config checklist's question.
    let Some(toml_text) = root
        .get("siteConfig")
        .and_then(|c| c.get("nicoApiSiteConfig"))
        .and_then(serde_yaml::Value::as_str)
    else {
        return Ok(ADMIN_RESERVED);
    };
    let table: toml::Table = toml_text
        .parse()
        .wrap_err_with(|| format!("{}: the NICo TOML does not parse", path.display()))?;
    // Core allows several admin networks; the planned segment's table is the one whose
    // prefix matches. A lone admin table without a prefix match is still the planned one.
    let planned = ctx.sd.network.segments.admin.prefix;
    let admins: Vec<&toml::Table> = table
        .get("networks")
        .and_then(toml::Value::as_table)
        .map(|networks| {
            networks
                .values()
                .filter_map(toml::Value::as_table)
                .filter(|t| t.get("type").and_then(toml::Value::as_str) == Some("admin"))
                .collect()
        })
        .unwrap_or_default();
    let has_prefix = |t: &toml::Table| {
        ["prefix", "prefix_v6"].into_iter().any(|k| {
            t.get(k)
                .and_then(toml::Value::as_str)
                .and_then(|p| p.parse::<IpNet>().ok())
                .is_some_and(|p| p == planned)
        })
    };
    let admin = match admins.iter().find(|t| has_prefix(t)) {
        Some(t) => Some(*t),
        None if admins.len() <= 1 => admins.first().copied(),
        None => eyre::bail!(
            "{}: {} admin tables in the TOML and none has prefix {planned}",
            path.display(),
            admins.len()
        ),
    };
    let reserved = admin.and_then(|admin| admin.get("reserve_first"));
    // Absent means Core's default; present but not a non-negative integer is a value Core
    // would reject, so it is an error here, not the default.
    let reserved = match reserved {
        None => ADMIN_RESERVED,
        Some(v) => match v.as_integer() {
            Some(n) if n >= 0 => n as u64,
            _ => eyre::bail!(
                "{}: [networks.admin] reserve_first = {v} is not a non-negative integer",
                path.display()
            ),
        },
    };
    Ok(reserved)
}

/// Addresses the TOML's `[networks.admin] reserve_first` keeps back by default,
/// gateway included, before any host gets one.
const ADMIN_RESERVED: u64 = 5;
/// Below this share of spare addresses the segment passes with a warning.
const HEADROOM_PERCENT: u64 = 20;

/// Unit-of-work sample: one row, one function, one test table. The admin
/// segment is where every managed host gets its address, so it has to hold
/// `counts.hosts` beyond the reserved block, with room to grow, and its MTU
/// cannot exceed the path MTU the fabric guarantees. Alignment and the gateway
/// being inside the prefix are the gate's rows and are not repeated here.
fn admin_segment(ctx: &Ctx) -> eyre::Result<Outcome> {
    let n = &ctx.sd.network;
    let seg = &n.segments.admin;
    // Zero-DPU hosts boot on HostInband and take no admin address. The site definition
    // counts hosts without their kind, so for a mixed fleet `counts.hosts` is only an
    // upper bound on the admin demand.
    let kinds = ctx.decision("managed-host-kinds-in-the-fleet");
    let zero_dpu_only =
        kinds.is_some_and(|kinds| !kinds.is_empty() && kinds.iter().all(|k| k == "Zero-DPU hosts"));
    let mixed = !zero_dpu_only && kinds.is_some_and(|k| k.iter().any(|k| k == "Zero-DPU hosts"));
    let hosts = if zero_dpu_only {
        0
    } else {
        u64::from(ctx.sd.counts.hosts)
    };
    let reserved = admin_reserve_first(ctx)?;
    let usable = usable(&seg.prefix, reserved, Some(seg.gateway));
    let mut problems = Vec::new();
    let mut warnings = Vec::new();

    if usable < hosts && mixed {
        warnings.push(format!(
            "{} has {usable} usable addresses for at most {hosts} hosts; counts.hosts includes \
             Zero-DPU hosts, which take no admin address",
            seg.prefix
        ));
    } else if usable < hosts {
        problems.push(format!(
            "{} has {usable} usable addresses for {hosts} hosts",
            seg.prefix
        ));
    } else if hosts > 0
        && u128::from(usable - hosts) * 100 < u128::from(hosts) * u128::from(HEADROOM_PERCENT)
    {
        warnings.push(format!(
            "{} leaves {} spare addresses for {hosts} hosts, under {HEADROOM_PERCENT}% headroom",
            seg.prefix,
            usable - hosts
        ));
    }
    if seg.mtu > n.path_mtu {
        problems.push(format!("mtu {} exceeds pathMtu {}", seg.mtu, n.path_mtu));
    }

    if !problems.is_empty() {
        return Ok(Outcome::Fail {
            observed: problems.join("; "),
            fix:
                "widen the admin prefix or lower counts.hosts; keep the segment MTU within pathMtu"
                    .to_string(),
        });
    }
    if !warnings.is_empty() {
        return Ok(Outcome::Warn {
            observed: warnings.join("; "),
            fix: if mixed {
                "confirm the DPU hosts fit the admin prefix, or widen it".to_string()
            } else {
                "widen the admin prefix before the fleet grows".to_string()
            },
        });
    }
    Ok(Outcome::Pass(if zero_dpu_only {
        format!(
            "{}: zero-DPU fleet, no admin addresses needed; mtu {} within pathMtu {}",
            seg.prefix, seg.mtu, n.path_mtu
        )
    } else {
        format!(
            "{}: {usable} usable for {hosts} hosts, mtu {} within pathMtu {}",
            seg.prefix, seg.mtu, n.path_mtu
        )
    }))
}

#[cfg(test)]
mod admin_segment_tests {
    use super::*;
    use crate::register::Register;

    const EXAMPLE: &str = include_str!("../../examples/site-def.yaml");

    /// The example with one line changed, run through the check.
    fn outcome(from: &str, to: &str) -> Outcome {
        assert!(
            EXAMPLE.contains(from),
            "splice target `{from}` not in the example"
        );
        let mut text = EXAMPLE.replacen(from, to, 1);
        if to.contains("Zero-DPU hosts") {
            // A /29 for 128 hosts: only the zero-DPU skip can make it pass.
            text = text.replacen(
                "admin:           { prefix: 10.10.60.0/24, gateway: 10.10.60.1",
                "admin:           { prefix: 10.10.60.0/29, gateway: 10.10.60.1",
                1,
            );
        }
        let sd = crate::sitedef::parse(&text).unwrap();
        let dec = crate::gate::effective_decisions(&sd, Register::builtin());
        let ctx = Ctx {
            sd: &sd,
            site_def_path: std::path::Path::new("examples/site-def.yaml"),
            decisions: &dec,
            evidence: None,
        };
        admin_segment(&ctx).unwrap()
    }

    /// A Core values file that carries no TOML names no reservation: the default holds.
    #[test]
    fn a_values_file_without_the_toml_uses_the_default_reservation() {
        let dir = tempfile::tempdir().unwrap();
        let values = dir.path().join("helm-prereqs/values");
        std::fs::create_dir_all(&values).unwrap();
        std::fs::write(values.join("nico-core.yaml"), "nico-api:\n  replicas: 1\n").unwrap();
        let site_def = dir.path().join("site-def.yaml");
        std::fs::write(&site_def, EXAMPLE).unwrap();
        let sd = crate::sitedef::load(&site_def).unwrap();
        let dec = crate::gate::effective_decisions(&sd, Register::builtin());
        let ctx = Ctx {
            sd: &sd,
            site_def_path: &site_def,
            decisions: &dec,
            evidence: None,
        };
        assert_eq!(admin_reserve_first(&ctx).unwrap(), ADMIN_RESERVED);
    }

    /// With two admin networks, the planned segment's table decides, not the first one
    /// (`aaa_other` sorts first, so a first-table lookup would read its 5).
    #[test]
    fn the_reservation_comes_from_the_admin_table_with_the_planned_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let values = dir.path().join("helm-prereqs/values");
        std::fs::create_dir_all(&values).unwrap();
        std::fs::write(
            values.join("nico-core.yaml"),
            "nico-api:\n  siteConfig:\n    nicoApiSiteConfig: |\n\
             \x20     [networks.aaa_other]\n      type = \"admin\"\n      prefix = \"10.10.70.0/24\"\n      reserve_first = 5\n\
             \x20     [networks.admin]\n      type = \"admin\"\n      prefix = \"10.10.60.0/24\"\n      reserve_first = 130\n",
        )
        .unwrap();
        let site_def = dir.path().join("site-def.yaml");
        std::fs::write(&site_def, EXAMPLE).unwrap();
        let sd = crate::sitedef::load(&site_def).unwrap();
        let dec = crate::gate::effective_decisions(&sd, Register::builtin());
        let ctx = Ctx {
            sd: &sd,
            site_def_path: &site_def,
            decisions: &dec,
            evidence: None,
        };
        assert_eq!(admin_reserve_first(&ctx).unwrap(), 130);
        match admin_segment(&ctx).unwrap() {
            Outcome::Fail { observed, .. } => {
                assert!(
                    observed.contains("125 usable addresses for 128 hosts"),
                    "{observed}"
                );
            }
            other => panic!("expected a failure, got {other:?}"),
        }
    }

    /// A `reserve_first` Core would reject is a tool error for the row, not the default.
    #[test]
    fn an_invalid_reservation_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let values = dir.path().join("helm-prereqs/values");
        std::fs::create_dir_all(&values).unwrap();
        std::fs::write(
            values.join("nico-core.yaml"),
            "nico-api:\n  siteConfig:\n    nicoApiSiteConfig: |\n      [networks.admin]\n      type = \"admin\"\n      reserve_first = \"200\"\n",
        )
        .unwrap();
        let site_def = dir.path().join("site-def.yaml");
        std::fs::write(&site_def, EXAMPLE).unwrap();
        let sd = crate::sitedef::load(&site_def).unwrap();
        let dec = crate::gate::effective_decisions(&sd, Register::builtin());
        let ctx = Ctx {
            sd: &sd,
            site_def_path: &site_def,
            decisions: &dec,
            evidence: None,
        };
        let err = admin_segment(&ctx).expect_err("an invalid reservation is an error");
        assert!(format!("{err:#}").contains("reserve_first"), "{err:#}");
    }

    #[test]
    fn admin_segment_outcomes() {
        struct Case {
            name: &'static str,
            from: &'static str,
            to: &'static str,
            want: fn(&Outcome) -> bool,
            observed_has: &'static str,
        }
        let cases = [
            Case {
                name: "the example fits: /24 for 128 hosts",
                from: "counts: { hosts: 128,",
                to: "counts: { hosts: 128,",
                want: |o| matches!(o, Outcome::Pass(_)),
                observed_has: "250 usable for 128 hosts",
            },
            Case {
                name: "too small fails naming both numbers",
                from: "counts: { hosts: 128,",
                to: "counts: { hosts: 300,",
                want: |o| matches!(o, Outcome::Fail { .. }),
                observed_has: "250 usable addresses for 300 hosts",
            },
            Case {
                name: "fits with under 20% headroom warns",
                from: "counts: { hosts: 128,",
                to: "counts: { hosts: 240,",
                want: |o| matches!(o, Outcome::Warn { .. }),
                observed_has: "10 spare addresses for 240 hosts",
            },
            Case {
                name: "a zero-DPU fleet needs no admin addresses",
                from: "managed-host-kinds-in-the-fleet: [DPU hosts booting through the DPU on the Admin segment]",
                to: "managed-host-kinds-in-the-fleet: [Zero-DPU hosts]",
                want: |o| matches!(o, Outcome::Pass(_)),
                observed_has: "zero-DPU fleet, no admin addresses needed",
            },
            Case {
                name: "a mixed fleet that does not fit the total only warns",
                from: "managed-host-kinds-in-the-fleet: [DPU hosts booting through the DPU on the Admin segment]",
                to: "managed-host-kinds-in-the-fleet: [DPU hosts booting through the DPU on the Admin segment, Zero-DPU hosts]",
                want: |o| matches!(o, Outcome::Warn { .. }),
                observed_has: "for at most 128 hosts",
            },
            Case {
                name: "segment MTU above the path MTU fails",
                from: "pathMtu: 9000",
                to: "pathMtu: 1500",
                want: |o| matches!(o, Outcome::Fail { .. }),
                observed_has: "mtu 9000 exceeds pathMtu 1500",
            },
        ];
        for c in cases {
            let o = outcome(c.from, c.to);
            assert!((c.want)(&o), "{}: got {o:?}", c.name);
            let observed = match &o {
                Outcome::Pass(s) | Outcome::Skip(s) => s.as_str(),
                Outcome::Warn { observed, .. } | Outcome::Fail { observed, .. } => {
                    observed.as_str()
                }
                Outcome::Todo => "",
            };
            assert!(
                observed.contains(c.observed_has),
                "{}: observed `{observed}`",
                c.name
            );
        }
    }
}
