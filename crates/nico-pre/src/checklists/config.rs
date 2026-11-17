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

use crate::collaterals;
use crate::harness::{Check, Ctx, Outcome};
use crate::setup_cmd::{Resolved, SetupCommand};

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
                 read from step 7, so before then its absence is a warning, not a failure. Both \
                 warnings share one row.\n\
                 Fail: `cpDpuSiteYaml` absent while the decision is a DPU, or any named path \
                 missing; observed names the key and the path (`coreValues: \
                 ./helm-prereqs/values/nico-core.yaml`); fix: correct the path or put the file \
                 there.\n\
                 Skip: never.\n\
                 Test: the example site-def against a directory of empty files; cases: all \
                 present passes; one missing fails naming key and path; `cpDpuSiteYaml` absent \
                 with a DPU fails; named with `no DPU` warns; only `expectedMachines` absent \
                 warns.",
        run: Some(collaterals_present),
    },
    Check {
        id: "config.segments.admin",
        expected: "the admin segment fits the fleet: usable addresses beyond the reserved block cover counts.hosts with headroom, and its MTU is within pathMtu",
        detail: "Reads: `network.segments.admin`, `network.pathMtu`, `counts.hosts`, the \
                 `managed-host-boot-paths` decision; `reserve_first` from the admin \
                 table in the NICo TOML inside `collaterals.coreValues` (the site TOML merged \
                 over `configFiles.nicoApiConfig` when the values carry one, as nico-api merges \
                 them) whose `prefix` or \
                 `prefix_v6` is the planned admin prefix, or the only admin table when none \
                 names it (5, the default, only when the file is absent or has no admin \
                 table; an admin table without `reserve_first`, a file that does not parse, \
                 several admin tables none with the planned prefix, or a `reserve_first` that \
                 is not a non-negative integer, is a tool error for this row).\n\
                 Pass: usable addresses, counted as Core's allocator does (the prefix less the \
                 first `reserve_first` addresses from the network address, assumed 5 when the \
                 TOML is absent, less the broadcast \
                 address, less the gateway when it lies outside the reserved block), at or above \
                 `counts.hosts` with 20% spare, and the segment MTU within `pathMtu`; observed \
                 `10.10.60.0/24: 250 usable for 128 hosts, mtu 9000 within pathMtu 9000`.\n\
                 Warn: under 20% spare; observed `leaves 9 spare addresses for 240 hosts`; fix: \
                 widen the prefix before the fleet grows. A mixed fleet (Zero-DPU or NIC-mode hosts among \
                 the kinds) that does not fit: `counts.hosts` is only an upper bound on the \
                 admin demand, so the shortfall is a warning naming both numbers; fix: confirm \
                 the DPU hosts fit or widen the prefix.\n\
                 Fail: fewer usable addresses than hosts, or MTU above `pathMtu`; observed names \
                 both numbers; fix: widen the prefix or lower `counts.hosts`, keep the MTU within \
                 `pathMtu`.\n\
                 Skip: the address demand (not the MTU) when every kind in the fleet boots on \
                 HostInband (Zero-DPU hosts, DPU hosts NICo flips to NIC mode) and takes no admin \
                 address. Alignment and the gateway \
                 are the gate's rows and are not repeated.\n\
                 Test: the example spliced one line at a time; cases: 128 hosts passes; 300 \
                 fails; 240 warns; `pathMtu: 1500` fails naming both MTUs; a zero-DPU fleet and a \
                 NIC-mode fleet on a /29 admin segment pass; a mixed fleet on a /29 warns; \
                 `reserve_first = 130` in the TOML fails with 125 usable.",
        run: Some(admin_segment),
    },
    Check {
        id: "config.consistency.prefixes",
        expected: "prefixes across all collaterals aligned and disjoint, gateways inside",
        detail: "Reads: every prefix in the collaterals: the CP-DPU site yaml (control plane, \
                 service VIP, DPU loopback, VPC VRF loopback), the TOML's `[networks.*]` and \
                 `[pools.*]` prefixes (`prefix_v6` too) with their gateways, the MetalLB pool CIDRs when `metallbConfig` \
                 is named; the Kubernetes \
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
        run: Some(prefixes),
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
                 Warn: a hostInband shortfall in a mixed fleet (`counts.hosts` is only an upper bound \
                 on the hosts that boot there; a fleet with no such kind puts no demand on the \
                 segment), or a pool with under 20% spare.\n\
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
        run: Some(pools),
    },
    Check {
        id: "config.consistency.site-controllers",
        expected: "at least three site controllers and Postgres sized to the host count",
        detail: "Reads: an omitted `postgresql.volumeSize` or memory limit is the chart's default \
                 (10Gi, 16Gi) and an omitted `max_database_connections` is nico-api's (1000), \
                 each sized like a stated value; a stated size that is neither a quantity string \
                 nor a byte count, or a connection limit that is not a non-negative integer, \
                 fails; `hosts.siteControllers`, `counts.hosts`; `postgresql.enabled`, \
                 `instances`, `volumeSize` and `resources.limits.memory` in \
                 `collaterals.helmPrereqsValues`; `max_database_connections` in the TOML inside \
                 `collaterals.coreValues`; the sizing table in `register/sizing.yaml` (compiled \
                 in; `--sizing-table <file>` replaces it for a run): today the chart's own \
                 Postgres memory guidance, 4Gi to 500 hosts, 8Gi to 1,500, 16Gi to 5,000, from \
                 a measured 4,500-host ingestion; a column the table leaves out (volume, \
                 connections) is read for its shape and not judged; the `core-postgres` \
                 decision.\n\
                 Pass: three or more site controllers; three Postgres instances; volume, memory \
                 and connections at or above the table row for `counts.hosts`, connections \
                 within the 1024 the chart's Postgres accepts; observed `3 controllers; postgres \
                 3 instances, 50Gi, 8Gi, 500 connections for 128 hosts; sizing table from \
                 <source>`.\n\
                 Warn: a Postgres value below the table's row, or a host count beyond its last \
                 row; fewer than three Postgres instances.\n\
                 Fail: fewer than three controllers; a Postgres value that is not a quantity or \
                 a count, or connections above 1024; observed names the value and the minimum; \
                 fix: add a controller or raise the value in the helm-prereqs values.\n\
                 Skip: the Postgres rows when `core-postgres` is `External Postgres`, pinned, \
                 implied, or read from `postgresql.enabled: false` when nothing pins it (observed \
                 says so); the controller count never.\n\
                 Test: the example's collaterals copied and spliced; cases: the example passes \
                 with the placeholder warning; two controllers fails; a volume below the row \
                 warns naming both sizes; external Postgres skips the Postgres rows only.",
        run: Some(site_controllers),
    },
    Check {
        id: "config.sitedef.agreement",
        expected: "every quantity present in both equal, key for key",
        detail: "Reads: the admin table is the `[networks.*]` entry with `type = \"admin\"` whose prefix \
                 is the planned one, or the only such entry, whatever its name; every quantity that appears both in site-def and in a collateral: in the \
                 CP-DPU site yaml `datacenterAsn`, `bgpAsnStart` against the MetalLB ASN \
                 (`asnLayout.siteControllerRoutes`, else `bgpAsnStart`), MTU, name server, \
                 prefixes, the `fnn` VNI, loopback prefix and the three common route targets, \
                 every `routeTargetsToImport` key as a target `fnn.routeTargets` names, the set \
                 of nodeIds, and each node's hostName and MAC by nodeId; the yaml's \
                 `siteControllerRoutesAsn` against the ASN every TOML routing profile imports \
                 the site-controller target with; in the TOML `sitename`, `datacenter_asn`, `[networks.admin]`, each \
                 underlay segment by name, the pools (`vlan-id`, `vni`, `vpc-vni`, \
                 `external-vpc-vni`, `fnn-asn`, `vpc-dpu-lo`, `lo-ip` against `pools.dpuLoopback`, every range of each, so a split \
                 pool is not read as one), `[fnn.admin_vpc] vpc_vni`, `ntp_servers`, \
                 `dhcp_servers`, and beside the TOML `nico-api.hostname` and the \
                 `nico-pxe.bootArtifactContainers` image tags against `versions.bootArtifacts`; in the \
                 helm-prereqs values `siteName`; through the collateral loaders.\n\
                 Pass: every pair equal; observed `N keys compared across 3 files, all equal`.\n\
                 Fail: any pair differs; observed one clause per mismatch with the key, both \
                 values and the file (`site.name site1 vs nico-core.yaml sitename site2`); fix: \
                 pick one as the source and copy it.\n\
                 Skip: never. A key absent from a file is not reported here; that is the file's \
                 `.rules` check.\n\
                 Test: the example's collaterals copied and spliced; cases: the agreeing set \
                 passes; `sitename` changed in the TOML fails naming key, both values and file; \
                 `datacenterAsn` changed in site-def fails against both files; a key removed from \
                 the TOML is not a mismatch.",
        run: Some(agreement),
    },
    Check {
        id: "config.cp-dpu-yaml.rules",
        expected: "the CP-DPU site yaml rows of the register",
        detail: "Reads: `collaterals.cpDpuSiteYaml`, parsed with the builder's known keys; the \
                 `smn` decision where pinned or implied; `dpu-in-the-site-controllers`.\n\
                 Pass: only known keys; the required fields present (`bgpAsnStart`, the three \
                 prefixes, `siteControllerMtuSize`, `nameServer`, `ubuntuPasswordHash`, \
                 `siteControllerNodes` with `hostName`, `mac`, `nodeId` 1..N); the loopback \
                 prefix larger than N and the control-plane prefix at least 2N; with `fnn`: \
                 `siteControllerRoutesAsn`, `datacenterAsn`, `controlPlaneVni` and the three \
                 common route targets present, `vpcVrfLoopbackPrefix` at least N, \
                 `routeTargetsToImport` a map keyed `<asn>:<n>` in canonical decimal with none \
                 equal to a fixed import; `installWithLeafPassword` true or false; `fnn` present \
                 exactly when `smn` is `SMN on the control-plane DPU`; observed `site.yaml: N \
                 nodes, builder rules hold`.\n\
                 Warn: a placeholder MAC (`aa:aa:aa:aa:aa:aa` or all zero); observed names the \
                 node.\n\
                 Fail: an unknown key, a required field missing, a builder rule violated; \
                 observed quotes the rule as the builder would print it; fix: edit the field \
                 named.\n\
                 Skip: when `dpu-in-the-site-controllers` is `no DPU`.\n\
                 Test: `examples/control-plane-dpu/site.yaml` copied and spliced; cases: the \
                 fixture passes; `fnn` removed with SMN pinned on the DPU fails; \
                 `routeTargetsToImport` as a sequence fails with the builder's message; a \
                 placeholder MAC warns; `no DPU` skips.",
        run: Some(cp_dpu_rules),
    },
    Check {
        id: "config.toml.rules",
        expected: "the NICo TOML inside the Core values parses and meets its rows",
        detail: "Reads: the TOML string under `nicoApiSiteConfig` in `collaterals.coreValues`, \
                 parsed as TOML (a narrow mirror of the tables below, not Core's own config \
                 types, which would pull the whole Core build into this binary); the decisions \
                 `managed-host-boot-paths`, `credential-store`, `evpn-overlay-peering`, \
                 `measured-boot-attestation`, `optional-components`; the site-def network \
                 values.\n\
                 Pass: the TOML parses; `sitename` present; an `admin` segment when a host kind \
                 boots through the DPU and a `hostinband` segment when one boots on HostInband, \
                 each `[networks.*]` MTU within `pathMtu`; `[site_explorer] dpu_policy` agrees \
                 with the host kinds (`manage`, also for hosts booting from an onboard NIC, whose \
                 DPU stays managed; `nic` for the flipped kind; `ignore` for a zero-DPU fleet); every route target's `asn` is `datacenter_asn` and the \
                 admin VPC exports `routeTargets.adminNetwork`; `deny_prefixes` covers the \
                 control-plane OOB and every BMC segment; `[secrets]` selects the credential \
                 store decided; `attestation_enabled` and `enable_route_servers` agree with their \
                 decisions; observed `nico-core.yaml: TOML parses, N tables, rules hold`.\n\
                 Warn: the deprecated `anycast_site_prefixes` key present; `[ib_config]`, \
                 `[nvlink_config]` or `[tracing]` absent while UFM, NVLink partitioning or \
                 Tracing is ticked; `dpu_policy` absent where the kinds need `nic` or `ignore`.\n\
                 Fail: `siteConfig.enabled: false`, which leaves the TOML undeployed; a parse error (quoted with its line), a missing segment for a host kind, a \
                 segment MTU above `pathMtu`, `dpu_policy` contradicting the kinds, a route \
                 target with another ASN, a BMC prefix outside `deny_prefixes`, a store, \
                 attestation or peering setting contradicting its decision; observed names the \
                 table and key; fix names the key to edit.\n\
                 Skip: never.\n\
                 Test: the example's Core values copied and spliced; cases: the fixture passes; a \
                 misspelled table fails with the parser's line; `dpu_policy` flipped fails naming \
                 both values; a segment MTU above `pathMtu` fails; the anycast key added warns; \
                 a BMC prefix dropped from `deny_prefixes` fails.",
        run: Some(toml_rules),
    },
    Check {
        id: "config.helm-prereqs.rules",
        expected: "the helm-prereqs values rows",
        detail: "Reads: `collaterals.helmPrereqsValues`, `restValues`, `siteAgentValues`, the \
                 service keys of `coreValues` (nico-dhcp, nico-dns, nico-ntp, nico-pxe, unbound) \
                 and the MetalLB BGPPeers when `metallbConfig` is named; the decisions \
                 `recursive-resolver-for-managed-machines`, `credential-store`, \
                 `managed-host-dpu-generation`, `rest-api-auth`, `core-postgres`, `root-ca`, \
                 `optional-components`.\n\
                 Pass: every BGPPeer hostname selector names a site controller; nico-dns and \
                 nico-ntp carry one per-pod VIP per replica; nico-pxe, and unbound with the \
                 built-in resolver, carry a VIP; Kea's `nameservers`, \
                 `provisioningServer` and `ntpServer` equal the unbound, nico-pxe and nico-ntp \
                 VIPs; with the built-in unbound, `forwarders.conf` lists \
                 `unboundForwardAddresses` and every `localData` name (the chart's list, or raw \
                 `local-data:` records in a `localConfig` conf) resolves to the right \
                 VIP; with external DNS (the decision, or `unbound.externalService.enabled: false` when \
                 none is pinned), Kea's `nameservers` is `network.nameServer` and the \
                 unbound service is off; `vault.kvSeeds` has the DPU factory root entry and the \
                 `bf4` entry when a BF4 generation is chosen; `rest.enabled` and `rms.enabled` \
                 follow the ticked components; the REST values' Keycloak block follows \
                 `rest-api-auth`; the site-agent's Core address (`CORE_GRPC_ADDRESS`, or the \
                 v2.0.x names `NICO_ADDRESS` / `CARBIDE_ADDRESS`) names nico-api and its \
                 `DB_ADDR` is `postgresql.host`; `certManager.siteRoot.create` follows `root-ca`; \
                 observed `N rules hold across 5 files`.\n\
                 Warn: a value left at the chart's placeholder where the site differs (an empty \
                 `forward-addr`, a `localData` entry without addresses).\n\
                 Fail: a rule violated; observed names the file, the key, the value and the \
                 expectation; fix names the key.\n\
                 Skip: the REST rows when `optional-components` lacks `REST group`; the Vault \
                 rows when `credential-store` is `Postgres envelope-encrypted` (pinned, implied, or \
                 read from `[secrets] writer` when nothing pins it); the Postgres \
                 host row with `External Postgres`.\n\
                 Test: the example's collaterals copied and spliced; cases: the fixture passes; \
                 Kea's `provisioningServer` moved off the PXE VIP fails naming both; a \
                 `localData` entry emptied warns; `BF4 generic` pinned without its seed entry \
                 fails; REST unticked skips the REST rows.",
        run: Some(helm_prereqs_rules),
    },
    Check {
        id: "config.metallb.rules",
        expected: "MetalLB pools hold every service VIP; peers match the site controllers",
        detail: "Reads: `collaterals.metallbConfig`, a file or a kustomize directory whose \
                 `resources` are plain files (IPAddressPools, BGPPeers, L2Advertisement); \
                 every `loadBalancerIPs` annotation (`metallb.io/` or `metallb.universe.tf/`) in \
                 `collaterals.coreValues` and `network.serviceVips`, as the VIPs; \
                 `hosts.siteControllers` with `torPeers` when present, the ASN layout, \
                 `forgeControlPlanePrefix`; the `dpu-in-the-site-controllers` decision.\n\
                 Pass: every VIP inside a pool, and over BGP every pool that holds a VIP covered \
                 by a BGPAdvertisement by name (coverage by an advertisement that selects pools by label, restricts \
                 announcing nodes or names its peers is \
                 not evaluated and is said so in observed); one BGPPeer per site controller with `myASN` the \
                 routes ASN, `peerASN` the controller's DPU ASN and `peerAddress` the DPU's end \
                 of its control-plane /31 (`forgeControlPlanePrefix` address 2(nodeId-1)), as \
                 the ISO builder lays them out; with `no DPU`, one peer per `torPeers` entry of \
                 each controller instead; a peer is found by its hostname selector, else by \
                 `peerAddress`; observed `10 VIPs in 2 pools; 3 peers match`.\n\
                 Warn: an L2Advertisement and no BGPPeer (development and test only) with every \
                 VIP inside a pool covered by an L2Advertisement by name; observed `L2 mode`.\n\
                 Fail: a VIP outside every pool, a pool holding a VIP that no advertisement \
                 covers (in L2 mode too) or that nothing announces (no BGPPeer and no \
                 L2Advertisement); a peer missing, or with a wrong \
                 ASN or address; a peer whose node selector is not `kubernetes.io/hostname In` \
                 (the check does not evaluate other selectors); observed names the VIP or the \
                 peer and both values; fix: edit the pool or the peer.\n\
                 Skip: no `collaterals.metallbConfig`: another tool renders MetalLB; observed \
                 says so.\n\
                 Test: the example's collaterals copied and spliced; cases: the fixture passes; \
                 the internal pool narrowed so a VIP falls outside fails naming it; one peer's \
                 `peerASN` off fails naming both; with `no DPU` and `torPeers` the peers are \
                 compared with the ToRs and the DPU peers are reported missing.",
        run: Some(metallb_rules),
    },
    Check {
        id: "config.setup.command",
        expected: "--skip-* flags agree with the decisions",
        detail: "Reads: `setup.command` and the `setup.env` variables that select components \
                 (`NICO_SKIP_DPF`, `NICO_INSTALL_DPF`, `NICO_SKIP_RMS`, `NICO_INSTALL_RMS`, \
                 `WITH_OBSERVABILITY`, `NICO_INSTALL_CONTOUR`), resolved as `setup.sh` does (a \
                 switch wins over its variable); the decisions `optional-components`, `dpf`, \
                 `rms`, `observability` where pinned or implied; `collaterals.coreValues` and \
                 `metallbConfig` as paths.\n\
                 Pass: each component's effective state agrees with the decisions (RMS and the \
                 REST group ticked exactly when installed, DPF installed exactly when `dpf` is \
                 `Install`, observability and Contour on exactly when ticked, `--site-overlay` \
                 exactly when ticked); `--core-values` and `--metallb-config`, when given, name \
                 the collateral by trailing path components, or by identity when an absolute \
                 operand and the collateral both resolve; observed the switches found and \
                 which decisions were not pinned.\n\
                 Warn: `--skip-dpf` while the TOML carries `[dpf.deployments.*]`, which setup.sh \
                 then leaves undeployed.\n\
                 Fail: a flag contradicting a decision, or missing for an unticked component; a \
                 values flag naming another file, or `--metallb-config` with no `metallbConfig` \
                 collateral; observed names the flag and the decision; fix: \
                 edit the command or the decision.\n\
                 Skip: a `setup.command` that does not run `setup.sh` (another tool deploys the \
                 site); observed says so. A decision that is neither pinned nor implied is not compared; \
                 `config.derived.decisions` reads it from the command instead.\n\
                 Test: `setup.command` spliced in the example; cases: the example passes; \
                 `--skip-rms` removed with RMS unticked fails; `--skip-dpf` added with `dpf: \
                 Install` pinned fails; `--core-values other.yaml` fails naming both paths; \
                 `NICO_SKIP_DPF: \"true\"` with `dpf: Install` pinned fails.",
        run: Some(setup_command),
    },
    Check {
        id: "config.setup.env",
        expected: "required NICO_* variables set for the ticked components; image tags equal versions",
        detail: "Reads: `setup.env` and the assignments around the invocation in `setup.command`; \
                 `versions`; the skip variables for \
                 what is installed; the \
                 decision `registry-authentication` where pinned; the `helm_repo_url` of every \
                 `[dpf.services.<service>]` in the TOML inside `collaterals.coreValues`.\n\
                 Pass: every variable `setup.sh` requires for what the command installs is \
                 present by name (`NICO_IMAGE_REGISTRY`, `NICO_CORE_IMAGE_TAG`, \
                 `NICO_REST_IMAGE_TAG`, `NICO_RMS_IMAGE_TAG`, `NICO_DPF_DPU_INTERFACE`, \
                 `NICO_DPF_DPU_CLUSTER_VIP`; `REGISTRY_PULL_SECRET` with credentialed pulls); a \
                 `{ from: ... }` source counts as present; `NICO_CORE_IMAGE_TAG` equals \
                 `versions.infraController`, `NICO_REST_IMAGE_TAG` equals `versions.nicoRest`, \
                 `NICO_RMS_IMAGE_TAG` equals `versions.nvRms` and `NICO_DPF_IMAGE_TAG` equals \
                 `versions.docaPlatform` where both are stated and the command installs the \
                 component; every DPF service `helm_repo_url` \
                 equals one of `NICO_DPF_HELM_REPO_CARBIDE`, `_OCI`, `_HTTPS` (the variable, or \
                 `setup.sh`'s default when unset); observed `12 variables; tags equal versions; 1 \
                 DPF repository matches`.\n\
                 Warn: a `setup.env` variable setup.sh does not read (the list of the ones it \
                 reads is kept in src/setup_cmd.rs, checked against the script by a test); \
                 observed names it.\n\
                 Fail: a required variable absent, a tag differing from `versions`, a DPF \
                 repository the variables do not name; observed names the variable and both \
                 values; fix: set it.\n\
                 Skip: the DPF rows when the command has `--skip-dpf`; the RMS row with \
                 `--skip-rms`; the REST row with `--skip-rest`; the whole row when the command does \
                 not run `setup.sh`.\n\
                 Test: `setup.env` spliced, with `examples/helm-prereqs/values/nico-core.yaml`; \
                 cases: the example passes; `NICO_CORE_IMAGE_TAG` changed fails naming both tags; \
                 `NICO_DPF_DPU_INTERFACE` removed fails; the same with `--skip-dpf` in the command \
                 passes; the DPF repository changed in the TOML fails naming it.",
        run: Some(setup_env),
    },
    Check {
        id: "config.derived.decisions",
        expected: "derived decisions read from the files agree with any pinned or implied value",
        detail: "Reads: every register entry with `stated: false`, read from its source (the \
                 setup command, `setup.env`, the TOML tables, the Helm values, the CP-DPU site \
                 yaml) by the readers in `src/derived.rs`; the values pinned under `decisions` \
                 or implied by the register's rules.\n\
                 Pass: every derived entry resolves and every pinned or implied value equals \
                 what the files say; observed `33 derived, 6 pinned or implied, all agree`.\n\
                 Warn: an entry the files leave ambiguous between options and nothing pins it \
                 (`credential-store` without a `[secrets]` table, `smn` without a site yaml, `dpf` and \
                 the other command-read entries when `setup.command` does not run `setup.sh`); \
                 observed names the entry and the candidates; fix: pin it under `decisions`.\n\
                 Fail: a pinned or implied value the files contradict (`dpf: pinned Skip, files \
                 say Install`), including a pin outside the candidates of an ambiguous entry \
                 (`BF3` pinned where the files allow only the BF4 variants) and a multi-select \
                 pin that lacks a member the files select (`optional-components` without `UFM` \
                 while `ib_config.enabled` is true); fix: drop the pin or change the file.\n\
                 Skip: never.\n\
                 Test: `decisions` spliced with the example's collaterals; cases: the example \
                 passes; `dpf: Skip` pinned against the example command fails naming both; \
                 `credential-store` pinned to the value the TOML holds passes; the `[secrets]` \
                 table removed leaves `credential-store` ambiguous and warns.",
        run: Some(derived_decisions),
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
        ("metallbConfig", c.metallb_config.as_deref()),
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
    // Both warnings in one row, like the failures above.
    let mut warnings: Vec<(&str, &str)> = Vec::new();
    if no_dpu && c.cp_dpu_site_yaml.is_some() {
        warnings.push((
            "cpDpuSiteYaml named although the site controllers carry no DPU",
            "drop it, or correct dpu-in-the-site-controllers",
        ));
    }
    let later = (!later.is_empty())
        .then(|| format!("not yet present, needed from step 7: {}", later.join(", ")));
    if let Some(later) = &later {
        warnings.push((
            later,
            "write the expected-machines file before the ingest checklist",
        ));
    }
    if !warnings.is_empty() {
        return Ok(Outcome::Warn {
            observed: warnings
                .iter()
                .map(|(o, _)| *o)
                .collect::<Vec<_>>()
                .join("; "),
            fix: warnings
                .iter()
                .map(|(_, f)| *f)
                .collect::<Vec<_>>()
                .join("; "),
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
        let kustomize = dir.path().join(c.metallb_config.as_deref().unwrap());
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
            c.metallb_config.as_deref().unwrap(),
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
        // With the controllers carrying no DPU as well, both warnings share the row.
        let mut sd = sd;
        sd.decisions.insert(
            "dpu-in-the-site-controllers".to_string(),
            crate::sitedef::DecisionValue::One("no DPU".to_string()),
        );
        let dec = crate::gate::effective_decisions(&sd, Register::builtin());
        let ctx = Ctx {
            sd: &sd,
            site_def_path: &site_def_path,
            decisions: &dec,
            evidence: None,
        };
        match collaterals_present(&ctx).unwrap() {
            Outcome::Warn { observed, fix } => {
                assert!(
                    observed.contains("cpDpuSiteYaml") && observed.contains("expectedMachines:"),
                    "{observed}"
                );
                assert!(
                    fix.contains("drop it") && fix.contains("expected-machines"),
                    "{fix}"
                );
            }
            other => panic!("expected one warning row, got {other:?}"),
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
    // nico-api merges the site TOML over the global one, key by key: the global file is
    // `configFiles.nicoApiConfig` when the values carry one, else the chart's built-in
    // file, which has no network tables. No TOML in the file names no `reserve_first`, so
    // Core's default applies; whether the TOML has to be there is the config checklist's
    // question.
    let toml_text = |section: &str, key: &str| {
        root.get(section)
            .and_then(|c| c.get(key))
            .and_then(serde_yaml::Value::as_str)
            .filter(|t| !t.trim().is_empty())
    };
    let global_text = toml_text("configFiles", "nicoApiConfig");
    let site_text = toml_text("siteConfig", "nicoApiSiteConfig");
    if global_text.is_none() && site_text.is_none() {
        return Ok(ADMIN_RESERVED);
    }
    let mut table = toml::Table::new();
    for (which, text) in [("global", global_text), ("site", site_text)] {
        if let Some(text) = text {
            let parsed: toml::Table = text.parse().wrap_err_with(|| {
                format!("{}: the {which} NICo TOML does not parse", path.display())
            })?;
            collaterals::merge_toml(&mut table, parsed);
        }
    }
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
    // Core requires `reserve_first` in an admin network table (its `NetworkDefinition` has
    // no default), so an absent key is an error, as is a value Core would reject. Only a
    // TOML with no admin table at all leaves the default standing.
    let reserved = match admin.and_then(|admin| admin.get("reserve_first")) {
        None if admin.is_some() => eyre::bail!(
            "{}: the admin table for {planned} has no reserve_first, which nico-api requires",
            path.display()
        ),
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
    // Zero-DPU hosts and DPU hosts NICo flips to NIC mode boot on HostInband and take no
    // admin address. The site definition counts hosts without their kind, so for a mixed
    // fleet `counts.hosts` is only an upper bound on the admin demand.
    const INBAND_ONLY: [&str; 2] = ["Zero-DPU hosts", "DPU hosts NICo flips to NIC mode"];
    let inband = |k: &String| INBAND_ONLY.contains(&k.as_str());
    let kinds = ctx.decision("managed-host-boot-paths");
    let inband_only = kinds.is_some_and(|kinds| !kinds.is_empty() && kinds.iter().all(inband));
    let mixed = !inband_only && kinds.is_some_and(|k| k.iter().any(inband));
    let hosts = if inband_only {
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
             hosts that boot on HostInband and take no admin address",
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
    Ok(Outcome::Pass(if inband_only {
        format!(
            "{}: the fleet boots on HostInband, no admin addresses needed; mtu {} within pathMtu {}",
            seg.prefix, seg.mtu, n.path_mtu
        )
    } else {
        format!(
            "{}: {usable} usable for {hosts} hosts (reserve_first {reserved}), mtu {} within pathMtu {}",
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
        if to.contains("Zero-DPU hosts") || to.contains("NIC mode") {
            // A /29 for 128 hosts: only the HostInband exemption can make it pass.
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

    /// nico-api reads the site TOML merged over the global one, so an admin table in
    /// `configFiles.nicoApiConfig` counts, and a site key wins over the global one.
    #[test]
    fn the_global_config_is_merged_under_the_site_config() {
        let run = |values_text: &str| {
            let dir = tempfile::tempdir().unwrap();
            let values = dir.path().join("helm-prereqs/values");
            std::fs::create_dir_all(&values).unwrap();
            std::fs::write(values.join("nico-core.yaml"), values_text).unwrap();
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
            admin_segment(&ctx).unwrap()
        };
        const GLOBAL: &str = "nico-api:\n  configFiles:\n    nicoApiConfig: |\n      [networks.admin]\n      type = \"admin\"\n      prefix = \"10.10.60.0/24\"\n      reserve_first = 130\n";
        match run(GLOBAL) {
            Outcome::Fail { observed, .. } => {
                assert!(observed.contains("125 usable"), "{observed}")
            }
            other => panic!("the global table's reservation counts: {other:?}"),
        }
        let site = format!(
            "{GLOBAL}  siteConfig:\n    nicoApiSiteConfig: |\n      [networks.admin]\n      reserve_first = 5\n"
        );
        match run(&site) {
            Outcome::Pass(observed) => assert!(observed.contains("250 usable"), "{observed}"),
            other => panic!("the site key wins over the global one: {other:?}"),
        }
    }

    /// Core requires the key, so an admin table without it is a tool error, not the default.
    #[test]
    fn an_admin_table_without_a_reservation_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let values = dir.path().join("helm-prereqs/values");
        std::fs::create_dir_all(&values).unwrap();
        std::fs::write(
            values.join("nico-core.yaml"),
            "nico-api:\n  siteConfig:\n    nicoApiSiteConfig: |\n      [networks.admin]\n      type = \"admin\"\n      prefix = \"10.10.60.0/24\"\n",
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
        let err = admin_segment(&ctx).expect_err("a missing reservation is an error");
        assert!(
            format!("{err:#}").contains("no reserve_first, which nico-api requires"),
            "{err:#}"
        );
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
                from: "managed-host-boot-paths: [DPU hosts booting through the DPU on the Admin segment]",
                to: "managed-host-boot-paths: [Zero-DPU hosts]",
                want: |o| matches!(o, Outcome::Pass(_)),
                observed_has: "boots on HostInband, no admin addresses needed",
            },
            Case {
                name: "a fleet NICo flips to NIC mode needs no admin addresses either",
                from: "managed-host-boot-paths: [DPU hosts booting through the DPU on the Admin segment]",
                to: "managed-host-boot-paths: [DPU hosts NICo flips to NIC mode]",
                want: |o| matches!(o, Outcome::Pass(_)),
                observed_has: "boots on HostInband, no admin addresses needed",
            },
            Case {
                name: "a mixed fleet that does not fit the total only warns",
                from: "managed-host-boot-paths: [DPU hosts booting through the DPU on the Admin segment]",
                to: "managed-host-boot-paths: [DPU hosts booting through the DPU on the Admin segment, Zero-DPU hosts]",
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
            };
            assert!(
                observed.contains(c.observed_has),
                "{}: observed `{observed}`",
                c.name
            );
        }
    }
}

/// VIPs every site's service pool has to hold, whatever the decisions.
const BASE_SERVICE_VIPS: u64 = 4; // api, dhcp, pxe, sshConsole
/// `nico-ntp` and `nico-dns` replicas when the Core values do not say; each gets a VIP.
const DEFAULT_NTP_REPLICAS: u64 = 3;
const DEFAULT_DNS_REPLICAS: u64 = 2;

/// One pool, what it holds and what the site asks of it.
struct Pool {
    /// How the observed clause names it.
    name: &'static str,
    /// The site-def field to widen when it is too small.
    field: &'static str,
    capacity: u64,
    demand: u64,
    /// What the demand counts (`dpus`, `hosts`, `VIPs`).
    unit: &'static str,
    /// The demand is `counts.hosts` while only some host kinds take an address here, so a
    /// shortfall is a warning, not a failure.
    upper_bound: bool,
}

/// Every pool the site definition sizes, against the counts it states. The
/// control-plane and loopback rows mirror `build-dpu-install-iso.sh`'s own
/// capacity checks so a site that passes here passes the builder; the ASN pool
/// is the range less what the convention (or `asnLayout`) reserves, one ASN per
/// managed DPU as `[pools.fnn-asn]` hands them out.
fn pools(ctx: &Ctx) -> eyre::Result<Outcome> {
    let sd = ctx.sd;
    let n = &sd.network;
    let controllers = sd.hosts.site_controllers.len() as u64;
    let dpus = u64::from(sd.counts.dpus);
    let hosts = u64::from(sd.counts.hosts);
    let no_dpu = ctx.decided("dpu-in-the-site-controllers", "no DPU");

    let mut vips = BASE_SERVICE_VIPS;
    // The decision when pinned or implied; else the setup command when it runs setup.sh
    // (REST and the observability stack install unless skipped), else setup.sh's default.
    let components = ctx.decision("optional-components");
    let cmd = SetupCommand::parse(&ctx.sd.setup);
    let ticked = |option: &str, by_command: bool| match components {
        Some(_) => ctx.decided("optional-components", option),
        None => !cmd.runs_setup_sh() || by_command,
    };
    if ticked("REST group", cmd.installs_rest()) {
        vips += 2; // REST and Keycloak
    }
    if ticked("Observability stack", cmd.with_observability()) {
        vips += 1;
    }
    if n.forge_service_vip_prefix.contains(&n.kubernetes.api_vip) {
        vips += 1;
    }
    // One VIP per nico-ntp and nico-dns replica; the Core values say how many when they load.
    let core = collaterals::core_values_tree(ctx).ok();
    // The built-in unbound takes a VIP: the decision when pinned or implied, else what the
    // Core values say, as `config.helm-prereqs.rules` reads them (the service is on only
    // when `unbound.externalService.enabled` is true; the chart's default is off). Values
    // that do not load keep the VIP counted: the larger demand is the safe one here.
    let builtin_unbound = match ctx.decision("recursive-resolver-for-managed-machines") {
        Some(_) => !ctx.decided("recursive-resolver-for-managed-machines", "External DNS"),
        None => core
            .as_ref()
            .is_none_or(|c| c.bool("unbound.externalService.enabled") == Some(true)),
    };
    if builtin_unbound {
        vips += 1;
    }
    let replicas = |svc: &str, default: u64| {
        core.as_ref()
            .and_then(|c| c.u64(&format!("{svc}.replicaCount")))
            .unwrap_or(default)
    };
    vips += replicas("nico-ntp", DEFAULT_NTP_REPLICAS) + replicas("nico-dns", DEFAULT_DNS_REPLICAS);

    let range = n.bgp_asn_start..=n.bgp_asn_end;
    let mut reserved: Vec<u32> = vec![n.site_controller_routes_asn()];
    reserved.extend(
        sd.hosts
            .site_controllers
            .iter()
            .map(|c| n.site_controller_asn(c.node_id)),
    );
    // Counted, not enumerated: the range may span the whole 32-bit space.
    let length = if n.bgp_asn_end >= n.bgp_asn_start {
        u64::from(n.bgp_asn_end - n.bgp_asn_start) + 1
    } else {
        0
    };
    let mut distinct = reserved.clone();
    distinct.sort_unstable();
    distinct.dedup();
    let in_range = distinct.iter().filter(|a| range.contains(a)).count() as u64;
    let asn_pool = length.saturating_sub(in_range);

    let mut pools = vec![
        Pool {
            name: "control plane",
            field: "forgeControlPlanePrefix",
            capacity: addresses(&n.forge_control_plane_prefix),
            demand: controllers * 2,
            unit: "addresses (a /31 per site controller)",
            upper_bound: false,
        },
        Pool {
            name: "service VIPs",
            field: "forgeServiceVipPrefix",
            // MetalLB hands out every address of a pool (the network and broadcast
            // addresses too, unless `avoidBuggyIPs` is set), unlike a Core segment.
            capacity: addresses(&n.forge_service_vip_prefix),
            demand: vips,
            unit: "VIPs",
            upper_bound: false,
        },
    ];
    if !no_dpu {
        pools.push(Pool {
            name: "control-plane DPU loopbacks",
            field: "forgeDpuLoopbackPrefix",
            capacity: addresses(&n.forge_dpu_loopback_prefix),
            demand: controllers + 1,
            unit: "addresses (one per control-plane DPU, plus the network)",
            upper_bound: false,
        });
    }
    // The managed DPUs take one loopback each from the TOML's `[pools.lo-ip]`, a prefix
    // or a list of ranges; the pool is sized when the Core values load and name it.
    if let Some(capacity) = collaterals::core_values(ctx)
        .ok()
        .and_then(|c| lo_ip_capacity(&c.toml))
    {
        pools.push(Pool {
            name: "managed DPU loopbacks",
            field: "[pools.lo-ip] in the TOML",
            capacity,
            demand: dpus,
            unit: "dpus",
            upper_bound: false,
        });
    }
    pools.push(Pool {
        name: "DPU ASNs",
        field: "bgpAsnEnd",
        capacity: asn_pool,
        demand: dpus,
        unit: "dpus",
        upper_bound: false,
    });
    pools.push(Pool {
        name: "BMC segments",
        field: "network.segments.managedHostBmc",
        capacity: n
            .segments
            .managed_host_bmc
            .iter()
            .map(|s| usable(&s.prefix, 1, Some(s.gateway)))
            .fold(0u64, u64::saturating_add),
        demand: hosts,
        unit: "hosts",
        upper_bound: false,
    });
    if !n.segments.host_inband.is_empty() {
        // Only the kinds in `INBAND_KINDS` boot there; the site definition counts hosts
        // without their kind, so in a mixed fleet `counts.hosts` is only an upper bound.
        let kinds = ctx.decision("managed-host-boot-paths");
        let inband = |k: &String| INBAND_KINDS.contains(&k.as_str());
        let any = kinds.is_some_and(|k| k.iter().any(inband));
        let all = kinds.is_some_and(|k| !k.is_empty() && k.iter().all(inband));
        pools.push(Pool {
            name: "hostInband segments",
            field: "network.segments.hostInband",
            capacity: n
                .segments
                .host_inband
                .iter()
                .map(|s| usable(&s.prefix, 1, Some(s.gateway)))
                .fold(0u64, u64::saturating_add),
            // A fleet with no such kind puts no demand on a spare segment; undecided
            // kinds keep the full count.
            demand: if kinds.is_some() && !any { 0 } else { hosts },
            unit: "hosts",
            upper_bound: any && !all,
        });
    }

    let mut clauses = Vec::new();
    let mut too_small = Vec::new();
    let mut tight = Vec::new();
    let mut bounded = Vec::new();
    for p in &pools {
        clauses.push(format!(
            "{}: {} for {} {}",
            p.name, p.capacity, p.demand, p.unit
        ));
        if p.capacity < p.demand && p.upper_bound {
            bounded.push(p.name);
        } else if p.capacity < p.demand {
            too_small.push(p.field);
        } else if p.demand > 0
            && u128::from(p.capacity - p.demand) * 100
                < u128::from(p.demand) * u128::from(HEADROOM_PERCENT)
        {
            tight.push(p.name);
        }
    }
    if range.contains(&n.datacenter_asn) && !reserved.contains(&n.datacenter_asn) {
        clauses.push(format!(
            "datacenterAsn {} inside the per-DPU pool",
            n.datacenter_asn
        ));
        too_small.push("bgpAsnStart/bgpAsnEnd or datacenterAsn");
    }

    let observed = clauses.join("; ");
    if !too_small.is_empty() {
        return Ok(Outcome::Fail {
            observed,
            fix: format!("widen {}", too_small.join(", ")),
        });
    }
    let mut fix = Vec::new();
    if !bounded.is_empty() {
        fix.push(format!(
            "{} hold fewer than counts.hosts, which includes hosts of kinds that boot \
             elsewhere; confirm the count of hosts that boot there",
            bounded.join(", ")
        ));
    }
    if !tight.is_empty() {
        fix.push(format!(
            "under {HEADROOM_PERCENT}% spare in {}; widen before the fleet grows",
            tight.join(", ")
        ));
    }
    if !fix.is_empty() {
        return Ok(Outcome::Warn {
            observed,
            fix: fix.join("; "),
        });
    }
    Ok(Outcome::Pass(observed))
}

#[cfg(test)]
mod pools_tests {
    use super::*;
    use crate::fixture::observed;
    use crate::register::Register;

    const EXAMPLE: &str = include_str!("../../examples/site-def.yaml");

    fn outcome(from: &str, to: &str) -> Outcome {
        outcome_all(&[(from, to)])
    }

    fn outcome_all(splices: &[(&str, &str)]) -> Outcome {
        let mut text = EXAMPLE.to_string();
        for (from, to) in splices {
            assert!(
                text.contains(from),
                "splice target `{from}` not in the example"
            );
            text = text.replacen(from, to, 1);
        }
        let sd = crate::sitedef::parse(&text).unwrap();
        let dec = crate::gate::effective_decisions(&sd, Register::builtin());
        let ctx = Ctx {
            sd: &sd,
            site_def_path: std::path::Path::new("examples/site-def.yaml"),
            decisions: &dec,
            evidence: None,
        };
        pools(&ctx).unwrap()
    }

    /// With `optional-components` unpinned, the REST VIPs follow the command: `--skip-rest`
    /// frees the two the pin would have demanded.
    #[test]
    fn unpinned_components_follow_the_command_for_the_vip_count() {
        let o = outcome_all(&[
            (
                "  optional-components: [REST group]",
                "  # optional-components unpinned",
            ),
            ("--skip-rms", "--skip-rms --skip-rest"),
        ]);
        assert!(
            observed(&o).contains("service VIPs: 32 for 11 VIPs"),
            "{o:?}"
        );
    }

    /// A fleet that boots on HostInband needs the segment for every host.
    #[test]
    fn a_host_inband_segment_too_small_for_an_inband_fleet_fails() {
        let o = outcome_all(&[
            (
                "prefix: 10.10.72.0/23, gateway: 10.10.72.1",
                "prefix: 10.10.72.0/26, gateway: 10.10.72.1",
            ),
            (
                "managed-host-boot-paths: [DPU hosts booting through the DPU on the Admin segment]",
                "managed-host-boot-paths: [Zero-DPU hosts]",
            ),
        ]);
        match &o {
            Outcome::Fail { observed, fix } => {
                assert!(
                    observed.contains("hostInband segments: 61 for 128 hosts"),
                    "{observed}"
                );
                assert!(fix.contains("network.segments.hostInband"), "{fix}");
            }
            other => panic!("expected a failure: {other:?}"),
        }
    }

    /// `counts.hosts` counts every kind; in a mixed fleet only some boot on HostInband, so a
    /// segment below the total is a warning that asks for the per-kind count, not a failure.
    #[test]
    fn a_host_inband_shortfall_in_a_mixed_fleet_only_warns() {
        let o = outcome_all(&[
            (
                "prefix: 10.10.72.0/23, gateway: 10.10.72.1",
                "prefix: 10.10.72.0/26, gateway: 10.10.72.1",
            ),
            (
                "managed-host-boot-paths: [DPU hosts booting through the DPU on the Admin segment]",
                "managed-host-boot-paths: [DPU hosts booting through the DPU on the Admin segment, Zero-DPU hosts]",
            ),
        ]);
        match &o {
            Outcome::Warn { observed, fix } => {
                assert!(
                    observed.contains("hostInband segments: 61 for 128 hosts"),
                    "{observed}"
                );
                assert!(
                    fix.contains("hostInband segments hold fewer than counts.hosts"),
                    "{fix}"
                );
            }
            other => panic!("expected a warning: {other:?}"),
        }
    }

    #[test]
    fn pool_outcomes() {
        struct Case {
            name: &'static str,
            from: &'static str,
            to: &'static str,
            want: fn(&Outcome) -> bool,
            observed_has: &'static str,
            fix_has: &'static str,
        }
        let cases = [
            Case {
                name: "the example holds its own counts",
                from: "counts: { hosts: 128, dpus: 128,",
                to: "counts: { hosts: 128, dpus: 128,",
                want: |o| matches!(o, Outcome::Pass(_)),
                observed_has: "DPU ASNs: 197 for 128 dpus",
                fix_has: "",
            },
            Case {
                name: "more DPUs than the ASN pool fails the ASN row",
                from: "counts: { hosts: 128, dpus: 128,",
                to: "counts: { hosts: 128, dpus: 200,",
                want: |o| matches!(o, Outcome::Fail { .. }),
                observed_has: "DPU ASNs: 197 for 200 dpus",
                fix_has: "widen bgpAsnEnd",
            },
            Case {
                name: "the datacenter ASN inside the per-DPU pool fails",
                from: "datacenterAsn: 65034",
                to: "datacenterAsn: 4244766900",
                want: |o| matches!(o, Outcome::Fail { .. }),
                observed_has: "datacenterAsn 4244766900 inside the per-DPU pool",
                fix_has: "datacenterAsn",
            },
            Case {
                name: "the service VIP pool counts every address, as MetalLB allocates",
                from: "counts: { hosts: 128, dpus: 128,",
                to: "counts: { hosts: 128, dpus: 128,",
                want: |o| matches!(o, Outcome::Pass(_)),
                observed_has: "service VIPs: 32 for 13 VIPs",
                fix_has: "",
            },
            Case {
                name: "a hostInband segment is spare when no kind in the fleet boots there",
                from: "prefix: 10.10.72.0/23, gateway: 10.10.72.1",
                to: "prefix: 10.10.72.0/26, gateway: 10.10.72.1",
                want: |o| matches!(o, Outcome::Pass(_)),
                observed_has: "hostInband segments: 61 for 0 hosts",
                fix_has: "",
            },
            Case {
                name: "no managed DPUs passes with zero demand",
                from: "counts: { hosts: 128, dpus: 128,",
                to: "counts: { hosts: 128, dpus: 0,",
                want: |o| matches!(o, Outcome::Pass(_)),
                observed_has: "DPU ASNs: 197 for 0 dpus",
                fix_has: "",
            },
            Case {
                name: "a control-plane prefix without a /31 per controller fails",
                from: "forgeControlPlanePrefix: 10.10.248.88/29",
                to: "forgeControlPlanePrefix: 10.10.248.88/30",
                want: |o| matches!(o, Outcome::Fail { .. }),
                observed_has: "control plane: 4 for 6 addresses",
                fix_has: "widen forgeControlPlanePrefix",
            },
            Case {
                name: "a fleet near the BMC capacity warns",
                from: "counts: { hosts: 128, dpus: 128,",
                to: "counts: { hosts: 240, dpus: 128,",
                want: |o| matches!(o, Outcome::Warn { .. }),
                observed_has: "BMC segments: 250 for 240 hosts",
                fix_has: "BMC segments",
            },
        ];
        for c in cases {
            let o = outcome(c.from, c.to);
            assert!((c.want)(&o), "{}: got {o:?}", c.name);
            let obs = observed(&o);
            assert!(obs.contains(c.observed_has), "{}: observed `{obs}`", c.name);
            if let Outcome::Fail { fix, .. } | Outcome::Warn { fix, .. } = &o {
                assert!(fix.contains(c.fix_has), "{}: fix `{fix}`", c.name);
            }
        }
    }
}

/// The `--skip-*`, `--install-*` and `--with-*` switches against the decisions that
/// are pinned or implied. A decision that is neither is left to
/// `config.derived.decisions`, which reads it from this very command.
fn setup_command(ctx: &Ctx) -> eyre::Result<Outcome> {
    let cmd = SetupCommand::parse(&ctx.sd.setup);
    if !cmd.runs_setup_sh() {
        return Ok(Outcome::Skip(NOT_SETUP_SH.to_string()));
    }
    let mut problems: Vec<String> = Vec::new();
    let mut unpinned: Vec<&str> = Vec::new();

    // Each component's effective state (switch or variable, as setup.sh resolves them)
    // against the decision; the message names what setup.sh will do and what was decided.
    let ticked = |id: &str, option: &str| ctx.decided(id, option);
    let compare =
        |problems: &mut Vec<String>, what: &str, effective: bool, expected: bool, why: &str| {
            if effective != expected {
                problems.push(format!(
                    "{what} {} by setup.command/env, {why} says {}",
                    if effective { "on" } else { "off" },
                    if expected { "on" } else { "off" }
                ));
            }
        };

    match ctx.decision("optional-components") {
        Some(_) => {
            compare(
                &mut problems,
                "RMS",
                cmd.installs_rms(),
                ticked("optional-components", "RMS"),
                "optional-components",
            );
            compare(
                &mut problems,
                "REST",
                cmd.installs_rest(),
                ticked("optional-components", "REST group"),
                "optional-components",
            );
            compare(
                &mut problems,
                "observability",
                cmd.with_observability(),
                ticked("optional-components", "Observability stack"),
                "optional-components",
            );
            compare(
                &mut problems,
                "Contour",
                cmd.installs_contour(),
                ticked("optional-components", "Contour ingress"),
                "optional-components",
            );
            if ticked("optional-components", "Site overlay") != cmd.site_overlay.is_some() {
                problems.push("--site-overlay disagrees with optional-components".to_string());
            }
        }
        None => unpinned.push("optional-components"),
    }
    match ctx.decision("dpf") {
        Some(_) => compare(
            &mut problems,
            "DPF",
            cmd.installs_dpf(),
            ticked("dpf", "Install"),
            "dpf",
        ),
        None => unpinned.push("dpf"),
    }
    // The command decides `dpf` when nothing pins it, so the comparison above cannot see a
    // TOML that writes DPF deployments the skipped install never deploys.
    let mut warnings: Vec<String> = Vec::new();
    if !cmd.installs_dpf()
        && let Ok(core) = collaterals::core_values(ctx)
        && core
            .toml
            .get("dpf")
            .and_then(|d| d.get("deployments"))
            .and_then(toml::Value::as_table)
            .is_some_and(|t| !t.is_empty())
    {
        warnings.push(
            "DPF skipped by setup.command/env while the TOML carries [dpf.deployments.*], which \
             stay undeployed"
                .to_string(),
        );
    }
    match ctx.decision("rms") {
        Some(_) => compare(
            &mut problems,
            "RMS",
            cmd.installs_rms(),
            ticked("rms", "In-cluster nv-rms"),
            "rms",
        ),
        None => unpinned.push("rms"),
    }
    match ctx.decision("observability") {
        Some(_) => compare(
            &mut problems,
            "observability",
            cmd.with_observability(),
            ticked("observability", "Full local stack"),
            "observability",
        ),
        None => unpinned.push("observability"),
    }

    let c = &ctx.sd.collaterals;
    for (flag, given, collateral) in [
        (
            "--core-values",
            &cmd.core_values,
            Some(c.core_values.as_path()),
        ),
        (
            "--metallb-config",
            &cmd.metallb_config,
            c.metallb_config.as_deref(),
        ),
    ] {
        // Compared by trailing components with `./` dropped from both, so
        // `--core-values ./values/nico-core.yaml` matches `./helm-prereqs/values/nico-core.yaml`.
        // Only the named components count (`./`, `../` and a leading `/` are where the
        // command runs from, not which file it names); either may be the longer path.
        fn names(p: &std::path::Path) -> Vec<&std::ffi::OsStr> {
            let mut out = Vec::new();
            for c in p.components() {
                match c {
                    std::path::Component::Normal(n) => out.push(n),
                    // `values/../nico-core.yaml` names the file beside `values`, not in it.
                    std::path::Component::ParentDir => {
                        out.pop();
                    }
                    _ => {}
                }
            }
            out
        }
        let names_it = |given: &str, collateral: &std::path::Path| {
            // An absolute operand names one file: when it and the collateral both resolve,
            // identity decides, so two files of one name in different places do not match.
            let given_path = std::path::Path::new(given);
            if given_path.is_absolute()
                && let (Ok(a), Ok(b)) = (
                    given_path.canonicalize(),
                    ctx.resolve(collateral).canonicalize(),
                )
            {
                return a == b;
            }
            let g = names(given_path);
            let c = names(collateral);
            let suffix = |long: &[&std::ffi::OsStr], short: &[&std::ffi::OsStr]| {
                !short.is_empty()
                    && long.len() >= short.len()
                    && long[long.len() - short.len()..] == short[..]
            };
            suffix(&c, &g) || suffix(&g, &c)
        };
        match (given, collateral) {
            (Some(given), _) if given.contains(['$', '`']) => problems.push(format!(
                "{flag} {given} cannot be read here; name the file, or set the variable it \
                 refers to as a plain value"
            )),
            (Some(given), None) => problems.push(format!(
                "{flag} {given} given, collaterals names no such file"
            )),
            (Some(given), Some(collateral)) if !names_it(given, collateral) => {
                problems.push(format!(
                    "{flag} {given} is not the collateral {}",
                    collateral.display()
                ));
            }
            _ => {}
        }
    }

    let mut observed = if cmd.switches.is_empty() {
        "no switches".to_string()
    } else {
        format!("switches {}", cmd.switches.join(" "))
    };
    if !unpinned.is_empty() {
        observed.push_str(&format!("; not pinned: {}", unpinned.join(", ")));
    }
    if problems.is_empty() && warnings.is_empty() {
        return Ok(Outcome::Pass(observed));
    }
    if problems.is_empty() {
        return Ok(Outcome::Warn {
            observed: format!("{observed}; {}", warnings.join("; ")),
            fix: "install DPF, or drop the deployments from the TOML".to_string(),
        });
    }
    problems.dedup();
    Ok(Outcome::Fail {
        observed: format!("{observed}; {}", problems.join("; ")),
        fix: "edit setup.command, or the decision it disagrees with".to_string(),
    })
}

/// Why the two setup rows skip when `setup.command` runs something else.
const NOT_SETUP_SH: &str = "the command does not run setup.sh; another tool deploys the site";

/// `setup.sh`'s defaults for the Argo CD helm repository URLs, when the variables are unset.
const DPF_HELM_REPO_DEFAULTS: [(&str, &str); 3] = [
    (
        "NICO_DPF_HELM_REPO_CARBIDE",
        "https://helm.ngc.nvidia.com/0837451325059433/carbide-dev",
    ),
    ("NICO_DPF_HELM_REPO_OCI", "nvcr.io/nvidia/doca"),
    (
        "NICO_DPF_HELM_REPO_HTTPS",
        "https://helm.ngc.nvidia.com/nvidia/doca",
    ),
];

/// The variables `setup.sh` needs for what the command installs, the image tags against
/// `versions`, and the DPF helm repositories against the TOML.
fn setup_env(ctx: &Ctx) -> eyre::Result<Outcome> {
    let setup = &ctx.sd.setup;
    let cmd = SetupCommand::parse(setup);
    if !cmd.runs_setup_sh() {
        return Ok(Outcome::Skip(NOT_SETUP_SH.to_string()));
    }
    let v = &ctx.sd.versions;
    // `setup.sh` treats an empty value as unset (`${VAR:-default}`), so an empty value is
    // neither present nor a value to compare.
    let plain = |name: &str| match cmd.variable(name, setup) {
        Resolved::Plain(s) if !s.is_empty() => Some(s),
        _ => None,
    };
    let present = |name: &str| {
        !matches!(
            cmd.variable(name, setup),
            Resolved::Absent | Resolved::Plain("")
        )
    };

    let mut required: Vec<&str> = Vec::new();
    if cmd.installs_core() || cmd.installs_rest() {
        required.push("NICO_IMAGE_REGISTRY");
    }
    if cmd.installs_core() {
        required.push("NICO_CORE_IMAGE_TAG");
    }
    if cmd.installs_rest() {
        required.push("NICO_REST_IMAGE_TAG");
    }
    if cmd.installs_rms() {
        required.push("NICO_RMS_IMAGE_TAG");
    }
    if cmd.installs_dpf() {
        required.extend(["NICO_DPF_DPU_INTERFACE", "NICO_DPF_DPU_CLUSTER_VIP"]);
    }
    if ctx.decided("registry-authentication", "Credentialed pulls") {
        required.push("REGISTRY_PULL_SECRET");
    }
    let mut problems: Vec<String> = required
        .iter()
        .filter(|name| !present(name))
        .map(|name| format!("{name} not set"))
        .collect();

    // Tags are compared only for what the command installs: a stale value for a skipped
    // component is not a problem of this run.
    let tags: [(&str, Option<&str>, bool); 4] = [
        (
            "NICO_CORE_IMAGE_TAG",
            Some(v.infra_controller.as_str()),
            cmd.installs_core(),
        ),
        (
            "NICO_REST_IMAGE_TAG",
            Some(v.nico_rest.as_str()),
            cmd.installs_rest(),
        ),
        (
            "NICO_RMS_IMAGE_TAG",
            v.nv_rms.as_deref(),
            cmd.installs_rms(),
        ),
        (
            "NICO_DPF_IMAGE_TAG",
            v.doca_platform.as_deref(),
            cmd.installs_dpf(),
        ),
    ];
    let mut compared = 0;
    for (name, want, installs) in tags {
        if !installs {
            continue;
        }
        if let (Some(got), Some(want)) = (plain(name), want) {
            compared += 1;
            if got != want {
                problems.push(format!("{name} {got}, versions says {want}"));
            }
        }
    }

    let mut repos_matched = 0;
    if cmd.installs_dpf() {
        let core = collaterals::core_values(ctx)?;
        // A variable named by source has a value this run cannot see: no URL can be held
        // against it, so the comparison is skipped for that run.
        let secret_repo = DPF_HELM_REPO_DEFAULTS
            .iter()
            .any(|(var, _)| matches!(cmd.variable(var, setup), Resolved::Secret));
        let named: Vec<String> = DPF_HELM_REPO_DEFAULTS
            .iter()
            .map(|(var, default)| plain(var).unwrap_or(default).to_string())
            .collect();
        if let Some(services) = core
            .toml
            .get("dpf")
            .and_then(|d| d.get("services"))
            .and_then(toml::Value::as_table)
        {
            for (service, _) in services {
                if let Some(url) = collaterals::toml_str(
                    &core.toml,
                    &format!("dpf.services.{service}.helm_repo_url"),
                ) {
                    if secret_repo || named.iter().any(|n| n == url) {
                        repos_matched += 1;
                    } else {
                        problems.push(format!(
                            "[dpf.services.{service}] helm_repo_url {url} in {} is none of NICO_DPF_HELM_REPO_CARBIDE/_OCI/_HTTPS",
                            core.path.display()
                        ));
                    }
                }
            }
        }
    }

    // A variable setup.sh does not read is ignored at run time, so a misspelling of a
    // required one (`NICO_SKIP_DFP`) would pass every check above; it warns here.
    let unknown: Vec<&str> = setup
        .env
        .keys()
        .map(String::as_str)
        .filter(|k| !crate::setup_cmd::SETUP_SH_VARIABLES.contains(k))
        .collect();
    let observed = format!(
        "{} variables; {compared} tags compared with versions; {repos_matched} DPF repositories match",
        setup.env.len()
    );
    if !problems.is_empty() {
        return Ok(Outcome::Fail {
            observed: format!("{observed}; {}", problems.join("; ")),
            fix: "set the variable in setup.env, or align versions and the TOML with it"
                .to_string(),
        });
    }
    if !unknown.is_empty() {
        return Ok(Outcome::Warn {
            observed: format!(
                "{observed}; not read by setup.sh: {} (misspelt, or meant for another tool)",
                unknown.join(", ")
            ),
            fix: "check the spelling against setup.sh's variables, or drop the entry".to_string(),
        });
    }
    Ok(Outcome::Pass(observed))
}

#[cfg(test)]
mod setup_tests {
    use super::*;
    use crate::fixture::observed;
    use crate::register::Register;

    const EXAMPLE: &str = include_str!("../../examples/site-def.yaml");
    const EXAMPLE_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/site-def.yaml");

    /// The example with each `(from, to)` spliced once, through `check`.
    fn outcome(splices: &[(&str, &str)], check: fn(&Ctx) -> eyre::Result<Outcome>) -> Outcome {
        let mut text = EXAMPLE.to_string();
        for (from, to) in splices {
            assert!(
                text.contains(from),
                "splice target `{from}` not in the example"
            );
            text = text.replacen(from, to, 1);
        }
        let sd = crate::sitedef::parse(&text).unwrap();
        let dec = crate::gate::effective_decisions(&sd, Register::builtin());
        let ctx = Ctx {
            sd: &sd,
            site_def_path: std::path::Path::new(EXAMPLE_PATH),
            decisions: &dec,
            evidence: None,
        };
        check(&ctx).unwrap()
    }

    const COMMAND: &str = "command: ./setup.sh -y --core-values values/nico-core.yaml --metallb-config values/metallb-config.yaml --skip-rms";

    #[test]
    fn setup_command_outcomes() {
        struct Case {
            name: &'static str,
            splices: &'static [(&'static str, &'static str)],
            pass: bool,
            observed_has: &'static str,
        }
        let cases = [
            Case {
                name: "the example's command agrees with its pinned decisions",
                splices: &[],
                pass: true,
                observed_has: "switches -y --skip-rms",
            },
            Case {
                name: "RMS unticked without --skip-rms fails",
                splices: &[(
                    COMMAND,
                    "command: ./setup.sh -y --core-values values/nico-core.yaml",
                )],
                pass: false,
                observed_has: "RMS on by setup.command/env, optional-components says off",
            },
            Case {
                name: "--skip-dpf against a pinned Install fails",
                splices: &[
                    (COMMAND, "command: ./setup.sh -y --skip-rms --skip-dpf"),
                    ("  # dpf: Install ", "  dpf: Install "),
                ],
                pass: false,
                observed_has: "DPF off by setup.command/env, dpf says on",
            },
            Case {
                name: "NICO_SKIP_DPF in the environment counts as skipping DPF",
                splices: &[
                    (
                        "    NICO_DPF_DPU_INTERFACE: ens1f0np0\n",
                        "    NICO_DPF_DPU_INTERFACE: ens1f0np0\n    NICO_SKIP_DPF: \"true\"\n",
                    ),
                    ("  # dpf: Install ", "  dpf: Install "),
                ],
                pass: false,
                observed_has: "DPF off by setup.command/env, dpf says on",
            },
            Case {
                name: "a values path given through a variable is the file the shell hands over",
                splices: &[
                    (
                        COMMAND,
                        "command: ./setup.sh -y --core-values \"$CORE_VALUES\" --skip-rms",
                    ),
                    (
                        "    REGISTRY_PULL_SECRET: { from: env }",
                        "    REGISTRY_PULL_SECRET: { from: env }\n    CORE_VALUES: values/nico-core.yaml",
                    ),
                ],
                pass: true,
                observed_has: "switches -y --skip-rms",
            },
            Case {
                name: "a values path the gate cannot read is said so, not called another file",
                splices: &[(
                    COMMAND,
                    "command: ./setup.sh -y --core-values $(pwd)/nico-core.yaml --skip-rms",
                )],
                pass: false,
                observed_has: "--core-values $(pwd)/nico-core.yaml cannot be read here",
            },
            Case {
                name: "a parent component is resolved before the paths are compared",
                splices: &[(
                    COMMAND,
                    "command: ./setup.sh -y --core-values helm-prereqs/values/../nico-core.yaml --skip-rms",
                )],
                pass: false,
                observed_has: "--core-values helm-prereqs/values/../nico-core.yaml is not the collateral",
            },
            Case {
                name: "a command that does not run setup.sh is skipped, not compared",
                splices: &[(COMMAND, "command: python3 tools/deploy.py --site dc1")],
                pass: false,
                observed_has: "the command does not run setup.sh",
            },
            Case {
                name: "a values flag naming another file fails with both paths",
                splices: &[(
                    COMMAND,
                    "command: ./setup.sh -y --core-values other.yaml --skip-rms",
                )],
                pass: false,
                observed_has: "--core-values other.yaml is not the collateral ./helm-prereqs/values/nico-core.yaml",
            },
        ];
        for c in cases {
            let o = outcome(c.splices, setup_command);
            assert_eq!(
                matches!(o, Outcome::Pass(_)),
                c.pass,
                "{}: got {o:?}",
                c.name
            );
            let obs = observed(&o);
            assert!(obs.contains(c.observed_has), "{}: observed `{obs}`", c.name);
        }
    }

    #[test]
    fn setup_env_outcomes() {
        struct Case {
            name: &'static str,
            splices: &'static [(&'static str, &'static str)],
            pass: bool,
            observed_has: &'static str,
        }
        let cases = [
            Case {
                name: "the example's variables cover what its command installs",
                splices: &[],
                pass: true,
                observed_has: "variables; 2 tags compared with versions; 1 DPF repositories match",
            },
            Case {
                name: "a command that does not run setup.sh is skipped",
                splices: &[(COMMAND, "command: python3 tools/deploy.py --site dc1")],
                pass: false,
                observed_has: "the command does not run setup.sh",
            },
            Case {
                name: "a variable setup.sh does not read warns naming it",
                splices: &[(
                    "    KUBECONFIG: ~/.kube/site1\n",
                    "    KUBECONFIG: ~/.kube/site1\n    NICO_SKIP_DFP: \"true\"\n",
                )],
                pass: false,
                observed_has: "not read by setup.sh: NICO_SKIP_DFP",
            },
            Case {
                name: "a variable assigned on the command line counts as set",
                splices: &[
                    ("    NICO_DPF_DPU_INTERFACE: ens1f0np0\n", ""),
                    (
                        COMMAND,
                        "command: NICO_DPF_DPU_INTERFACE=ens1f0np0 ./setup.sh -y --core-values values/nico-core.yaml --metallb-config values/metallb-config.yaml --skip-rms",
                    ),
                ],
                pass: true,
                observed_has: "variables;",
            },
            Case {
                name: "a Core tag differing from versions fails naming both",
                splices: &[(
                    "NICO_CORE_IMAGE_TAG: v2026.10.15",
                    "NICO_CORE_IMAGE_TAG: v2026.09.01",
                )],
                pass: false,
                observed_has: "NICO_CORE_IMAGE_TAG v2026.09.01, versions says v2026.10.15",
            },
            Case {
                name: "the DPU interface missing while DPF installs fails",
                splices: &[("    NICO_DPF_DPU_INTERFACE: ens1f0np0\n", "")],
                pass: false,
                observed_has: "NICO_DPF_DPU_INTERFACE not set",
            },
            Case {
                name: "a tag passed through from the environment on the command line is the tag",
                splices: &[(
                    COMMAND,
                    "command: NICO_CORE_IMAGE_TAG=\"${NICO_CORE_IMAGE_TAG}\" ./setup.sh -y --core-values values/nico-core.yaml --metallb-config values/metallb-config.yaml --skip-rms",
                )],
                pass: true,
                observed_has: "2 tags compared with versions",
            },
            Case {
                name: "an empty repository variable takes setup.sh's default",
                splices: &[(
                    "    REGISTRY_PULL_SECRET: { from: env }",
                    "    REGISTRY_PULL_SECRET: { from: env }\n    NICO_DPF_HELM_REPO_CARBIDE: \"\"",
                )],
                pass: true,
                observed_has: "1 DPF repositories match",
            },
            Case {
                name: "an empty value is unset, as setup.sh reads it",
                splices: &[(
                    "NICO_DPF_DPU_INTERFACE: ens1f0np0",
                    "NICO_DPF_DPU_INTERFACE: \"\"",
                )],
                pass: false,
                observed_has: "NICO_DPF_DPU_INTERFACE not set",
            },
            Case {
                name: "the same variable missing with --skip-dpf passes",
                splices: &[
                    ("    NICO_DPF_DPU_INTERFACE: ens1f0np0\n", ""),
                    (COMMAND, "command: ./setup.sh -y --skip-rms --skip-dpf"),
                ],
                pass: true,
                observed_has: "0 DPF repositories match",
            },
            Case {
                name: "a DPF repository the variables do not name fails",
                splices: &[(
                    "NICO_DPF_HELM_REPO_CARBIDE: oci://registry.site1.dc1.example.net/nico-charts",
                    "NICO_DPF_HELM_REPO_CARBIDE: oci://registry.site1.dc1.example.net/other-charts",
                )],
                pass: false,
                observed_has: "[dpf.services.dpu_agent] helm_repo_url oci://registry.site1.dc1.example.net/nico-charts",
            },
        ];
        for c in cases {
            let o = outcome(c.splices, setup_env);
            assert_eq!(
                matches!(o, Outcome::Pass(_)),
                c.pass,
                "{}: got {o:?}",
                c.name
            );
            let obs = observed(&o);
            assert!(obs.contains(c.observed_has), "{}: observed `{obs}`", c.name);
        }
    }
    /// An absolute operand names one file: the collateral itself passes; a copy of it
    /// elsewhere under the same name does not.
    #[test]
    fn an_absolute_values_path_is_compared_by_identity() {
        let ex = crate::fixture::Example::new();
        let real = ex.path("helm-prereqs/values/nico-core.yaml");
        ex.splice(
            "site-def.yaml",
            COMMAND,
            &format!(
                "command: ./setup.sh -y --core-values {} --metallb-config values/metallb-config.yaml --skip-rms",
                real.display()
            ),
        );
        let o = ex.run(setup_command);
        assert!(matches!(o, Outcome::Pass(_)), "{o:?}");
        let elsewhere = ex.path("elsewhere");
        std::fs::create_dir_all(&elsewhere).expect("the copy is writable");
        std::fs::copy(&real, elsewhere.join("nico-core.yaml")).expect("the copy is writable");
        ex.splice(
            "site-def.yaml",
            &real.display().to_string(),
            &elsewhere.join("nico-core.yaml").display().to_string(),
        );
        let o = ex.run(setup_command);
        assert!(
            matches!(o, Outcome::Fail { .. })
                && crate::fixture::observed(&o).contains("is not the collateral"),
            "{o:?}"
        );
    }

    /// DPF skipped while the TOML writes deployments warns: setup.sh leaves them undeployed.
    #[test]
    fn dpf_skipped_against_written_deployments_warns() {
        let ex = crate::fixture::Example::new();
        ex.splice("site-def.yaml", COMMAND, &format!("{COMMAND} --skip-dpf"));
        ex.splice(
            "helm-prereqs/values/nico-core.yaml",
            "      [secrets]\n",
            "      [dpf.deployments.bf4_generic]\n      bluefield_software = \"x\"\n\n      [secrets]\n",
        );
        let o = ex.run(setup_command);
        assert!(
            matches!(o, Outcome::Warn { .. })
                && crate::fixture::observed(&o)
                    .contains("[dpf.deployments.*], which stay undeployed"),
            "{o:?}"
        );
    }
}

/// The `n`th address of a prefix, counting the network address as 0.
fn nth_address(p: &IpNet, n: u32) -> IpAddr {
    match p {
        IpNet::V4(p) => IpAddr::V4((u32::from(p.network()).wrapping_add(n)).into()),
        IpNet::V6(p) => IpAddr::V6((u128::from(p.network()).wrapping_add(u128::from(n))).into()),
    }
}

/// A BGPPeer the site definition says MetalLB must have.
struct ExpectedPeer {
    host: String,
    peer_asn: u32,
    peer_address: IpAddr,
}

/// The MetalLB site configuration against the VIPs the Core values pin and the peers
/// the ASN layout and the control-plane prefix determine.
fn metallb_rules(ctx: &Ctx) -> eyre::Result<Outcome> {
    let Some(m) = collaterals::metallb_config(ctx)? else {
        return Ok(Outcome::Skip(
            "no collaterals.metallbConfig: another tool renders MetalLB".to_string(),
        ));
    };
    let core = collaterals::core_values(ctx)?;
    let n = &ctx.sd.network;

    let mut vips: Vec<(String, IpAddr)> = core.vips;
    // The site definition's VIPs join the values' own; one the values already carry is
    // not listed twice (the agreement check reports a disagreement between the two).
    for (k, ip) in n.service_vips.all() {
        if !vips.iter().any(|(_, v)| *v == ip) {
            vips.push((format!("serviceVips.{k}"), ip));
        }
    }
    let mut problems: Vec<String> = vips
        .iter()
        .filter(|(_, ip)| !m.pools.iter().any(|p| p.contains(*ip)))
        .map(|(k, ip)| format!("{k} {ip} outside every pool"))
        .collect();

    if m.peers.is_empty() && !m.l2_advertisements.is_empty() {
        let observed = format!(
            "L2 mode: {} VIPs in {} pools, no BGPPeer in {}",
            vips.len(),
            m.pools.len(),
            m.path.display()
        );
        // As over BGP: a pool that holds a VIP is announced only when an advertisement
        // covers it; one that selects pools by label is not evaluated here.
        let mut unevaluated: Vec<String> = Vec::new();
        for pool in &m.pools {
            if !vips.iter().any(|(_, ip)| pool.contains(*ip)) {
                continue;
            }
            let coverage: Vec<collaterals::Coverage> = m
                .l2_advertisements
                .iter()
                .map(|a| a.covers(&pool.name))
                .collect();
            if coverage.contains(&collaterals::Coverage::Named) {
                continue;
            }
            if coverage.contains(&collaterals::Coverage::Unknown) {
                unevaluated.push(pool.name.clone());
                continue;
            }
            let names: Vec<&str> = m
                .l2_advertisements
                .iter()
                .map(|a| a.name.as_str())
                .collect();
            problems.push(format!(
                "IPAddressPool {} holds service VIPs but no L2Advertisement covers it (advertisements: {})",
                pool.name,
                names.join(", ")
            ));
        }
        let observed = if unevaluated.is_empty() {
            observed
        } else {
            format!(
                "{observed}; coverage by selector not evaluated for {}",
                unevaluated.join(", ")
            )
        };
        if !problems.is_empty() {
            return Ok(Outcome::Fail {
                observed: format!("{observed}; {}", problems.join("; ")),
                fix: "edit the pool so every VIP lies inside one".to_string(),
            });
        }
        return Ok(Outcome::Warn {
            observed,
            fix: "L2 advertisement is for development and test; production sites peer over BGP"
                .to_string(),
        });
    }
    for p in &m.peers {
        if !p.unsupported_selectors.is_empty() {
            problems.push(format!(
                "BGPPeer {} uses node selectors this check does not evaluate ({}); select by kubernetes.io/hostname In",
                p.name,
                p.unsupported_selectors.join(", ")
            ));
        }
    }

    let no_dpu = ctx.decided("dpu-in-the-site-controllers", "no DPU");
    let my_asn = n.site_controller_routes_asn();
    let mut expected: Vec<ExpectedPeer> = Vec::new();
    for c in &ctx.sd.hosts.site_controllers {
        if no_dpu {
            for t in c.tor_peers.iter().flatten() {
                expected.push(ExpectedPeer {
                    host: c.host_name.clone(),
                    peer_asn: t.asn,
                    peer_address: t.address,
                });
            }
        } else {
            expected.push(ExpectedPeer {
                host: c.host_name.clone(),
                peer_asn: n.site_controller_asn(c.node_id),
                peer_address: nth_address(
                    &n.forge_control_plane_prefix,
                    c.node_id.saturating_sub(1) * 2,
                ),
            });
        }
    }
    let mut matched = 0;
    for e in &expected {
        // By hostname selector first; by address only for a peer whose selector does not
        // exclude this controller (no selector, or one that names it).
        let found = m
            .peers
            .iter()
            .find(|p| {
                p.hostnames.contains(&e.host) && (!no_dpu || p.peer_address == e.peer_address)
            })
            .or_else(|| {
                m.peers.iter().find(|p| {
                    p.peer_address == e.peer_address
                        && (p.hostnames.is_empty() || p.hostnames.contains(&e.host))
                })
            });
        let Some(p) = found else {
            problems.push(format!("no BGPPeer for {} ({})", e.host, e.peer_address));
            continue;
        };
        let mut wrong = Vec::new();
        if p.my_asn != my_asn {
            wrong.push(format!("myASN {}, layout says {my_asn}", p.my_asn));
        }
        if p.peer_asn != e.peer_asn {
            wrong.push(format!(
                "peerASN {}, layout says {}",
                p.peer_asn, e.peer_asn
            ));
        }
        if p.peer_address != e.peer_address {
            wrong.push(format!(
                "peerAddress {}, layout says {}",
                p.peer_address, e.peer_address
            ));
        }
        if wrong.is_empty() {
            matched += 1;
        } else {
            problems.push(format!("{} ({}): {}", e.host, p.name, wrong.join(", ")));
        }
    }

    // Over BGP, a pool announces only through a BGPAdvertisement that covers it. An
    // advertisement that selects pools by label is not evaluated here; its coverage is
    // reported as unknown rather than guessed either way.
    let mut unevaluated: Vec<String> = Vec::new();
    if m.peers.is_empty() {
        // Neither a BGPPeer nor an L2Advertisement (that case returned above): nothing
        // announces the pools, so a VIP in one is unreachable.
        for pool in &m.pools {
            if vips.iter().any(|(_, ip)| pool.contains(*ip)) {
                problems.push(format!(
                    "IPAddressPool {} holds service VIPs and nothing announces it: no BGPPeer, no L2Advertisement",
                    pool.name
                ));
            }
        }
    } else {
        for pool in &m.pools {
            if !vips.iter().any(|(_, ip)| pool.contains(*ip)) {
                continue;
            }
            let coverage: Vec<collaterals::Coverage> = m
                .advertisements
                .iter()
                .map(|a| a.covers(&pool.name))
                .collect();
            if coverage.contains(&collaterals::Coverage::Named) {
                continue;
            }
            if coverage.contains(&collaterals::Coverage::Unknown) {
                unevaluated.push(pool.name.clone());
                continue;
            }
            let names: Vec<&str> = m.advertisements.iter().map(|a| a.name.as_str()).collect();
            problems.push(format!(
                "IPAddressPool {} holds service VIPs but no BGPAdvertisement covers it (advertisements: {})",
                pool.name,
                if names.is_empty() { "none".to_string() } else { names.join(", ") }
            ));
        }
    }
    let pool_names: Vec<&str> = m.pools.iter().map(|p| p.name.as_str()).collect();
    let mut observed = format!(
        "{} VIPs in {} pools ({}); {matched} of {} peers match",
        vips.len(),
        m.pools.len(),
        pool_names.join(", "),
        expected.len()
    );
    if !unevaluated.is_empty() {
        observed.push_str(&format!(
            "; BGPAdvertisement coverage of {} by selectors not evaluated",
            unevaluated.join(", ")
        ));
    }
    if problems.is_empty() {
        Ok(Outcome::Pass(observed))
    } else {
        Ok(Outcome::Fail {
            observed: format!("{observed}; {}", problems.join("; ")),
            fix: format!(
                "edit the pool or the peer in {}, or the value in site-def.yaml it disagrees with",
                m.path.display()
            ),
        })
    }
}

#[cfg(test)]
mod metallb_tests {
    use super::*;
    use crate::fixture::{Example, observed};

    const METALLB: &str = "helm-prereqs/values/metallb-config.yaml";

    #[test]
    fn metallb_outcomes() {
        struct Case {
            name: &'static str,
            /// `(file, from, to)`; the file is relative to the site definition.
            splices: &'static [(&'static str, &'static str, &'static str)],
            pass: bool,
            observed_has: &'static str,
        }
        let cases = [
            Case {
                name: "the fixture agrees with the example",
                splices: &[],
                pass: true,
                observed_has: "10 VIPs in 2 pools (vip-pool-internal, vip-pool-external); 3 of 3 peers match",
            },
            Case {
                name: "an advertisement restricted to nodes that names another pool does not cover this one",
                splices: &[(
                    METALLB,
                    "    - vip-pool-internal\n    - vip-pool-external\n",
                    "    - vip-pool-external\n  nodeSelectors:\n    - matchLabels: { kubernetes.io/hostname: sc1 }\n",
                )],
                pass: false,
                observed_has: "IPAddressPool vip-pool-internal holds service VIPs but no BGPAdvertisement covers it",
            },
            Case {
                name: "a pool narrowed so a VIP falls outside fails naming it",
                splices: &[(
                    METALLB,
                    "10.10.126.194-10.10.126.207",
                    "10.10.126.194-10.10.126.200",
                )],
                pass: false,
                observed_has: "unbound.externalService 10.10.126.202 outside every pool",
            },
            Case {
                name: "a peer ASN off the layout fails naming both",
                splices: &[(METALLB, "peerASN: 4244766862", "peerASN: 4244766865")],
                pass: false,
                observed_has: "sc2 (sc2-bgp-peer): peerASN 4244766865, layout says 4244766862",
            },
            Case {
                name: "plain-NIC controllers are compared with their ToR peers",
                splices: &[
                    (
                        "site-def.yaml",
                        "dpu-in-the-site-controllers: BF3 DPU in DPU mode",
                        "dpu-in-the-site-controllers: no DPU",
                    ),
                    (
                        "site-def.yaml",
                        "dpuSsh: { address: 10.10.248.55, user: ubuntu },",
                        "torPeers: [{ address: 10.10.248.1, asn: 65034 }], dpuSsh: { address: 10.10.248.55, user: ubuntu },",
                    ),
                ],
                pass: false,
                observed_has: "no BGPPeer for sc1 (10.10.248.1)",
            },
        ];
        for c in cases {
            let ex = Example::new();
            for (file, from, to) in c.splices {
                ex.splice(file, from, to);
            }
            let o = ex.run(metallb_rules);
            assert_eq!(
                matches!(o, Outcome::Pass(_)),
                c.pass,
                "{}: got {o:?}",
                c.name
            );
            let obs = observed(&o);
            assert!(obs.contains(c.observed_has), "{}: observed `{obs}`", c.name);
        }
    }
}

/// `max_connections` the chart's Postgres is rendered with (`templates/postgresql.yaml`).
const POSTGRES_MAX_CONNECTIONS: u64 = 1024;
/// nico-api's `max_database_connections` when the TOML omits it (`cfg/file.rs`).
const CORE_MAX_CONNECTIONS: u64 = 1000;

/// A Kubernetes quantity in GiB: binary (`10Gi`, `1.5Gi`, `512Mi`, `1Ti`), decimal
/// (`10G`, `500M`) or bytes; `None` for a form this row does not read (`1e9`, `10%`).
fn quantity_gi(text: &str) -> Option<f64> {
    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
    let t = text.trim();
    let (num, unit) = t.split_at(
        t.find(|c: char| !(c.is_ascii_digit() || c == '.'))
            .unwrap_or(t.len()),
    );
    let n: f64 = num.parse().ok()?;
    let bytes = match unit {
        "" => 1.0,
        "k" => 1e3,
        "M" => 1e6,
        "G" => 1e9,
        "T" => 1e12,
        "Ki" => 1024.0,
        "Mi" => 1024.0 * 1024.0,
        "Gi" => GIB,
        "Ti" => GIB * 1024.0,
        _ => return None,
    };
    Some(n * bytes / GIB)
}

/// The sizes this row reads, for the message when a quantity is in another form.
const QUANTITY_FORMS: &str = "Gi, Mi, Ti, G, M, T, Ki, k or bytes";

/// Three site controllers, and Postgres sized to the fleet by the sizing table.
fn site_controllers(ctx: &Ctx) -> eyre::Result<Outcome> {
    let controllers = ctx.sd.hosts.site_controllers.len();
    let hosts = ctx.sd.counts.hosts;
    let mut problems = Vec::new();
    let mut warnings = Vec::new();
    let mut observed = vec![format!("{controllers} controllers")];
    if controllers < 3 {
        problems.push(format!(
            "{controllers} site controllers, three are required"
        ));
    }

    let hv = collaterals::helm_prereqs_values(ctx)?;
    // The decision when pinned or implied; else what the values say, as the derived
    // reader reads it: `postgresql.enabled: false` is an external Postgres.
    let external = match ctx.decision("core-postgres") {
        Some(_) => ctx.decided("core-postgres", "External Postgres"),
        None => hv.bool("postgresql.enabled") == Some(false),
    };
    if external {
        observed.push("Postgres rows skipped: External Postgres".to_string());
    } else {
        let core = collaterals::core_values(ctx)?;
        let table = crate::sizing::table();
        let row = table.row_for(hosts);
        let instances = hv.u64("postgresql.instances");
        // An omitted key is the chart's default (helm-prereqs/values.yaml), which the
        // row sizes like a stated value.
        // A stated value in another shape is a failure, not the default.
        let mut or_default = |key: &str, default: &str| -> Option<String> {
            match hv.get(key) {
                None => Some(format!("{default} (chart default)")),
                Some(v) => v
                    .as_str()
                    .map(str::to_string)
                    // A bare YAML integer is a byte count.
                    .or_else(|| v.as_u64().map(|n| n.to_string()))
                    .or_else(|| {
                        problems.push(format!(
                            "{key} is neither a size nor a byte count in {}",
                            hv.name()
                        ));
                        None
                    }),
            }
        };
        let volume = or_default("postgresql.volumeSize", "10Gi");
        let memory = or_default("postgresql.resources.limits.memory", "16Gi");
        // Omitted, nico-api's own default applies (`max_database_connections`, 1000),
        // sized like a stated value; a stated value that is not a count is a failure.
        let connections = match core.toml.get("max_database_connections") {
            None => Some((CORE_MAX_CONNECTIONS, " (nico-api default)")),
            Some(v) => match v.as_integer() {
                Some(c) if c >= 0 => Some((c as u64, "")),
                _ => {
                    problems.push(format!(
                        "max_database_connections {v} is not a non-negative integer"
                    ));
                    None
                }
            },
        };
        observed.push(format!(
            "postgres {} instances, {}, {}, {} connections for {hosts} hosts",
            instances.map_or("?".to_string(), |i| i.to_string()),
            volume.as_deref().unwrap_or("volumeSize not a size"),
            memory.as_deref().unwrap_or("memory limit not a size"),
            connections.map_or("?".to_string(), |(c, how)| format!("{c}{how}")),
        ));
        if hv.bool("postgresql.enabled") == Some(false) {
            problems.push(format!(
                "postgresql.enabled is false in {} while core-postgres is not External Postgres",
                hv.name()
            ));
        }
        if let Some(i) = instances
            && i < 3
        {
            warnings.push(format!(
                "postgresql.instances {i}: one instance is for development"
            ));
        }
        // A value below the table's row warns: the row is a floor, advice while the table
        // is the placeholder; a value in a shape the row cannot read, or above what the
        // chart's Postgres accepts, fails on its own.
        match row {
            None => warnings.push(format!(
                "{hosts} hosts is beyond the sizing table's last row ({} hosts)",
                table.last_max_hosts()
            )),
            Some(r) => {
                // A column the table leaves out is read for its shape and not judged.
                for (key, text, floor) in [
                    (
                        "postgresql.volumeSize",
                        volume.as_deref(),
                        r.postgres_volume_gi,
                    ),
                    (
                        "postgresql.resources.limits.memory",
                        memory.as_deref(),
                        Some(r.postgres_memory_gi),
                    ),
                ] {
                    match text.map(|t| (t, quantity_gi(t.trim_end_matches(" (chart default)")))) {
                        Some((t, None)) => problems.push(format!(
                            "{key} {t} is not a quantity this row reads ({QUANTITY_FORMS})"
                        )),
                        Some((t, Some(v))) if floor.is_some_and(|f| v < f as f64) => {
                            warnings.push(format!(
                                "{key} {t} below the {}Gi the table gives {hosts} hosts",
                                floor.unwrap_or_default()
                            ));
                        }
                        _ => {}
                    }
                }
                if let Some((c, how)) = connections {
                    if let Some(min) = r.db_connections
                        && c < min
                    {
                        warnings.push(format!(
                            "max_database_connections {c}{how} below the {min} the table gives {hosts} hosts"
                        ));
                    }
                    if c > POSTGRES_MAX_CONNECTIONS {
                        problems.push(format!(
                            "max_database_connections {c} above the {POSTGRES_MAX_CONNECTIONS} the chart's Postgres accepts"
                        ));
                    }
                }
            }
        }
        // Which table judged it, in the observed text; the status follows the values.
        observed.push(if table.is_placeholder() {
            "sizing table is the placeholder".to_string()
        } else {
            format!("sizing table from {}", table.source)
        });
    }

    let observed = observed.join("; ");
    if !problems.is_empty() {
        return Ok(Outcome::Fail {
            observed: format!("{observed}; {}", problems.join("; ")),
            fix: "add a site controller, or raise the Postgres value in the helm-prereqs values"
                .to_string(),
        });
    }
    if !warnings.is_empty() {
        return Ok(Outcome::Warn {
            observed: format!("{observed}; {}", warnings.join("; ")),
            fix: "raise the value in the helm-prereqs values to the table's row (register/sizing.yaml, or the file given with --sizing-table)".to_string(),
        });
    }
    Ok(Outcome::Pass(observed))
}

/// A prefix as a collateral states it, with the file and key that hold it.
struct Stated {
    label: String,
    /// The file's name, for messages; two files may share it.
    file: String,
    /// Which file, for the same-file rule.
    source: u8,
    prefix: IpNet,
    gateway: Option<IpAddr>,
}

fn overlaps(a: &IpNet, b: &IpNet) -> bool {
    a.contains(&b.network()) || b.contains(&a.network())
}

/// Every prefix the collaterals state: aligned, pairwise disjoint, gateways inside.
fn prefixes(ctx: &Ctx) -> eyre::Result<Outcome> {
    let mut stated: Vec<Stated> = Vec::new();
    let mut files: u8 = 0;

    if let Some(cp) = collaterals::cp_dpu_site_yaml(ctx)? {
        files += 1;
        let file = cp
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        for (key, p) in [
            (
                "forgeControlPlanePrefix",
                cp.site.forge_control_plane_prefix,
            ),
            ("forgeServiceVipPrefix", cp.site.forge_service_vip_prefix),
            ("forgeDpuLoopbackPrefix", cp.site.forge_dpu_loopback_prefix),
            (
                "fnn.vpcVrfLoopbackPrefix",
                cp.site.fnn.as_ref().and_then(|f| f.vpc_vrf_loopback_prefix),
            ),
        ] {
            if let Some(prefix) = p {
                stated.push(Stated {
                    label: key.to_string(),
                    file: file.clone(),
                    source: files,
                    prefix,
                    gateway: None,
                });
            }
        }
    }

    let core = collaterals::core_values(ctx)?;
    files += 1;
    let core_name = core
        .path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    for (table, entries) in [
        ("networks", core.toml.get("networks")),
        ("pools", core.toml.get("pools")),
    ] {
        for (name, def) in entries
            .and_then(toml::Value::as_table)
            .into_iter()
            .flatten()
        {
            let Some(prefix) = def.get("prefix").and_then(toml::Value::as_str) else {
                continue;
            };
            let Ok(prefix) = prefix.parse::<IpNet>() else {
                return Ok(Outcome::Fail {
                    observed: format!(
                        "[{table}.{name}] prefix {prefix} in {core_name} is not a CIDR"
                    ),
                    fix: "write the prefix as address/length".to_string(),
                });
            };
            let gateway = match def.get("gateway").and_then(toml::Value::as_str) {
                None => None,
                Some(g) => match g.parse() {
                    Ok(ip) => Some(ip),
                    Err(_) => {
                        return Ok(Outcome::Fail {
                            observed: format!(
                                "[{table}.{name}] gateway {g} in {core_name} is not an address"
                            ),
                            fix: "write the gateway as an IP address".to_string(),
                        });
                    }
                },
            };
            stated.push(Stated {
                label: format!("[{table}.{name}]"),
                file: core_name.clone(),
                source: files,
                prefix,
                gateway,
            });
            // A dual-stack segment's IPv6 prefix joins the matrix; Core keeps no gateway for
            // it (the DHCPv6 link address may lie outside it).
            if let Some(p6) = def.get("prefix_v6").and_then(toml::Value::as_str) {
                let Ok(prefix) = p6.parse::<IpNet>() else {
                    return Ok(Outcome::Fail {
                        observed: format!(
                            "[{table}.{name}] prefix_v6 {p6} in {core_name} is not a CIDR"
                        ),
                        fix: "write the prefix as address/length".to_string(),
                    });
                };
                stated.push(Stated {
                    label: format!("[{table}.{name}] prefix_v6"),
                    file: core_name.clone(),
                    source: files,
                    prefix,
                    gateway: None,
                });
            }
        }
    }

    let mut problems: Vec<String> = Vec::new();
    if let Some(m) = collaterals::metallb_config(ctx)? {
        files += 1;
        let m_name = m
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        // MetalLB pools are carved out of the service VIP prefix: each must lie inside it, and
        // none joins the disjointness matrix.
        let service_vips = ctx.sd.network.forge_service_vip_prefix;
        for pool in &m.pools {
            for a in &pool.addresses {
                match a {
                    collaterals::Addresses::Net(prefix) => {
                        if prefix.addr() != prefix.network() {
                            problems.push(format!(
                                "IPAddressPool {} {prefix} ({m_name}) is not aligned to its mask",
                                pool.name
                            ));
                        }
                        if !service_vips.contains(prefix) {
                            problems.push(format!(
                            "IPAddressPool {} {prefix} ({m_name}) outside forgeServiceVipPrefix {service_vips}",
                            pool.name
                        ));
                        }
                    }
                    collaterals::Addresses::Range(first, last) => {
                        if !service_vips.contains(first) || !service_vips.contains(last) {
                            problems.push(format!(
                            "IPAddressPool {} {first}-{last} ({m_name}) outside forgeServiceVipPrefix {service_vips}",
                            pool.name
                        ));
                        }
                    }
                }
            }
        }
    }

    let k = &ctx.sd.network.kubernetes;
    files += 1;
    for (key, prefix) in [
        ("kubernetes.podCidr", k.pod_cidr),
        ("kubernetes.serviceCidr", k.service_cidr),
    ] {
        stated.push(Stated {
            label: key.to_string(),
            file: "site-def.yaml".to_string(),
            source: files,
            prefix,
            gateway: None,
        });
    }

    for s in &stated {
        if s.prefix.addr() != s.prefix.network() {
            problems.push(format!(
                "{} {} ({}) is not aligned to its mask",
                s.label, s.prefix, s.file
            ));
        }
        if let Some(g) = s.gateway
            && !s.prefix.contains(&g)
        {
            problems.push(format!(
                "{} gateway {g} outside {} ({})",
                s.label, s.prefix, s.file
            ));
        }
    }
    // The same prefix in two files is one prefix (agreement decides whether it should be);
    // the same prefix twice in one file is a collision.
    let mut distinct: Vec<&Stated> = Vec::new();
    for (i, s) in stated.iter().enumerate() {
        if let Some(d) = stated[..i]
            .iter()
            .find(|d| d.prefix == s.prefix && d.source == s.source)
        {
            problems.push(format!(
                "{} {} ({}) duplicates {} in the same file",
                s.label, s.prefix, s.file, d.label
            ));
        }
        if !distinct.iter().any(|d| d.prefix == s.prefix) {
            distinct.push(s);
        }
    }
    for (i, a) in distinct.iter().enumerate() {
        for b in &distinct[i + 1..] {
            if overlaps(&a.prefix, &b.prefix) {
                problems.push(format!(
                    "{} {} ({}) overlaps {} {} ({})",
                    a.label, a.prefix, a.file, b.label, b.prefix, b.file
                ));
            }
        }
    }

    if problems.is_empty() {
        Ok(Outcome::Pass(format!(
            "{} prefixes from {files} files, {} distinct, disjoint",
            stated.len(),
            distinct.len()
        )))
    } else {
        Ok(Outcome::Fail {
            observed: problems.join("; "),
            fix: "correct the prefix in the file named".to_string(),
        })
    }
}

/// One value stated in both the site definition and a collateral.
struct Pair {
    key: String,
    site_def: String,
    file: String,
    file_key: String,
    collateral: String,
}

fn pair(
    pairs: &mut Vec<Pair>,
    key: &str,
    sd: impl ToString,
    file: &str,
    file_key: &str,
    value: Option<impl ToString>,
) {
    if let Some(v) = value {
        pairs.push(Pair {
            key: key.to_string(),
            site_def: sd.to_string(),
            file: file.to_string(),
            file_key: file_key.to_string(),
            collateral: v.to_string(),
        });
    }
}

/// A value with every comma-separated address or prefix in its canonical text, so
/// `fd00:0:0:0::/64` in a collateral equals the site definition's `fd00::/64`, and the
/// parts in sorted order, so two files listing the same ranges or servers differently
/// agree; anything else is compared as written.
fn canonical(value: &str) -> String {
    let mut parts: Vec<String> = value
        .split(',')
        .map(|part| {
            let part = part.trim();
            if let Ok(net) = part.parse::<IpNet>() {
                net.to_string()
            } else if let Ok(addr) = part.parse::<IpAddr>() {
                addr.to_string()
            } else {
                part.to_string()
            }
        })
        .collect();
    parts.sort();
    parts.join(", ")
}

/// Addresses `[pools.lo-ip]` can hand out: every address of its `prefix`, or the distinct
/// addresses of its inclusive IPv4 `ranges` (an overlap counted once, a reversed range
/// none); `None` when the TOML has no such pool.
fn lo_ip_capacity(t: &toml::Table) -> Option<u64> {
    let pool = t.get("pools")?.get("lo-ip")?;
    if let Some(prefix) = pool
        .get("prefix")
        .and_then(toml::Value::as_str)
        .and_then(|p| p.parse::<IpNet>().ok())
    {
        return Some(addresses(&prefix));
    }
    let ranges = pool.get("ranges")?.as_array()?;
    let mut spans: Vec<(u32, u32)> = Vec::new();
    for r in ranges {
        let ip = |k: &str| {
            r.get(k)
                .and_then(toml::Value::as_str)
                .and_then(|a| a.parse::<std::net::Ipv4Addr>().ok())
                .map(u32::from)
        };
        let (start, end) = (ip("start")?, ip("end")?);
        if start <= end {
            spans.push((start, end));
        }
    }
    spans.sort_unstable();
    let mut total = 0u64;
    let mut open: Option<(u32, u32)> = None;
    for (start, end) in spans {
        match open {
            Some((s, e)) if start <= e.saturating_add(1) => open = Some((s, e.max(end))),
            Some((s, e)) => {
                total += u64::from(e - s) + 1;
                open = Some((start, end));
            }
            None => open = Some((start, end)),
        }
    }
    if let Some((s, e)) = open {
        total += u64::from(e - s) + 1;
    }
    Some(total)
}

/// Whether `prefix` lies entirely inside the union of `covers`: one entry contains it, or
/// both halves are covered in turn. A half no entry overlaps ends the search at once, so
/// the walk visits only the pieces the entries touch.
fn covered_by(prefix: &IpNet, covers: &[IpNet]) -> bool {
    if covers.iter().any(|c| c.contains(prefix)) {
        return true;
    }
    if !covers.iter().any(|c| overlaps(c, prefix)) || prefix.prefix_len() >= prefix.max_prefix_len()
    {
        return false;
    }
    prefix
        .subnets(prefix.prefix_len() + 1)
        .is_ok_and(|halves| halves.into_iter().all(|h| covered_by(&h, covers)))
}

/// Every range of a TOML pool as `start..end`, comma-joined, so a pool split into pieces
/// does not read as one continuous range.
fn toml_ranges(t: &toml::Table, pool: &str) -> Option<String> {
    let ranges = t.get("pools")?.get(pool)?.get("ranges")?.as_array()?;
    let parts: Vec<String> = ranges
        .iter()
        .filter_map(|r| {
            Some(format!(
                "{}..{}",
                r.get("start")?.as_str()?,
                r.get("end")?.as_str()?
            ))
        })
        .collect();
    (!parts.is_empty()).then(|| parts.join(", "))
}

/// Every quantity the site definition and a collateral both state, compared.
fn agreement(ctx: &Ctx) -> eyre::Result<Outcome> {
    let sd = ctx.sd;
    let n = &sd.network;
    let mut pairs: Vec<Pair> = Vec::new();
    let mut files = 0;
    // The CP-DPU yaml's `siteControllerRoutesAsn`, compared with the TOML below.
    let mut cp_routes_asn: Option<(String, u32)> = None;

    if let Some(cp) = collaterals::cp_dpu_site_yaml(ctx)? {
        files += 1;
        let f = cp
            .path
            .file_name()
            .map(|x| x.to_string_lossy().into_owned())
            .unwrap_or_default();
        let c = &cp.site;
        pair(
            &mut pairs,
            "network.datacenterAsn",
            n.datacenter_asn,
            &f,
            "datacenterAsn",
            c.datacenter_asn,
        );
        // The builder peers every DPU with `bgpAsnStart` and gives node i `bgpAsnStart + i`:
        // that is the MetalLB ASN, `asnLayout.siteControllerRoutes` at a site that did not
        // follow the convention. `siteControllerRoutesAsn` is the ASN half of the route
        // target tagging site-controller routes; the TOML's imports say what that is.
        pair(
            &mut pairs,
            "the MetalLB ASN (asnLayout.siteControllerRoutes or bgpAsnStart)",
            n.site_controller_routes_asn(),
            &f,
            "bgpAsnStart",
            c.bgp_asn_start,
        );
        cp_routes_asn = c.site_controller_routes_asn.map(|a| (f.clone(), a));
        pair(
            &mut pairs,
            "network.siteControllerMtuSize",
            n.site_controller_mtu_size,
            &f,
            "siteControllerMtuSize",
            c.site_controller_mtu_size,
        );
        pair(
            &mut pairs,
            "network.nameServer",
            n.name_server,
            &f,
            "nameServer",
            c.name_server,
        );
        pair(
            &mut pairs,
            "network.forgeControlPlanePrefix",
            n.forge_control_plane_prefix,
            &f,
            "forgeControlPlanePrefix",
            c.forge_control_plane_prefix,
        );
        pair(
            &mut pairs,
            "network.forgeServiceVipPrefix",
            n.forge_service_vip_prefix,
            &f,
            "forgeServiceVipPrefix",
            c.forge_service_vip_prefix,
        );
        pair(
            &mut pairs,
            "network.forgeDpuLoopbackPrefix",
            n.forge_dpu_loopback_prefix,
            &f,
            "forgeDpuLoopbackPrefix",
            c.forge_dpu_loopback_prefix,
        );
        if let (Some(sf), Some(cf)) = (&n.fnn, &c.fnn) {
            pair(
                &mut pairs,
                "network.fnn.controlPlaneVni",
                sf.control_plane_vni,
                &f,
                "fnn.controlPlaneVni",
                cf.control_plane_vni,
            );
            pair(
                &mut pairs,
                "network.fnn.vpcVrfLoopbackPrefix",
                sf.vpc_vrf_loopback_prefix,
                &f,
                "fnn.vpcVrfLoopbackPrefix",
                cf.vpc_vrf_loopback_prefix,
            );
            for (name, file_key, value) in [
                (
                    "managedNodeBmc",
                    "fnn.commonManagedNodeBmcRouteTarget",
                    cf.common_managed_node_bmc_route_target,
                ),
                (
                    "siteController",
                    "fnn.commonSiteControllerRouteTarget",
                    cf.common_site_controller_route_target,
                ),
                (
                    "adminNetwork",
                    "fnn.commonAdminNetworkTarget",
                    cf.common_admin_network_target,
                ),
            ] {
                if let Some(rt) = sf.route_targets.get(name) {
                    pair(
                        &mut pairs,
                        &format!("network.fnn.routeTargets.{name}"),
                        rt,
                        &f,
                        file_key,
                        value,
                    );
                }
            }
            if let Some(imports) = cf
                .route_targets_to_import
                .as_ref()
                .and_then(serde_yaml::Value::as_mapping)
            {
                let keys: Vec<String> = imports
                    .keys()
                    .filter_map(|k| k.as_str().map(str::to_string))
                    .collect();
                // Which targets the control-plane DPU imports beyond the three common
                // ones is the site's design; each one it names has to be a target the
                // site definition knows.
                for key in &keys {
                    let known = sf
                        .route_targets
                        .iter()
                        .find(|(_, rt)| format!("{}:{rt}", n.datacenter_asn) == *key);
                    match known {
                        Some((name, _)) => pair(
                            &mut pairs,
                            &format!("network.fnn.routeTargets.{name}"),
                            key,
                            &f,
                            "fnn.routeTargetsToImport",
                            Some(key.clone()),
                        ),
                        None => pair(
                            &mut pairs,
                            "network.fnn.routeTargets",
                            format!("no entry {key}"),
                            &f,
                            "fnn.routeTargetsToImport",
                            Some(key.clone()),
                        ),
                    }
                }
            }
        }
        let mut sd_ids: Vec<u32> = sd
            .hosts
            .site_controllers
            .iter()
            .map(|s| s.node_id)
            .collect();
        let mut cp_ids: Vec<u32> = c
            .site_controller_nodes
            .iter()
            .filter_map(|x| x.node_id)
            .collect();
        sd_ids.sort_unstable();
        cp_ids.sort_unstable();
        let render = |ids: &[u32]| {
            ids.iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(",")
        };
        pair(
            &mut pairs,
            "siteControllers nodeIds",
            render(&sd_ids),
            &f,
            "siteControllerNodes[].nodeId",
            Some(render(&cp_ids)),
        );
        for node in &c.site_controller_nodes {
            let Some(id) = node.node_id else { continue };
            let Some(sc) = sd.hosts.site_controllers.iter().find(|s| s.node_id == id) else {
                continue;
            };
            pair(
                &mut pairs,
                &format!("siteControllers[nodeId {id}].hostName"),
                &sc.host_name,
                &f,
                "siteControllerNodes[].hostName",
                node.host_name.as_deref(),
            );
            if let Some(mac) = &sc.p0_mac {
                pair(
                    &mut pairs,
                    &format!("siteControllers[nodeId {id}].p0Mac"),
                    mac.to_ascii_lowercase(),
                    &f,
                    "siteControllerNodes[].mac",
                    node.mac.as_deref().map(str::to_ascii_lowercase),
                );
            }
        }
    }

    let core = collaterals::core_values(ctx)?;
    files += 1;
    let f = core
        .path
        .file_name()
        .map(|x| x.to_string_lossy().into_owned())
        .unwrap_or_default();
    let t = &core.toml;
    pair(
        &mut pairs,
        "site.name",
        &sd.site.name,
        &f,
        "sitename",
        t.get("sitename").and_then(toml::Value::as_str),
    );
    // The API's external name sits beside the TOML in the Core values (`nico-api.hostname`,
    // or `hostname` in the subchart's own layout); the TLS SAN and the API URL come from it.
    let core_tree = collaterals::core_values_tree(ctx)?;
    let api = core_tree.root.get("nico-api").unwrap_or(&core_tree.root);
    pair(
        &mut pairs,
        "site.apiHostname",
        &sd.site.api_hostname,
        &f,
        "nico-api.hostname",
        api.get("hostname").and_then(serde_yaml::Value::as_str),
    );
    pair(
        &mut pairs,
        "network.datacenterAsn",
        n.datacenter_asn,
        &f,
        "datacenter_asn",
        t.get("datacenter_asn").and_then(toml::Value::as_integer),
    );
    // Every routing profile imports the site-controller target with one ASN; that is
    // the ASN the control-plane DPUs tag those routes with.
    if let (Some((cp_file, asn)), Some(rt)) = (
        &cp_routes_asn,
        n.fnn
            .as_ref()
            .and_then(|f| f.route_targets.get("siteController")),
    ) {
        let fnn = t.get("fnn");
        let mut profiles: Vec<&toml::Value> = fnn
            .and_then(|f| f.get("routing_profiles"))
            .and_then(toml::Value::as_table)
            .map(|p| p.values().collect())
            .unwrap_or_default();
        profiles.extend(
            fnn.and_then(|f| f.get("admin_vpc"))
                .and_then(|a| a.get("routing_profile")),
        );
        let mut asns: Vec<i64> = Vec::new();
        for import in profiles
            .iter()
            .filter_map(|p| p.get("route_target_imports"))
            .filter_map(toml::Value::as_array)
            .flatten()
        {
            if import.get("vni").and_then(toml::Value::as_integer) == Some(i64::from(*rt))
                && let Some(a) = import.get("asn").and_then(toml::Value::as_integer)
                && !asns.contains(&a)
            {
                asns.push(a);
            }
        }
        if let [one] = asns.as_slice() {
            pair(
                &mut pairs,
                &format!("{cp_file} siteControllerRoutesAsn"),
                asn,
                &f,
                &format!("route_target_imports asn for vni {rt}"),
                Some(*one),
            );
        }
    }
    let seg_pairs = |pairs: &mut Vec<Pair>,
                     sd_key: &str,
                     prefix: IpNet,
                     gateway: Option<IpAddr>,
                     mtu: u32,
                     table: &str,
                     prefix_key: &str| {
        let Some(def) = t.get("networks").and_then(|x| x.get(table)) else {
            return;
        };
        pair(
            pairs,
            &format!("{sd_key}.prefix"),
            prefix,
            &f,
            &format!("[networks.{table}] {prefix_key}"),
            def.get(prefix_key).and_then(toml::Value::as_str),
        );
        if let Some(gateway) = gateway {
            pair(
                pairs,
                &format!("{sd_key}.gateway"),
                gateway,
                &f,
                &format!("[networks.{table}] gateway"),
                def.get("gateway").and_then(toml::Value::as_str),
            );
        }
        pair(
            pairs,
            &format!("{sd_key}.mtu"),
            mtu,
            &f,
            &format!("[networks.{table}] mtu"),
            def.get("mtu").and_then(toml::Value::as_integer),
        );
    };
    // The admin table is the one with `type = "admin"`, whatever its name; two of them
    // cannot both be the planned segment.
    let a = &n.segments.admin;
    let admin_tables: Vec<&String> = t
        .get("networks")
        .and_then(toml::Value::as_table)
        .map(|networks| {
            networks
                .iter()
                .filter(|(_, d)| d.get("type").and_then(toml::Value::as_str) == Some("admin"))
                .map(|(k, _)| k)
                .collect()
        })
        .unwrap_or_default();
    // Core allows several admin networks: the planned segment's table is the one whose
    // prefix matches, and a lone admin table is the planned one whatever its prefix (its
    // differences are the mismatches to report).
    // Either address family may be the planned one; the table's matching key is the one
    // compared, and the gateway only for IPv4, which Core keeps a gateway for.
    let prefix_key = |table: &str| {
        ["prefix", "prefix_v6"].into_iter().find(|k| {
            t.get("networks")
                .and_then(|x| x.get(table))
                .and_then(|d| d.get(k))
                .and_then(toml::Value::as_str)
                .and_then(|p| p.parse::<IpNet>().ok())
                == Some(a.prefix)
        })
    };
    let admin_table = admin_tables
        .iter()
        .find(|table| prefix_key(table).is_some())
        .or_else(|| (admin_tables.len() == 1).then(|| &admin_tables[0]));
    match admin_table {
        Some(table) => {
            let key = prefix_key(table).unwrap_or("prefix");
            seg_pairs(
                &mut pairs,
                "network.segments.admin",
                a.prefix,
                (key == "prefix").then_some(a.gateway),
                a.mtu,
                table,
                key,
            )
        }
        // No admin table is `config.toml.rules`'s row.
        None if admin_tables.is_empty() => {}
        None => pair(
            &mut pairs,
            "network.segments.admin.prefix",
            a.prefix,
            &f,
            "[networks.*] type = \"admin\" prefix",
            Some(format!(
                "none of {} tables ({})",
                admin_tables.len(),
                admin_tables
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        ),
    }
    for seg in &n.segments.host_inband {
        seg_pairs(
            &mut pairs,
            &format!("network.segments.hostInband[{}]", seg.name),
            seg.prefix,
            Some(seg.gateway),
            seg.mtu,
            &seg.name,
            "prefix",
        );
    }
    if let Some(pool) = &n.pools.dpu_loopback {
        let t_pool = t.get("pools").and_then(|p| p.get("lo-ip"));
        let (file_key, got) = match t_pool
            .and_then(|p| p.get("prefix"))
            .and_then(toml::Value::as_str)
        {
            Some(prefix) => ("[pools.lo-ip] prefix", Some(prefix.to_string())),
            None => ("[pools.lo-ip] ranges", toml_ranges(t, "lo-ip")),
        };
        pair(
            &mut pairs,
            "network.pools.dpuLoopback",
            pool.text(),
            &f,
            file_key,
            got,
        );
    }
    if let Some(tag) = &sd.versions.boot_artifacts {
        // Each boot-artifact image nico-pxe copies in, by the tag after its last `:`.
        let containers = core_tree
            .get("nico-pxe.bootArtifactContainers")
            .and_then(serde_yaml::Value::as_sequence)
            .into_iter()
            .flatten()
            .enumerate();
        for (i, c) in containers {
            let image = c.get("image").and_then(serde_yaml::Value::as_str);
            let got = image.and_then(|img| {
                let (_, after) = img.rsplit_once(':')?;
                (!after.contains('/')).then(|| after.to_string())
            });
            pair(
                &mut pairs,
                "versions.bootArtifacts",
                tag,
                &f,
                &format!("nico-pxe.bootArtifactContainers[{i}].image tag"),
                got,
            );
        }
    }
    for (sd_key, range, pool) in [
        ("network.pools.vlan", Some(n.pools.vlan), "vlan-id"),
        ("network.pools.vni", Some(n.pools.vni), "vni"),
        (
            "network.pools.externalVpcVni",
            n.pools.external_vpc_vni,
            "external-vpc-vni",
        ),
    ] {
        if let (Some(r), Some(ranges)) = (range, toml_ranges(t, pool)) {
            pair(
                &mut pairs,
                sd_key,
                format!("{}..{}", r.start, r.end),
                &f,
                &format!("[pools.{pool}] ranges"),
                Some(ranges),
            );
        }
    }
    if let (Some(fnn), Some(ranges)) = (&n.fnn, toml_ranges(t, "vpc-vni"))
        && !fnn.pools_vpc_vni.is_empty()
    {
        let sd_ranges: Vec<String> = fnn
            .pools_vpc_vni
            .iter()
            .map(|r| format!("{}..{}", r.start, r.end))
            .collect();
        pair(
            &mut pairs,
            "network.fnn.poolsVpcVni",
            sd_ranges.join(", "),
            &f,
            "[pools.vpc-vni] ranges",
            Some(ranges),
        );
    }
    if let Some(ranges) = toml_ranges(t, "fnn-asn") {
        // The per-DPU pool is the range less what the layout reserves, as the pools check
        // derives it; rendered as its contiguous runs.
        let range = n.bgp_asn_start..=n.bgp_asn_end;
        let mut reserved: Vec<u32> = vec![n.site_controller_routes_asn()];
        reserved.extend(
            sd.hosts
                .site_controllers
                .iter()
                .map(|c| n.site_controller_asn(c.node_id)),
        );
        reserved.retain(|a| range.contains(a));
        reserved.sort_unstable();
        reserved.dedup();
        // A wider cursor, so a range ending at the largest ASN keeps its last value.
        let mut runs: Vec<String> = Vec::new();
        let mut start = u64::from(n.bgp_asn_start);
        let end_marker = u64::from(n.bgp_asn_end) + 1;
        for r in reserved
            .iter()
            .map(|&r| u64::from(r))
            .chain(std::iter::once(end_marker))
        {
            if r > start {
                runs.push(format!("{start}..{}", r - 1));
            }
            start = r + 1;
        }
        pair(
            &mut pairs,
            "the per-DPU ASN pool (the range less the routes and site-controller ASNs)",
            runs.join(", "),
            &f,
            "[pools.fnn-asn] ranges",
            Some(ranges),
        );
    }
    if let Some(fnn) = &n.fnn {
        pair(
            &mut pairs,
            "network.fnn.vpcVrfLoopbackPrefix",
            fnn.vpc_vrf_loopback_prefix,
            &f,
            "[pools.vpc-dpu-lo] prefix",
            collaterals::toml_str(t, "pools.vpc-dpu-lo.prefix"),
        );
        if let Some(v) = fnn.admin_vpc_vni {
            pair(
                &mut pairs,
                "network.fnn.adminVpcVni",
                v,
                &f,
                "[fnn.admin_vpc] vpc_vni",
                t.get("fnn")
                    .and_then(|x| x.get("admin_vpc"))
                    .and_then(|x| x.get("vpc_vni"))
                    .and_then(toml::Value::as_integer),
            );
        }
    }
    let list = |key: &str| -> Option<String> {
        let items = t.get(key)?.as_array()?;
        Some(
            items
                .iter()
                .filter_map(toml::Value::as_str)
                .collect::<Vec<_>>()
                .join(", "),
        )
    };
    pair(
        &mut pairs,
        "network.ntpServers",
        n.ntp_servers
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", "),
        &f,
        "ntp_servers",
        list("ntp_servers"),
    );
    let v = &n.service_vips;
    pair(
        &mut pairs,
        "network.serviceVips.dhcp",
        v.dhcp,
        &f,
        "dhcp_servers",
        list("dhcp_servers"),
    );
    // Each VIP against its Service's loadBalancerIPs annotation in the Core values.
    let lb = |path: &str| -> Option<String> {
        let ips: Vec<String> = core
            .vips
            .iter()
            .filter(|(k, _)| k == path || k.starts_with(&format!("{path}[")))
            .map(|(_, ip)| ip.to_string())
            .collect();
        (!ips.is_empty()).then(|| ips.join(", "))
    };
    for (key, ip, path) in [
        ("api", Some(v.api), "nico-api.externalService"),
        ("dhcp", Some(v.dhcp), "nico-dhcp.externalService"),
        ("dhcpV6", v.dhcp_v6, "nico-dhcp.v6ExternalService"),
        ("pxe", Some(v.pxe), "nico-pxe.externalService"),
        (
            "sshConsole",
            Some(v.ssh_console),
            "nico-ssh-console-rs.externalService",
        ),
        ("unbound", v.unbound, "unbound.externalService"),
    ] {
        if let Some(ip) = ip {
            pair(
                &mut pairs,
                &format!("network.serviceVips.{key}"),
                ip,
                &f,
                path,
                lb(path),
            );
        }
    }
    for (key, ips, svc) in [("dns", &v.dns, "nico-dns"), ("ntp", &v.ntp, "nico-ntp")] {
        let path = format!("{svc}.externalService.perPodAnnotations");
        pair(
            &mut pairs,
            &format!("network.serviceVips.{key}"),
            ips.iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", "),
            &f,
            &path,
            lb(&path),
        );
    }

    let hv = collaterals::helm_prereqs_values(ctx)?;
    files += 1;
    pair(
        &mut pairs,
        "site.name",
        &sd.site.name,
        &hv.name(),
        "siteName",
        hv.str("siteName"),
    );

    let mismatches: Vec<String> = pairs
        .iter()
        .filter(|p| canonical(&p.site_def) != canonical(&p.collateral))
        .map(|p| {
            format!(
                "{} {} vs {} {} {}",
                p.key, p.site_def, p.file, p.file_key, p.collateral
            )
        })
        .collect();
    if mismatches.is_empty() {
        Ok(Outcome::Pass(format!(
            "{} keys compared across {files} files, all equal",
            pairs.len()
        )))
    } else {
        Ok(Outcome::Fail {
            observed: mismatches.join("; "),
            fix: "pick one as the source and copy the value to the other".to_string(),
        })
    }
}

/// MACs the sample ships with, which a provider forgets to replace.
const PLACEHOLDER_MACS: [&str; 2] = ["aa:aa:aa:aa:aa:aa", "00:00:00:00:00:00"];

/// The control-plane DPU site yaml against the rules `build-dpu-install-iso.sh` enforces,
/// re-expressed here so the provider learns of them before the build.
fn cp_dpu_rules(ctx: &Ctx) -> eyre::Result<Outcome> {
    if ctx.decided("dpu-in-the-site-controllers", "no DPU") {
        return Ok(Outcome::Skip(
            "the site controllers carry no DPU; no ISO is built".to_string(),
        ));
    }
    let cp = match collaterals::cp_dpu_site_yaml(ctx) {
        Ok(Some(cp)) => cp,
        Ok(None) => {
            return Ok(Outcome::Fail {
                observed: "collaterals.cpDpuSiteYaml not named".to_string(),
                fix: "name the scripts/control-plane-dpu site yaml under collaterals".to_string(),
            });
        }
        Err(e) => {
            // deny_unknown_fields: the builder's "Unsupported field in site config".
            return Ok(Outcome::Fail {
                observed: format!("{e:#}"),
                fix: "use only the keys scripts/control-plane-dpu/site-sample.yaml shows"
                    .to_string(),
            });
        }
    };
    let name = cp
        .path
        .file_name()
        .map(|x| x.to_string_lossy().into_owned())
        .unwrap_or_default();
    let c = &cp.site;
    let mut problems: Vec<String> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    let missing = |problems: &mut Vec<String>, present: bool, field: &str| {
        if !present {
            problems.push(format!("'{field}' is required"));
        }
    };
    missing(&mut problems, c.bgp_asn_start.is_some(), "bgpAsnStart");
    missing(
        &mut problems,
        c.forge_dpu_loopback_prefix.is_some(),
        "forgeDpuLoopbackPrefix",
    );
    missing(
        &mut problems,
        c.forge_control_plane_prefix.is_some(),
        "forgeControlPlanePrefix",
    );
    missing(
        &mut problems,
        c.forge_service_vip_prefix.is_some(),
        "forgeServiceVipPrefix",
    );
    missing(
        &mut problems,
        c.site_controller_mtu_size.is_some(),
        "siteControllerMtuSize",
    );
    missing(&mut problems, c.name_server.is_some(), "nameServer");
    missing(
        &mut problems,
        c.ubuntu_password_hash.is_some(),
        "ubuntuPasswordHash",
    );

    let nodes = c.site_controller_nodes.len() as u64;
    if nodes == 0 {
        problems.push("siteControllerNodes is empty".to_string());
    }
    let mut ids: Vec<u32> = c
        .site_controller_nodes
        .iter()
        .filter_map(|x| x.node_id)
        .collect();
    ids.sort_unstable();
    let expected_ids: Vec<u32> = (1..=nodes as u32).collect();
    if ids != expected_ids {
        problems.push(format!(
            "siteControllerNodes: nodeId values must be unique integers from 1 to {nodes}, got: {}",
            ids.iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(" ")
        ));
    }
    for (i, node) in c.site_controller_nodes.iter().enumerate() {
        let who = node
            .host_name
            .clone()
            .unwrap_or_else(|| format!("siteControllerNodes[{i}]"));
        if node.host_name.is_none() {
            problems.push(format!("siteControllerNodes[{i}]: hostName is required"));
        }
        match node.mac.as_deref() {
            None => problems.push(format!("{who}: mac is required")),
            Some(m) if m.parse::<mac_address::MacAddress>().is_err() => {
                problems.push(format!("{who}: mac '{m}' is not a valid MAC address"));
            }
            Some(m) if PLACEHOLDER_MACS.contains(&m.to_ascii_lowercase().as_str()) => {
                warnings.push(format!("{who}: mac {m} is the sample's placeholder"));
            }
            Some(_) => {}
        }
    }
    if let Some(p) = c.forge_dpu_loopback_prefix
        && addresses(&p) <= nodes
    {
        problems.push(format!(
            "forgeDpuLoopbackPrefix {p} is too small for {nodes} node(s) (capacity {} addresses, need > {nodes})",
            addresses(&p)
        ));
    }
    if let Some(p) = c.forge_control_plane_prefix
        && addresses(&p) < nodes * 2
    {
        problems.push(format!(
            "forgeControlPlanePrefix {p} is too small for {nodes} node(s) (capacity {} addresses, need >= {})",
            addresses(&p),
            nodes * 2
        ));
    }
    if let Some(v) = &c.install_with_leaf_password
        && v.as_bool().is_none()
    {
        problems.push(format!(
            "'installWithLeafPassword' must be true or false, got: {}",
            serde_yaml::to_string(v).unwrap_or_default().trim()
        ));
    }

    match ctx.decision("smn") {
        Some(_) if ctx.decided("smn", "SMN on the control-plane DPU") && c.fnn.is_none() => {
            problems.push(
                "smn is SMN on the control-plane DPU but the site yaml has no fnn block"
                    .to_string(),
            );
        }
        Some(_) if !ctx.decided("smn", "SMN on the control-plane DPU") && c.fnn.is_some() => {
            problems.push(
                "the site yaml has an fnn block but smn is not SMN on the control-plane DPU"
                    .to_string(),
            );
        }
        _ => {}
    }
    if let Some(fnn) = &c.fnn {
        missing(
            &mut problems,
            c.site_controller_routes_asn.is_some(),
            "siteControllerRoutesAsn",
        );
        missing(&mut problems, c.datacenter_asn.is_some(), "datacenterAsn");
        missing(
            &mut problems,
            fnn.control_plane_vni.is_some(),
            "fnn.controlPlaneVni",
        );
        missing(
            &mut problems,
            fnn.common_managed_node_bmc_route_target.is_some(),
            "fnn.commonManagedNodeBmcRouteTarget",
        );
        missing(
            &mut problems,
            fnn.common_site_controller_route_target.is_some(),
            "fnn.commonSiteControllerRouteTarget",
        );
        missing(
            &mut problems,
            fnn.common_admin_network_target.is_some(),
            "fnn.commonAdminNetworkTarget",
        );
        missing(
            &mut problems,
            fnn.vpc_vrf_loopback_prefix.is_some(),
            "fnn.vpcVrfLoopbackPrefix",
        );
        if let Some(p) = fnn.vpc_vrf_loopback_prefix
            && addresses(&p) < nodes
        {
            problems.push(format!(
                "fnn.vpcVrfLoopbackPrefix {p} is too small for {nodes} node(s) (capacity {} addresses, need >= {nodes})",
                addresses(&p)
            ));
        }
        if let Some(rt) = &fnn.route_targets_to_import {
            match rt.as_mapping() {
                None => problems.push(
                    "'fnn.routeTargetsToImport' must be a map whose keys are full numeric route targets ('<asn>:<n>: {}' per entry), not a sequence".to_string(),
                ),
                Some(map) => {
                    let canonical = |s: &str| s == "0" || (!s.starts_with('0') && !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()));
                    let fixed: Vec<String> = [
                        fnn.common_managed_node_bmc_route_target,
                        fnn.common_admin_network_target,
                        fnn.common_site_controller_route_target,
                    ]
                    .into_iter()
                    .flatten()
                    .filter_map(|n| c.datacenter_asn.map(|a| format!("{a}:{n}")))
                    .collect();
                    for k in map.keys() {
                        let key = k.as_str().map(str::to_string).unwrap_or_else(|| serde_yaml::to_string(k).unwrap_or_default().trim().to_string());
                        let ok = key.split_once(':').is_some_and(|(a, b)| canonical(a) && canonical(b));
                        if !ok {
                            problems.push(format!("'fnn.routeTargetsToImport': key '{key}' is not a full numeric route target <asn>:<n> (decimal, no leading zeros)"));
                        } else if fixed.contains(&key) {
                            problems.push(format!("'fnn.routeTargetsToImport': key '{key}' duplicates a fixed import the template already renders"));
                        }
                    }
                }
            }
        }
    }

    if !problems.is_empty() {
        return Ok(Outcome::Fail {
            observed: format!("{name}: {}", problems.join("; ")),
            fix: "edit the field named; build-dpu-install-iso.sh enforces the same rule"
                .to_string(),
        });
    }
    if !warnings.is_empty() {
        return Ok(Outcome::Warn {
            observed: format!("{name}: {}", warnings.join("; ")),
            fix: "replace the placeholder with the DPU's p0 MAC".to_string(),
        });
    }
    Ok(Outcome::Pass(format!(
        "{name}: {nodes} nodes{}, builder rules hold",
        if c.fnn.is_some() { ", fnn" } else { "" }
    )))
}

#[cfg(test)]
mod collateral_tests {
    use super::*;
    use crate::fixture::{Example, observed};

    const CORE: &str = "helm-prereqs/values/nico-core.yaml";
    const PREREQS: &str = "helm-prereqs/values.yaml";
    const CP_DPU: &str = "control-plane-dpu/site.yaml";
    const SC3: &str = "    - { hostName: sc3, hostSerial: ABC1233, model: Dell R760, nodeId: 3, bmc: 10.10.248.51, dpuBmc: 10.10.248.54, p0Mac: \"aa:bb:cc:dd:ee:03\",\n        dpuSsh: { address: 10.10.248.57, user: ubuntu },\n        uplinks: [{ port: p0, tor: dc1-tor-01, torPort: Ethernet1/3 }, { port: p1, tor: dc1-tor-02, torPort: Ethernet1/3 }] }\n";

    struct Case {
        name: &'static str,
        splices: &'static [(&'static str, &'static str, &'static str)],
        want: fn(&Outcome) -> bool,
        /// Every entry has to appear in the observed text.
        observed_has: &'static [&'static str],
    }

    fn run_cases(check: fn(&Ctx) -> eyre::Result<Outcome>, cases: &[Case]) {
        for c in cases {
            let ex = Example::new();
            for (file, from, to) in c.splices {
                ex.splice(file, from, to);
            }
            let o = ex.run(check);
            assert!((c.want)(&o), "{}: got {o:?}", c.name);
            let obs = observed(&o);
            for want in c.observed_has {
                assert!(
                    obs.contains(want),
                    "{}: observed `{obs}` lacks `{want}`",
                    c.name
                );
            }
        }
    }

    #[test]
    fn site_controller_outcomes() {
        run_cases(
            site_controllers,
            &[
                Case {
                    name: "the example passes, naming the placeholder table in the observed text",
                    splices: &[],
                    want: |o| matches!(o, Outcome::Pass(_)),
                    observed_has: &[
                        "3 controllers; postgres 3 instances, 50Gi, 8Gi, 1000 (nico-api default) connections for 128 hosts; sizing table from helm-prereqs/values.yaml",
                    ],
                },
                Case {
                    name: "two controllers fail",
                    splices: &[("site-def.yaml", SC3, "")],
                    want: |o| matches!(o, Outcome::Fail { .. }),
                    observed_has: &["2 site controllers, three are required"],
                },
                Case {
                    name: "a memory limit below the table's row warns naming both sizes",
                    splices: &[(PREREQS, "memory: \"8Gi\"", "memory: \"2Gi\"")],
                    want: |o| matches!(o, Outcome::Warn { .. }),
                    observed_has: &[
                        "postgresql.resources.limits.memory 2Gi below the 4Gi the table gives 128 hosts",
                    ],
                },
                Case {
                    name: "a volume size is read for its shape but not judged, the table having no column for it",
                    splices: &[(PREREQS, "volumeSize: \"50Gi\"", "volumeSize: \"1Gi\"")],
                    want: |o| matches!(o, Outcome::Pass(_)),
                    observed_has: &["1Gi"],
                },
                Case {
                    name: "a host count beyond the table's last row warns",
                    splices: &[(
                        "site-def.yaml",
                        "counts: { hosts: 128, dpus: 128,",
                        "counts: { hosts: 6000, dpus: 128,",
                    )],
                    want: |o| matches!(o, Outcome::Warn { .. }),
                    observed_has: &[
                        "6000 hosts is beyond the sizing table's last row (5000 hosts)",
                    ],
                },
                Case {
                    name: "a size that is neither a string nor a byte count fails naming the key",
                    splices: &[(PREREQS, "memory: \"8Gi\"", "memory: false")],
                    want: |o| matches!(o, Outcome::Fail { .. }),
                    observed_has: &[
                        "postgresql.resources.limits.memory is neither a size nor a byte count",
                    ],
                },
                Case {
                    name: "a connection limit that is not a count fails naming it",
                    // A top-level key has to precede the first table.
                    splices: &[(
                        CORE,
                        "      sitename = \"site1\"\n",
                        "      sitename = \"site1\"\n      max_database_connections = \"5000\"\n",
                    )],
                    want: |o| matches!(o, Outcome::Fail { .. }),
                    observed_has: &[
                        "max_database_connections \"5000\" is not a non-negative integer",
                    ],
                },
                Case {
                    name: "a memory limit given as a byte count is read",
                    splices: &[(PREREQS, "memory: \"8Gi\"", "memory: 8589934592")],
                    want: |o| matches!(o, Outcome::Pass(_)),
                    observed_has: &["8589934592"],
                },
                Case {
                    name: "a fractional memory limit is read, not skipped",
                    splices: &[(PREREQS, "memory: \"8Gi\"", "memory: \"1.5Gi\"")],
                    want: |o| matches!(o, Outcome::Warn { .. }),
                    observed_has: &["postgresql.resources.limits.memory 1.5Gi below the"],
                },
                Case {
                    name: "a quantity in a form the row does not read is named",
                    splices: &[(PREREQS, "memory: \"8Gi\"", "memory: \"50%\"")],
                    want: |o| matches!(o, Outcome::Fail { .. }),
                    observed_has: &[
                        "postgresql.resources.limits.memory 50% is not a quantity this row reads",
                    ],
                },
                Case {
                    name: "postgresql.enabled false reads as external Postgres when nothing pins it",
                    splices: &[
                        (
                            PREREQS,
                            "postgresql:\n  enabled: true\n",
                            "postgresql:\n  enabled: false\n",
                        ),
                        (PREREQS, "volumeSize: \"50Gi\"", "volumeSize: \"1Gi\""),
                    ],
                    want: |o| matches!(o, Outcome::Pass(_)),
                    observed_has: &["Postgres rows skipped: External Postgres"],
                },
                Case {
                    name: "external Postgres skips the Postgres rows only",
                    splices: &[
                        (
                            "site-def.yaml",
                            "  build-host: Yes\n",
                            "  build-host: Yes\n  core-postgres: External Postgres\n",
                        ),
                        (PREREQS, "volumeSize: \"50Gi\"", "volumeSize: \"1Gi\""),
                    ],
                    want: |o| matches!(o, Outcome::Pass(_)),
                    observed_has: &["3 controllers; Postgres rows skipped: External Postgres"],
                },
            ],
        );
    }

    #[test]
    fn prefix_outcomes() {
        run_cases(
            prefixes,
            &[
                Case {
                    name: "the consistent set passes",
                    splices: &[],
                    want: |o| matches!(o, Outcome::Pass(_)),
                    observed_has: &["prefixes from 4 files"],
                },
                Case {
                    name: "a TOML segment shifted into another's range fails naming both",
                    splices: &[(
                        CORE,
                        "prefix = \"10.10.72.0/23\"",
                        "prefix = \"10.10.60.0/23\"",
                    )],
                    want: |o| matches!(o, Outcome::Fail { .. }),
                    observed_has: &[
                        "[networks.admin] 10.10.60.0/24 (nico-core.yaml) overlaps [networks.rack1] 10.10.60.0/23 (nico-core.yaml)",
                    ],
                },
                Case {
                    name: "a gateway outside its prefix fails",
                    splices: &[(CORE, "gateway = \"10.10.60.1\"", "gateway = \"10.10.61.1\"")],
                    want: |o| matches!(o, Outcome::Fail { .. }),
                    observed_has: &["[networks.admin] gateway 10.10.61.1 outside 10.10.60.0/24"],
                },
                Case {
                    name: "a prefix repeated in the TOML is caught when another file states it too",
                    splices: &[
                        (
                            CORE,
                            "prefix = \"10.10.60.0/24\"",
                            "prefix = \"10.10.63.128/26\"",
                        ),
                        (
                            CORE,
                            "prefix = \"10.10.72.0/23\"",
                            "prefix = \"10.10.63.128/26\"",
                        ),
                    ],
                    want: |o| matches!(o, Outcome::Fail { .. }),
                    observed_has: &[
                        "[networks.rack1] 10.10.63.128/26 (nico-core.yaml) duplicates [networks.admin] in the same file",
                    ],
                },
                Case {
                    name: "a secondary IPv6 prefix joins the matrix",
                    splices: &[
                        (
                            CORE,
                            "prefix = \"10.10.60.0/24\"",
                            "prefix = \"10.10.60.0/24\"\n      prefix_v6 = \"fd00:1::/64\"",
                        ),
                        (
                            CORE,
                            "prefix = \"10.10.72.0/23\"",
                            "prefix = \"10.10.72.0/23\"\n      prefix_v6 = \"fd00:1::/64\"",
                        ),
                    ],
                    want: |o| matches!(o, Outcome::Fail { .. }),
                    observed_has: &[
                        "[networks.rack1] prefix_v6 fd00:1::/64 (nico-core.yaml) duplicates [networks.admin] prefix_v6 in the same file",
                    ],
                },
                Case {
                    name: "a gateway that is not an address fails naming it",
                    splices: &[(CORE, "gateway = \"10.10.60.1\"", "gateway = \"not-an-ip\"")],
                    want: |o| matches!(o, Outcome::Fail { .. }),
                    observed_has: &[
                        "[networks.admin] gateway not-an-ip in nico-core.yaml is not an address",
                    ],
                },
                Case {
                    name: "a misaligned prefix in the site yaml fails",
                    splices: &[(
                        CP_DPU,
                        "forgeDpuLoopbackPrefix: 10.10.63.128/26",
                        "forgeDpuLoopbackPrefix: 10.10.63.130/26",
                    )],
                    want: |o| matches!(o, Outcome::Fail { .. }),
                    observed_has: &[
                        "forgeDpuLoopbackPrefix 10.10.63.130/26 (site.yaml) is not aligned",
                    ],
                },
            ],
        );
    }

    #[test]
    fn agreement_outcomes() {
        run_cases(
            agreement,
            &[
                Case {
                    name: "the agreeing set passes",
                    splices: &[],
                    want: |o| matches!(o, Outcome::Pass(_)),
                    observed_has: &["keys compared across 3 files, all equal"],
                },
                Case {
                    name: "sitename changed in the TOML fails naming key, values and file",
                    splices: &[(CORE, "sitename = \"site1\"", "sitename = \"site2\"")],
                    want: |o| matches!(o, Outcome::Fail { .. }),
                    observed_has: &["site.name site1 vs nico-core.yaml sitename site2"],
                },
                Case {
                    name: "the API hostname in the Core values is compared with site.apiHostname",
                    splices: &[(
                        CORE,
                        "hostname: api.site1.dc1.example.net",
                        "hostname: api.other.example.net",
                    )],
                    want: |o| matches!(o, Outcome::Fail { .. }),
                    observed_has: &[
                        "site.apiHostname api.site1.dc1.example.net vs nico-core.yaml nico-api.hostname api.other.example.net",
                    ],
                },
                Case {
                    name: "the DPU loopback pool differing from the TOML's lo-ip fails naming both",
                    splices: &[(
                        "site-def.yaml",
                        "dpuLoopback: { prefix: 10.10.62.0/24 }",
                        "dpuLoopback: { prefix: 10.10.62.0/25 }",
                    )],
                    want: |o| matches!(o, Outcome::Fail { .. }),
                    observed_has: &[
                        "network.pools.dpuLoopback 10.10.62.0/25 vs nico-core.yaml [pools.lo-ip] prefix 10.10.62.0/24",
                    ],
                },
                Case {
                    name: "a boot-artifact image tag differing from versions.bootArtifacts fails per container",
                    splices: &[(
                        CORE,
                        "boot-artifacts-aarch64:v2026.10.15",
                        "boot-artifacts-aarch64:v2026.10.14",
                    )],
                    want: |o| matches!(o, Outcome::Fail { .. }),
                    observed_has: &[
                        "versions.bootArtifacts v2026.10.15 vs nico-core.yaml nico-pxe.bootArtifactContainers[1].image tag v2026.10.14",
                    ],
                },
                Case {
                    name: "a service VIP differing from its Service's annotation fails naming both",
                    splices: &[(
                        "site-def.yaml",
                        "unbound: 10.10.126.202",
                        "unbound: 10.10.126.203",
                    )],
                    want: |o| matches!(o, Outcome::Fail { .. }),
                    observed_has: &[
                        "network.serviceVips.unbound 10.10.126.203 vs nico-core.yaml unbound.externalService 10.10.126.202",
                    ],
                },
                Case {
                    name: "a per-pod list differing from the annotations fails as a list",
                    splices: &[(
                        "site-def.yaml",
                        "ntp: [10.10.126.199, 10.10.126.200, 10.10.126.201]",
                        "ntp: [10.10.126.199, 10.10.126.200]",
                    )],
                    want: |o| matches!(o, Outcome::Fail { .. }),
                    observed_has: &[
                        "network.serviceVips.ntp 10.10.126.199, 10.10.126.200 vs nico-core.yaml nico-ntp.externalService.perPodAnnotations 10.10.126.199, 10.10.126.200, 10.10.126.201",
                    ],
                },
                Case {
                    name: "datacenterAsn changed in site-def fails against both files",
                    splices: &[(
                        "site-def.yaml",
                        "datacenterAsn: 65034",
                        "datacenterAsn: 65035",
                    )],
                    want: |o| matches!(o, Outcome::Fail { .. }),
                    observed_has: &[
                        "network.datacenterAsn 65035 vs site.yaml datacenterAsn 65034",
                        "network.datacenterAsn 65035 vs nico-core.yaml datacenter_asn 65034",
                    ],
                },
                Case {
                    name: "the yaml's bgpAsnStart is the MetalLB ASN",
                    splices: &[(
                        "control-plane-dpu/site.yaml",
                        "bgpAsnStart: 4244766860",
                        "bgpAsnStart: 4244766861",
                    )],
                    want: |o| matches!(o, Outcome::Fail { .. }),
                    observed_has: &[
                        "the MetalLB ASN (asnLayout.siteControllerRoutes or bgpAsnStart) 4244766860 vs site.yaml bgpAsnStart 4244766861",
                    ],
                },
                Case {
                    name: "siteControllerRoutesAsn is the ASN the TOML imports the target with",
                    splices: &[(
                        "control-plane-dpu/site.yaml",
                        "siteControllerRoutesAsn: 65034",
                        "siteControllerRoutesAsn: 65035",
                    )],
                    want: |o| matches!(o, Outcome::Fail { .. }),
                    observed_has: &[
                        "site.yaml siteControllerRoutesAsn 65035 vs nico-core.yaml route_target_imports asn for vni 50100 65034",
                    ],
                },
                Case {
                    name: "an import the site definition does not name fails",
                    splices: &[(
                        "control-plane-dpu/site.yaml",
                        "    \"65034:101\": {}\n",
                        "    \"65034:101\": {}\n    \"65034:777\": {}\n",
                    )],
                    want: |o| matches!(o, Outcome::Fail { .. }),
                    observed_has: &[
                        "network.fnn.routeTargets no entry 65034:777 vs site.yaml fnn.routeTargetsToImport 65034:777",
                    ],
                },
                Case {
                    name: "the admin table is found by type, whatever its name",
                    splices: &[(CORE, "[networks.admin]", "[networks.management]")],
                    want: |o| matches!(o, Outcome::Pass(_)),
                    observed_has: &["all equal"],
                },
                Case {
                    name: "a second admin network elsewhere leaves the planned one compared",
                    splices: &[(
                        CORE,
                        "      [secrets]\n",
                        "      [networks.admin2]\n      type = \"admin\"\n      prefix = \"10.10.200.0/24\"\n      gateway = \"10.10.200.1\"\n      mtu = 9000\n      reserve_first = 5\n\n      [secrets]\n",
                    )],
                    want: |o| matches!(o, Outcome::Pass(_)),
                    observed_has: &["all equal"],
                },
                Case {
                    name: "a key absent from the TOML is not a mismatch",
                    splices: &[(
                        CORE,
                        "      [fnn.admin_vpc]\n      enabled = true\n      vpc_vni = 60100\n",
                        "",
                    )],
                    want: |o| matches!(o, Outcome::Pass(_)),
                    observed_has: &["all equal"],
                },
            ],
        );
    }

    #[test]
    fn cp_dpu_outcomes() {
        run_cases(
            cp_dpu_rules,
            &[
                Case {
                    name: "the fixture passes",
                    splices: &[],
                    want: |o| matches!(o, Outcome::Pass(_)),
                    observed_has: &["site.yaml: 3 nodes, fnn, builder rules hold"],
                },
                Case {
                    name: "fnn removed with SMN pinned on the DPU fails",
                    splices: &[
                        (
                            "site-def.yaml",
                            "  build-host: Yes\n",
                            "  build-host: Yes\n  smn: SMN on the control-plane DPU\n",
                        ),
                        (
                            CP_DPU,
                            "fnn:\n  controlPlaneVni: 60020\n  vpcVrfLoopbackPrefix: 10.10.255.0/24\n  commonManagedNodeBmcRouteTarget: 900\n  commonSiteControllerRouteTarget: 50100\n  commonAdminNetworkTarget: 50400\n  routeTargetsToImport:\n    \"65034:101\": {}\n",
                            "",
                        ),
                    ],
                    want: |o| matches!(o, Outcome::Fail { .. }),
                    observed_has: &["no fnn block"],
                },
                Case {
                    name: "fnn without vpcVrfLoopbackPrefix fails naming the field",
                    splices: &[(CP_DPU, "  vpcVrfLoopbackPrefix: 10.10.255.0/24\n", "")],
                    want: |o| matches!(o, Outcome::Fail { .. }),
                    observed_has: &["fnn.vpcVrfLoopbackPrefix"],
                },
                Case {
                    name: "routeTargetsToImport as a sequence fails with the builder's message",
                    splices: &[(
                        CP_DPU,
                        "  routeTargetsToImport:\n    \"65034:101\": {}\n",
                        "  routeTargetsToImport:\n    - \"65034:101\"\n",
                    )],
                    want: |o| matches!(o, Outcome::Fail { .. }),
                    observed_has: &["'fnn.routeTargetsToImport' must be a map"],
                },
                Case {
                    name: "a placeholder MAC warns",
                    splices: &[(CP_DPU, "mac: aa:bb:cc:dd:ee:02", "mac: aa:aa:aa:aa:aa:aa")],
                    want: |o| matches!(o, Outcome::Warn { .. }),
                    observed_has: &["sc2: mac aa:aa:aa:aa:aa:aa is the sample's placeholder"],
                },
                Case {
                    name: "an unknown key fails as the builder would",
                    splices: &[(
                        CP_DPU,
                        "siteControllerMtuSize: 9000",
                        "siteControllerMtuSize: 9000\nmtu: 9000",
                    )],
                    want: |o| matches!(o, Outcome::Fail { .. }),
                    observed_has: &["unknown field `mtu`"],
                },
                Case {
                    name: "no DPU skips",
                    splices: &[(
                        "site-def.yaml",
                        "dpu-in-the-site-controllers: BF3 DPU in DPU mode",
                        "dpu-in-the-site-controllers: no DPU",
                    )],
                    want: |o| matches!(o, Outcome::Skip(_)),
                    observed_has: &["no ISO is built"],
                },
            ],
        );
    }
}

/// The host kind whose DPU is used as a plain NIC, which `dpu_policy = "nic"` expresses. A
/// host booting from its onboard NIC keeps its DPU managed for the data plane, so it is
/// not one of them.
const NIC_KINDS: [&str; 1] = ["DPU hosts NICo flips to NIC mode"];
/// Host kinds that boot on HostInband and need a `hostinband` segment.
const INBAND_KINDS: [&str; 3] = [
    "DPU hosts booting from an onboard NIC",
    "Zero-DPU hosts",
    "DPU hosts NICo flips to NIC mode",
];
const ADMIN_KIND: &str = "DPU hosts booting through the DPU on the Admin segment";

/// The NICo TOML against the rows that are not another check's: segments per host kind,
/// the DPU policy, route targets, security prefixes, and the tables the decisions select.
fn toml_rules(ctx: &Ctx) -> eyre::Result<Outcome> {
    let core = match collaterals::core_values(ctx) {
        Ok(c) => c,
        Err(e) => {
            return Ok(Outcome::Fail {
                observed: format!("{e:#}"),
                fix: "fix the TOML under nico-api.siteConfig.nicoApiSiteConfig at the line quoted"
                    .to_string(),
            });
        }
    };
    let t = &core.toml;
    let name = core
        .path
        .file_name()
        .map(|x| x.to_string_lossy().into_owned())
        .unwrap_or_default();
    let n = &ctx.sd.network;
    let mut problems: Vec<String> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    let get = |path: &str| -> Option<&toml::Value> {
        let mut v = t.get(path.split('.').next()?)?;
        for k in path.split('.').skip(1) {
            v = v.get(k)?;
        }
        Some(v)
    };

    if t.get("sitename").and_then(toml::Value::as_str).is_none() {
        problems.push("sitename missing".to_string());
    }

    let kinds: Vec<String> = ctx
        .decision("managed-host-boot-paths")
        .map(<[String]>::to_vec)
        .unwrap_or_default();
    let networks = t.get("networks").and_then(toml::Value::as_table);
    let types: Vec<(&str, &str)> = networks
        .into_iter()
        .flatten()
        .filter_map(|(k, v)| {
            v.get("type")
                .and_then(toml::Value::as_str)
                .map(|ty| (k.as_str(), ty))
        })
        .collect();
    if kinds.iter().any(|k| k == ADMIN_KIND) && !types.iter().any(|(_, ty)| *ty == "admin") {
        problems.push(
            "no [networks.*] with type = \"admin\" for the hosts booting through the DPU"
                .to_string(),
        );
    }
    if kinds.iter().any(|k| INBAND_KINDS.contains(&k.as_str()))
        && !types.iter().any(|(_, ty)| *ty == "hostinband")
    {
        problems.push(
            "no [networks.*] with type = \"hostinband\" for the hosts booting on HostInband"
                .to_string(),
        );
    }
    for (k, v) in networks.into_iter().flatten() {
        if let Some(mtu) = v.get("mtu").and_then(toml::Value::as_integer)
            && mtu > i64::from(n.path_mtu)
        {
            problems.push(format!(
                "[networks.{k}] mtu {mtu} exceeds pathMtu {}",
                n.path_mtu
            ));
        }
    }

    let expected_policy = if !kinds.is_empty() && kinds.iter().all(|k| k == "Zero-DPU hosts") {
        "ignore"
    } else if kinds.iter().any(|k| NIC_KINDS.contains(&k.as_str())) {
        "nic"
    } else {
        "manage"
    };
    match get("site_explorer.dpu_policy").and_then(toml::Value::as_str) {
        Some(p) if p == expected_policy || (p == "use_as_nic" && expected_policy == "nic") => {}
        Some(p) => problems.push(format!(
            "[site_explorer] dpu_policy {p}, the host kinds need {expected_policy}"
        )),
        None if expected_policy != "manage" => warnings.push(format!(
            "[site_explorer] dpu_policy absent; the host kinds need {expected_policy}"
        )),
        None => {}
    }

    let dc = i64::from(n.datacenter_asn);
    let mut profiles: Vec<(String, &toml::Value)> = get("fnn.routing_profiles")
        .and_then(toml::Value::as_table)
        .into_iter()
        .flatten()
        .map(|(k, v)| (format!("[fnn.routing_profiles.{k}]"), v))
        .collect();
    if let Some(p) = get("fnn.admin_vpc.routing_profile") {
        profiles.push(("[fnn.admin_vpc.routing_profile]".to_string(), p));
    }
    for (label, p) in &profiles {
        for key in ["route_target_imports", "route_targets_on_exports"] {
            for rt in p
                .get(key)
                .and_then(toml::Value::as_array)
                .into_iter()
                .flatten()
            {
                if let Some(asn) = rt.get("asn").and_then(toml::Value::as_integer)
                    && asn != dc
                {
                    problems.push(format!("{label} {key} asn {asn}, datacenterAsn is {dc}"));
                }
            }
        }
    }
    if let (Some(fnn), Some(p)) = (&n.fnn, get("fnn.admin_vpc.routing_profile"))
        && let Some(want) = fnn.route_targets.get("adminNetwork")
    {
        let exports: Vec<i64> = p
            .get("route_targets_on_exports")
            .and_then(toml::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|rt| rt.get("vni").and_then(toml::Value::as_integer))
            .collect();
        if !exports.contains(&i64::from(*want)) {
            problems.push(format!(
                "[fnn.admin_vpc.routing_profile] route_targets_on_exports {exports:?} lacks routeTargets.adminNetwork {want}"
            ));
        }
    }

    let deny: Vec<IpNet> = t
        .get("deny_prefixes")
        .and_then(toml::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(toml::Value::as_str)
        .filter_map(|s| s.parse().ok())
        .collect();
    let mut required: Vec<(String, IpNet)> = vec![(
        "controlPlaneOob".to_string(),
        n.segments.control_plane_oob.prefix,
    )];
    required.extend(
        n.segments
            .managed_host_bmc
            .iter()
            .map(|s| (format!("managedHostBmc {}", s.switch_name), s.prefix)),
    );
    for (label, p) in required {
        if !covered_by(&p, &deny) {
            problems.push(format!("deny_prefixes lacks the {label} prefix {p}"));
        }
    }

    if t.contains_key("anycast_site_prefixes") {
        warnings.push("anycast_site_prefixes is deprecated".to_string());
    }
    for (option, table) in [
        ("UFM", "ib_config"),
        ("NVLink partitioning", "nvlink_config"),
        ("Tracing", "tracing"),
    ] {
        if ctx.decided("optional-components", option) && !t.contains_key(table) {
            warnings.push(format!("[{table}] absent while {option} is ticked"));
        }
    }

    if ctx.decided("credential-store", "Postgres envelope-encrypted") {
        let writer = get("secrets.writer").and_then(toml::Value::as_str);
        if writer != Some("postgres") {
            problems.push(format!(
                "[secrets] writer {}, credential-store is Postgres envelope-encrypted",
                writer.unwrap_or("absent")
            ));
        }
    } else if ctx.decided("credential-store", "Vault KV")
        && get("secrets.writer").and_then(toml::Value::as_str) == Some("postgres")
    {
        problems.push("[secrets] writer postgres, credential-store is Vault KV".to_string());
    }
    // Absent is Core's default (off); a present value that is not a boolean is what Core
    // rejects, not the default.
    match t.get("attestation_enabled").map(toml::Value::as_bool) {
        Some(None) => problems.push("attestation_enabled is not a boolean".to_string()),
        present => {
            let attestation = present.flatten().unwrap_or(false);
            if ctx.decided("measured-boot-attestation", "On") != attestation
                && ctx.decision("measured-boot-attestation").is_some()
            {
                problems.push(format!(
                    "attestation_enabled {attestation}, measured-boot-attestation decided otherwise"
                ));
            }
        }
    }
    let route_servers = t
        .get("enable_route_servers")
        .and_then(toml::Value::as_bool)
        .unwrap_or(false);
    if ctx.decided("evpn-overlay-peering", "Route servers") != route_servers
        && ctx.decision("evpn-overlay-peering").is_some()
    {
        problems.push(format!(
            "enable_route_servers {route_servers}, evpn-overlay-peering decided otherwise"
        ));
    }

    if !problems.is_empty() {
        return Ok(Outcome::Fail {
            observed: format!("{name}: {}", problems.join("; ")),
            fix: "edit the TOML key named under nicoApiSiteConfig".to_string(),
        });
    }
    if !warnings.is_empty() {
        return Ok(Outcome::Warn {
            observed: format!("{name}: {}", warnings.join("; ")),
            fix: "review the TOML key named".to_string(),
        });
    }
    Ok(Outcome::Pass(format!(
        "{name}: TOML parses, {} tables, rules hold",
        t.len()
    )))
}

/// The lines of `forwarders.conf` that name an upstream resolver, comments dropped.
fn forward_addrs(conf: &str) -> Vec<String> {
    conf.lines()
        .map(uncommented)
        .filter_map(|l| l.trim().strip_prefix("forward-addr:"))
        .map(|a| a.trim().to_string())
        .filter(|a| !a.is_empty())
        .collect()
}

/// An unbound configuration line without its `#` comment.
fn uncommented(line: &str) -> &str {
    line.split_once('#').map_or(line, |(code, _)| code)
}

/// The `local-data: "<name>. <ttl> IN A <address>"` records in every `*.conf` string under
/// `unbound.localConfig`, by name without the trailing dot: the raw way to state what the
/// chart's structured `localData` list states, which some sites write directly.
fn raw_local_data(core: &collaterals::Values) -> Vec<(String, Vec<String>)> {
    let mut out: Vec<(String, Vec<String>)> = Vec::new();
    let confs = core
        .get("unbound.localConfig")
        .and_then(serde_yaml::Value::as_mapping);
    for (_, text) in confs.into_iter().flatten() {
        let Some(text) = text.as_str() else { continue };
        for line in text.lines().map(uncommented) {
            let Some(rest) = line.trim().strip_prefix("local-data:") else {
                continue;
            };
            let record = rest.trim().trim_matches('"');
            let mut words = record.split_whitespace();
            let (Some(name), Some(address)) = (words.next(), words.last()) else {
                continue;
            };
            let name = name.trim_end_matches('.').to_string();
            if record.contains(" IN A ") || record.contains(" IN AAAA ") {
                match out.iter_mut().find(|(n, _)| *n == name) {
                    Some((_, addrs)) => addrs.push(address.to_string()),
                    None => out.push((name, vec![address.to_string()])),
                }
            }
        }
    }
    out
}

/// The Helm values against each other: the service VIPs Kea and unbound hand out, the
/// Vault seeds, and the REST, site-agent and certificate settings the decisions select.
fn helm_prereqs_rules(ctx: &Ctx) -> eyre::Result<Outcome> {
    let hv = collaterals::helm_prereqs_values(ctx)?;
    let core = collaterals::core_values_tree(ctx)?;
    let m = collaterals::metallb_config(ctx)?;
    let n = &ctx.sd.network;
    let mut problems: Vec<String> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    let mut rules = 0;
    let core_name = core.name();

    // BGPPeer selectors name site controllers.
    rules += 1;
    for p in m.iter().flat_map(|m| m.peers.iter()) {
        for h in &p.hostnames {
            if !ctx
                .sd
                .hosts
                .site_controllers
                .iter()
                .any(|c| &c.host_name == h)
            {
                problems.push(format!(
                    "BGPPeer {} selects hostname {h}, not a site controller",
                    p.name
                ));
            }
        }
    }

    // One per-pod VIP per replica.
    // An empty annotation is the chart's placeholder: no VIP, as `collect_vips` reads it.
    let lb = |path: &str| -> Option<String> {
        let annotations = core.get(&format!("{path}.annotations"))?;
        collaterals::LB_ANNOTATIONS
            .iter()
            .find_map(|k| annotations.get(*k))?
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let per_pod = |svc: &str| -> Vec<String> {
        core.get(&format!("{svc}.externalService.perPodAnnotations"))
            .and_then(serde_yaml::Value::as_sequence)
            .into_iter()
            .flatten()
            .filter_map(|a| {
                collaterals::LB_ANNOTATIONS
                    .iter()
                    .find_map(|k| a.get(*k))
                    .and_then(serde_yaml::Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
            })
            .collect()
    };
    for (svc, default_replicas) in [("nico-dns", 2u64), ("nico-ntp", 3u64)] {
        rules += 1;
        let replicas = core
            .u64(&format!("{svc}.replicaCount"))
            .unwrap_or(default_replicas);
        let vips = per_pod(svc);
        if vips.len() as u64 != replicas {
            problems.push(format!(
                "{core_name}: {svc} has {} per-pod VIPs for {replicas} replicas",
                vips.len()
            ));
        }
    }

    // Kea hands out the VIPs the other services hold.
    let unbound_vip = lb("unbound.externalService");
    let pxe_vip = lb("nico-pxe.externalService");
    let ntp_vips = per_pod("nico-ntp");
    // The decision when pinned or implied; else what the Core values say, read as the
    // derived reader reads it: the built-in unbound is in use when its service is on.
    let builtin_unbound = match ctx.decision("recursive-resolver-for-managed-machines") {
        Some(_) => !ctx.decided("recursive-resolver-for-managed-machines", "External DNS"),
        None => core.bool("unbound.externalService.enabled") == Some(true),
    };
    rules += 1;
    if pxe_vip.is_none() {
        problems.push(format!(
            "{core_name}: nico-pxe.externalService has no loadBalancerIPs annotation"
        ));
    }
    if builtin_unbound && unbound_vip.is_none() {
        problems.push(format!(
            "{core_name}: unbound.externalService has no loadBalancerIPs annotation"
        ));
    }
    let kea = |key: &str| {
        core.str(&format!("nico-dhcp.config.kea.hookParameters.{key}"))
            .map(str::to_string)
    };
    rules += 1;
    match (
        kea("nameservers"),
        if builtin_unbound {
            unbound_vip.clone()
        } else {
            Some(n.name_server.to_string())
        },
    ) {
        (Some(got), Some(want)) if canonical(&got) != canonical(&want) => problems.push(format!(
            "{core_name}: nico-dhcp hookParameters.nameservers {got}, {} is {want}",
            if builtin_unbound {
                "the unbound VIP"
            } else {
                "network.nameServer"
            }
        )),
        (None, _) => problems.push(format!(
            "{core_name}: nico-dhcp hookParameters.nameservers not set"
        )),
        _ => {}
    }
    rules += 1;
    match (kea("provisioningServer"), &pxe_vip) {
        (Some(got), Some(want)) if canonical(&got) != canonical(want) => problems.push(format!(
            "{core_name}: nico-dhcp hookParameters.provisioningServer {got}, the nico-pxe VIP is {want}"
        )),
        (None, _) => problems.push(format!("{core_name}: nico-dhcp hookParameters.provisioningServer not set")),
        _ => {}
    }
    rules += 1;
    match kea("ntpServer") {
        Some(got) => {
            let mut listed: Vec<String> = got.split(',').map(|s| canonical(s.trim())).collect();
            listed.sort();
            let mut want: Vec<String> = ntp_vips.iter().map(|v| canonical(v)).collect();
            want.sort();
            if !want.is_empty() && listed != want {
                problems.push(format!(
                    "{core_name}: nico-dhcp hookParameters.ntpServer {got}, the nico-ntp VIPs are {}",
                    ntp_vips.join(",")
                ));
            }
        }
        // The chart's default is a placeholder, not the VIPs.
        None if !ntp_vips.is_empty() => problems.push(format!(
            "{core_name}: nico-dhcp hookParameters.ntpServer not set"
        )),
        None => {}
    }

    // unbound: forwarders and local data, or off with external DNS.
    if builtin_unbound {
        rules += 1;
        let conf = core
            .str("unbound.localConfig.forwarders\\.conf")
            .map(str::to_string)
            .or_else(|| {
                core.get("unbound.localConfig")
                    .and_then(|c| c.get("forwarders.conf"))
                    .and_then(serde_yaml::Value::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_default();
        let mut addrs: Vec<String> = forward_addrs(&conf).iter().map(|a| canonical(a)).collect();
        addrs.sort();
        let mut want: Vec<String> = n
            .unbound_forward_addresses
            .iter()
            .map(|a| canonical(&a.to_string()))
            .collect();
        want.sort();
        if addrs.is_empty() {
            warnings.push(format!(
                "{core_name}: unbound forwarders.conf has no forward-addr"
            ));
        } else if !want.is_empty() && addrs != want {
            problems.push(format!(
                "{core_name}: unbound forward-addr {}, unboundForwardAddresses is {}",
                addrs.join(","),
                want.join(",")
            ));
        }
        rules += 1;
        let api_vip = lb("nico-api.externalService");
        let as_vec = |v: &Option<String>| v.clone().map(|v| vec![v]);
        let expected_local: Vec<(&str, Option<Vec<String>>)> = vec![
            ("carbide-api.forge", as_vec(&api_vip)),
            ("nico-api.forge", as_vec(&api_vip)),
            ("carbide-pxe.forge", as_vec(&pxe_vip)),
            ("nico-pxe.forge", as_vec(&pxe_vip)),
            ("carbide-static-pxe.forge", as_vec(&pxe_vip)),
            ("carbide-ntp.forge", Some(ntp_vips.clone())),
            ("nico-ntp.forge", Some(ntp_vips)),
            ("unbound.forge", as_vec(&unbound_vip)),
        ];
        let local = core
            .get("unbound.localData")
            .and_then(serde_yaml::Value::as_sequence);
        // The names resolve through the chart's `localData` list, or through raw
        // `local-data:` records in a `localConfig` conf; either carries them.
        let raw = raw_local_data(&core);
        for (name, want) in expected_local {
            let entry = local
                .into_iter()
                .flatten()
                .find(|e| e.get("name").and_then(serde_yaml::Value::as_str) == Some(name));
            let got: Vec<String> = match (entry, raw.iter().find(|(n, _)| n == name)) {
                (Some(entry), _) => entry
                    .get("addresses")
                    .and_then(serde_yaml::Value::as_sequence)
                    .into_iter()
                    .flatten()
                    .filter_map(|a| a.as_str().map(str::to_string))
                    .collect(),
                (None, Some((_, addrs))) => addrs.clone(),
                (None, None) => {
                    problems.push(format!("{core_name}: unbound localData lacks {name}"));
                    continue;
                }
            };
            if got.is_empty() {
                warnings.push(format!(
                    "{core_name}: unbound localData {name} has no addresses"
                ));
                continue;
            }
            if let Some(want) = want {
                let mut got_c: Vec<String> = got.iter().map(|a| canonical(a)).collect();
                let mut want_c: Vec<String> = want.iter().map(|a| canonical(a)).collect();
                got_c.sort();
                want_c.sort();
                if !want_c.is_empty() && got_c != want_c {
                    problems.push(format!(
                        "{core_name}: unbound localData {name} {}, expected {}",
                        got.join(","),
                        want.join(",")
                    ));
                }
            }
        }
    } else {
        rules += 1;
        if core.bool("unbound.externalService.enabled") == Some(true) {
            warnings.push(format!(
                "{core_name}: unbound.externalService.enabled while the resolver is external DNS"
            ));
        }
    }

    // Vault seeds, unless the credential store is Postgres: the decision when pinned or
    // implied, else the TOML's `[secrets] writer`, as the derived reader reads it.
    let postgres_store = match ctx.decision("credential-store") {
        Some(_) => ctx.decided("credential-store", "Postgres envelope-encrypted"),
        None => collaterals::toml_str(&collaterals::core_values(ctx)?.toml, "secrets.writer")
            .is_some_and(|w| w.eq_ignore_ascii_case("postgres")),
    };
    if !postgres_store {
        rules += 1;
        let seeds: Vec<String> = hv
            .get("vault.kvSeeds")
            .and_then(serde_yaml::Value::as_sequence)
            .into_iter()
            .flatten()
            .filter_map(|s| {
                s.get("path")
                    .and_then(serde_yaml::Value::as_str)
                    .map(str::to_string)
            })
            .collect();
        let root = "machines/all_dpus/factory_default/bmc-metadata-items/root";
        if !seeds.iter().any(|s| s == root) {
            problems.push(format!("{}: vault.kvSeeds lacks {root}", hv.name()));
        }
        // The decision when pinned or implied; else the generation the files select, as
        // `config.derived.decisions` reads it.
        let bf4 = match ctx.decision("managed-host-dpu-generation") {
            Some(_) => {
                ctx.decided("managed-host-dpu-generation", "BF4 generic")
                    || ctx.decided("managed-host-dpu-generation", "BF4 Astra")
            }
            // Only what the files settle: a BF4 that is one candidate among others is
            // not known to apply, and `config.derived.decisions` asks for the pin.
            None => crate::derived::derive_all(ctx)?
                .iter()
                .find(|(id, _)| id == "managed-host-dpu-generation")
                .is_some_and(|(_, d)| {
                    let certain: &[String] = match d {
                        crate::derived::Derived::Value(v) => v,
                        crate::derived::Derived::Ambiguous(_) => &[],
                        crate::derived::Derived::Partial { certain, .. } => certain,
                    };
                    certain.iter().any(|n| n.starts_with("BF4"))
                }),
        };
        if bf4 {
            let bf4 = "machines/all_dpus/factory_default/bmc-metadata-items/bf4";
            if !seeds.iter().any(|s| s == bf4) {
                problems.push(format!(
                    "{}: vault.kvSeeds lacks {bf4} for the BF4 generation",
                    hv.name()
                ));
            }
        }
    }

    // REST group and RMS.
    // Pinned or implied, the decision; else the values themselves say, so the rule only
    // holds a decision to the file, never a guess.
    let cmd = SetupCommand::parse(&ctx.sd.setup);
    let (rest_ticked, says) = match ctx.decision("optional-components") {
        Some(_) => (
            ctx.decided("optional-components", "REST group"),
            "optional-components",
        ),
        None if cmd.runs_setup_sh() => (cmd.installs_rest(), "setup.command"),
        None => (
            hv.bool("rest.enabled").unwrap_or(true),
            "optional-components",
        ),
    };
    rules += 1;
    if let Some(enabled) = hv.bool("rest.enabled")
        && enabled != rest_ticked
    {
        problems.push(format!(
            "{}: rest.enabled {enabled}, {says} says {}",
            hv.name(),
            if rest_ticked {
                "REST group"
            } else {
                "no REST group"
            }
        ));
    }
    if ctx.decision("optional-components").is_some() {
        rules += 1;
        let rms_ticked = ctx.decided("optional-components", "RMS");
        if let Some(enabled) = hv.bool("rms.enabled")
            && enabled != rms_ticked
        {
            problems.push(format!(
                "{}: rms.enabled {enabled}, optional-components says {}",
                hv.name(),
                if rms_ticked { "RMS" } else { "no RMS" }
            ));
        }
    }
    let mut skipped = Vec::new();
    if rest_ticked {
        // Read only when their rows apply, so a REST file a site does not deploy cannot
        // stop the other rows.
        let rest = collaterals::rest_values(ctx)?;
        let agent = collaterals::site_agent_values(ctx)?;
        rules += 1;
        let kc = rest.bool("nico-rest-api.config.keycloak.enabled");
        if ctx.decided("rest-api-auth", "Generic OIDC issuers") && kc != Some(false) {
            problems.push(format!(
                "{}: keycloak.enabled {kc:?}, rest-api-auth is Generic OIDC issuers",
                rest.name()
            ));
        }
        if (ctx.decided("rest-api-auth", "Bundled dev Keycloak")
            || ctx.decided("rest-api-auth", "Your own Keycloak"))
            && kc == Some(false)
        {
            problems.push(format!(
                "{}: keycloak.enabled false, rest-api-auth needs Keycloak",
                rest.name()
            ));
        }
        rules += 1;
        // The agent reads `CORE_GRPC_ADDRESS` and falls back to `CARBIDE_ADDRESS`; the
        // v2.0.x values files set `NICO_ADDRESS` and `CARBIDE_ADDRESS`. The first one set
        // is the address it will use.
        const AGENT_ADDRESS_KEYS: [&str; 3] =
            ["CORE_GRPC_ADDRESS", "NICO_ADDRESS", "CARBIDE_ADDRESS"];
        let address = AGENT_ADDRESS_KEYS
            .iter()
            .find_map(|k| agent.str(&format!("envConfig.{k}")).map(|a| (*k, a)));
        match address {
            Some((_, a)) if a.contains("nico-api") => {}
            Some((k, a)) => {
                problems.push(format!("{}: {k} {a} does not name nico-api", agent.name()))
            }
            None => problems.push(format!(
                "{}: none of CORE_GRPC_ADDRESS, NICO_ADDRESS, CARBIDE_ADDRESS set",
                agent.name()
            )),
        }
        // External Postgres by the same precedence as `config.consistency.site-controllers`.
        let external_postgres = match ctx.decision("core-postgres") {
            Some(_) => ctx.decided("core-postgres", "External Postgres"),
            None => hv.bool("postgresql.enabled") == Some(false),
        };
        if !external_postgres {
            rules += 1;
            if let (Some(db), Some(host)) =
                (agent.str("envConfig.DB_ADDR"), hv.str("postgresql.host"))
                && db != host
            {
                problems.push(format!(
                    "{}: DB_ADDR {db}, postgresql.host is {host}",
                    agent.name()
                ));
            }
        }
    } else {
        skipped.push("REST rows");
    }

    // Root CA.
    if ctx.decision("root-ca").is_some() {
        rules += 1;
        let create = hv.bool("certManager.siteRoot.create").unwrap_or(true);
        if create != ctx.decided("root-ca", "Self-signed by setup.sh") {
            problems.push(format!(
                "{}: certManager.siteRoot.create {create}, root-ca decided otherwise",
                hv.name()
            ));
        }
    }

    let mut observed = format!("{rules} rules across 5 files");
    if !skipped.is_empty() {
        observed.push_str(&format!("; skipped {}", skipped.join(", ")));
    }
    if !problems.is_empty() {
        return Ok(Outcome::Fail {
            observed: format!("{observed}; {}", problems.join("; ")),
            fix: "edit the key named in the file named".to_string(),
        });
    }
    if !warnings.is_empty() {
        return Ok(Outcome::Warn {
            observed: format!("{observed}; {}", warnings.join("; ")),
            fix: "fill the placeholder the chart ships with".to_string(),
        });
    }
    Ok(Outcome::Pass(format!("{observed}, all hold")))
}

/// The derived register entries, read from the files, against what is pinned or implied.
fn derived_decisions(ctx: &Ctx) -> eyre::Result<Outcome> {
    use crate::derived::Derived;
    let values = crate::derived::derive_all(ctx)?;
    let mut problems: Vec<String> = Vec::new();
    let mut ambiguous: Vec<String> = Vec::new();
    let mut compared = 0;
    for (id, d) in &values {
        // An entry the gate read from `setup.command` is not a pin: the reader reads the
        // same command plus the files and variables (`NICO_SERVICEMONITORS` for
        // observability), so its answer is the finer one and an open answer asks for a pin.
        let effective = if ctx
            .decisions
            .implied_by
            .get(id)
            .is_some_and(|by| by == "setup.command")
        {
            None
        } else {
            ctx.decision(id)
        };
        match d {
            Derived::Ambiguous(options) => match effective {
                None => ambiguous.push(format!("{id}: {}", options.join(" | "))),
                Some(pinned) => {
                    compared += 1;
                    let outside: Vec<&String> =
                        pinned.iter().filter(|p| !options.contains(p)).collect();
                    if !outside.is_empty() {
                        problems.push(format!(
                            "{id}: pinned {}, files say one of {}",
                            pinned.join(" + "),
                            options.join(" | ")
                        ));
                    }
                }
            },
            Derived::Partial {
                certain,
                candidates,
            } => match effective {
                None => ambiguous.push(format!(
                    "{id}: {}, and perhaps {}",
                    if certain.is_empty() {
                        "none".to_string()
                    } else {
                        certain.join(" + ")
                    },
                    candidates.join(" | ")
                )),
                Some(pinned) => {
                    compared += 1;
                    let lacking = certain.iter().any(|c| !pinned.contains(c));
                    let outside = pinned
                        .iter()
                        .any(|p| !certain.contains(p) && !candidates.contains(p));
                    if lacking || outside {
                        problems.push(format!(
                            "{id}: pinned {}, files select {} and leave open {}",
                            if pinned.is_empty() {
                                "none".to_string()
                            } else {
                                pinned.join(" + ")
                            },
                            if certain.is_empty() {
                                "none".to_string()
                            } else {
                                certain.join(" + ")
                            },
                            candidates.join(" | ")
                        ));
                    }
                }
            },
            Derived::Value(files_say) => {
                if let Some(pinned) = effective {
                    compared += 1;
                    let mut a: Vec<String> = pinned.to_vec();
                    let mut b = files_say.clone();
                    a.sort();
                    b.sort();
                    if a != b {
                        let how = if ctx.decisions.implied_by.contains_key(id) {
                            "implied"
                        } else {
                            "pinned"
                        };
                        problems.push(format!(
                            "{id}: {how} {}, files say {}",
                            if a.is_empty() {
                                "none".to_string()
                            } else {
                                a.join(" + ")
                            },
                            if b.is_empty() {
                                "none".to_string()
                            } else {
                                b.join(" + ")
                            }
                        ));
                    }
                }
            }
        }
    }
    let observed = format!("{} derived, {compared} pinned or implied", values.len());
    if !problems.is_empty() {
        return Ok(Outcome::Fail {
            observed: format!("{observed}; {}", problems.join("; ")),
            fix: "drop the pin under decisions, or change the file it disagrees with".to_string(),
        });
    }
    if !ambiguous.is_empty() {
        return Ok(Outcome::Warn {
            observed: format!("{observed}; ambiguous: {}", ambiguous.join("; ")),
            fix: "pin the entry under decisions".to_string(),
        });
    }
    Ok(Outcome::Pass(format!("{observed}, all agree")))
}

#[cfg(test)]
mod rules_tests {
    use super::*;
    use crate::fixture::{Example, observed};

    const CORE: &str = "helm-prereqs/values/nico-core.yaml";
    const PREREQS: &str = "helm-prereqs/values.yaml";

    struct Case {
        name: &'static str,
        splices: &'static [(&'static str, &'static str, &'static str)],
        want: fn(&Outcome) -> bool,
        observed_has: &'static [&'static str],
    }

    fn run_cases(check: fn(&Ctx) -> eyre::Result<Outcome>, cases: &[Case]) {
        for c in cases {
            let ex = Example::new();
            for (file, from, to) in c.splices {
                ex.splice(file, from, to);
            }
            let o = ex.run(check);
            assert!((c.want)(&o), "{}: got {o:?}", c.name);
            let obs = observed(&o);
            for want in c.observed_has {
                assert!(
                    obs.contains(want),
                    "{}: observed `{obs}` lacks `{want}`",
                    c.name
                );
            }
        }
    }

    /// The admin check reads the TOML's reservation once the Core values load.
    #[test]
    fn the_toml_reservation_counts_against_admin_capacity() {
        let ex = Example::new();
        ex.splice(CORE, "reserve_first = 5", "reserve_first = 130");
        let o = ex.run(admin_segment);
        assert!(
            matches!(&o, Outcome::Fail { observed, .. } if observed.contains("125 usable addresses for 128 hosts")),
            "{o:?}"
        );
    }

    /// `10.10.250.128/25` as its thirty-two `/30` pieces, quoted and comma-joined.
    /// A `hostinband` segment spliced in before `[firmware_global]`, for the HostInband kinds.
    const HOSTINBAND_SEGMENT: &str = "      [networks.hostinband1]\n      type = \"hostinband\"\n      prefix = \"10.10.74.0/24\"\n      gateway = \"10.10.74.1\"\n      mtu = 9000\n      reserve_first = 2\n\n      [firmware_global]\n";
    const QUARTERS_OF_250_128: &str = "\"10.10.250.128/30\", \"10.10.250.132/30\", \"10.10.250.136/30\", \"10.10.250.140/30\", \"10.10.250.144/30\", \"10.10.250.148/30\", \"10.10.250.152/30\", \"10.10.250.156/30\", \"10.10.250.160/30\", \"10.10.250.164/30\", \"10.10.250.168/30\", \"10.10.250.172/30\", \"10.10.250.176/30\", \"10.10.250.180/30\", \"10.10.250.184/30\", \"10.10.250.188/30\", \"10.10.250.192/30\", \"10.10.250.196/30\", \"10.10.250.200/30\", \"10.10.250.204/30\", \"10.10.250.208/30\", \"10.10.250.212/30\", \"10.10.250.216/30\", \"10.10.250.220/30\", \"10.10.250.224/30\", \"10.10.250.228/30\", \"10.10.250.232/30\", \"10.10.250.236/30\", \"10.10.250.240/30\", \"10.10.250.244/30\", \"10.10.250.248/30\", \"10.10.250.252/30\"";

    #[test]
    fn toml_outcomes() {
        run_cases(
            toml_rules,
            &[
                Case {
                    name: "a deny prefix listed as its halves covers the segment",
                    splices: &[(
                        CORE,
                        "\"10.10.250.128/25\"",
                        "\"10.10.250.128/26\", \"10.10.250.192/26\"",
                    )],
                    want: |o| !matches!(o, Outcome::Fail { .. }),
                    observed_has: &[],
                },
                Case {
                    name: "a deny prefix listed as many small pieces still covers the segment",
                    splices: &[(CORE, "\"10.10.250.128/25\"", QUARTERS_OF_250_128)],
                    want: |o| !matches!(o, Outcome::Fail { .. }),
                    observed_has: &[],
                },
                Case {
                    name: "one half of a deny prefix does not cover the segment",
                    splices: &[(CORE, "\"10.10.250.128/25\"", "\"10.10.250.128/26\"")],
                    want: |o| matches!(o, Outcome::Fail { .. }),
                    observed_has: &["deny_prefixes lacks the managedHostBmc"],
                },
                Case {
                    name: "siteConfig.enabled false leaves the TOML undeployed and fails",
                    splices: &[(
                        CORE,
                        "  siteConfig:\n    enabled: true\n",
                        "  siteConfig:\n    enabled: false\n",
                    )],
                    want: |o| matches!(o, Outcome::Fail { .. }),
                    observed_has: &[
                        "siteConfig.enabled is false, so the NICo TOML is not deployed",
                    ],
                },
                Case {
                    name: "an admin plus zero-DPU fleet keeps dpu_policy manage",
                    splices: &[
                        (
                            "site-def.yaml",
                            "managed-host-boot-paths: [DPU hosts booting through the DPU on the Admin segment]",
                            "managed-host-boot-paths: [DPU hosts booting through the DPU on the Admin segment, Zero-DPU hosts]",
                        ),
                        (CORE, "      [firmware_global]\n", HOSTINBAND_SEGMENT),
                    ],
                    want: |o| matches!(o, Outcome::Pass(_)),
                    observed_has: &["rules hold"],
                },
                Case {
                    name: "a HostInband kind without a hostinband segment fails naming the type",
                    splices: &[(
                        "site-def.yaml",
                        "managed-host-boot-paths: [DPU hosts booting through the DPU on the Admin segment]",
                        "managed-host-boot-paths: [DPU hosts booting through the DPU on the Admin segment, Zero-DPU hosts]",
                    )],
                    want: |o| matches!(o, Outcome::Fail { .. }),
                    observed_has: &[
                        "no [networks.*] with type = \"hostinband\" for the hosts booting on HostInband",
                    ],
                },
                Case {
                    name: "hosts booting from an onboard NIC keep their DPU managed",
                    splices: &[
                        (
                            "site-def.yaml",
                            "managed-host-boot-paths: [DPU hosts booting through the DPU on the Admin segment]",
                            "managed-host-boot-paths: [DPU hosts booting from an onboard NIC]",
                        ),
                        (CORE, "      [firmware_global]\n", HOSTINBAND_SEGMENT),
                    ],
                    want: |o| matches!(o, Outcome::Pass(_)),
                    observed_has: &["rules hold"],
                },
                Case {
                    name: "a present attestation_enabled that is not a boolean fails",
                    splices: &[(
                        CORE,
                        "attestation_enabled = false",
                        "attestation_enabled = \"false\"",
                    )],
                    want: |o| matches!(o, Outcome::Fail { .. }),
                    observed_has: &["attestation_enabled is not a boolean"],
                },
                Case {
                    name: "the fixture passes",
                    splices: &[],
                    want: |o| matches!(o, Outcome::Pass(_)),
                    observed_has: &["nico-core.yaml: TOML parses"],
                },
                Case {
                    name: "a broken table fails with the parser's line",
                    splices: &[(CORE, "      [site_explorer]\n", "      [site_explorer\n")],
                    want: |o| matches!(o, Outcome::Fail { .. }),
                    observed_has: &["the NICo TOML does not parse"],
                },
                Case {
                    name: "dpu_policy against the host kinds fails naming both",
                    splices: &[(CORE, "dpu_policy = \"manage\"", "dpu_policy = \"nic\"")],
                    want: |o| matches!(o, Outcome::Fail { .. }),
                    observed_has: &["[site_explorer] dpu_policy nic, the host kinds need manage"],
                },
                Case {
                    name: "a segment MTU above pathMtu fails",
                    splices: &[(
                        CORE,
                        "      prefix = \"10.10.72.0/23\"\n      gateway = \"10.10.72.1\"\n      mtu = 9000",
                        "      prefix = \"10.10.72.0/23\"\n      gateway = \"10.10.72.1\"\n      mtu = 9216",
                    )],
                    want: |o| matches!(o, Outcome::Fail { .. }),
                    observed_has: &["[networks.rack1] mtu 9216 exceeds pathMtu 9000"],
                },
                Case {
                    name: "the deprecated anycast key warns",
                    splices: &[(
                        CORE,
                        "      route_servers = []\n",
                        "      route_servers = []\n      anycast_site_prefixes = []\n",
                    )],
                    want: |o| matches!(o, Outcome::Warn { .. }),
                    observed_has: &["anycast_site_prefixes is deprecated"],
                },
                Case {
                    name: "a BMC prefix dropped from deny_prefixes fails",
                    splices: &[(CORE, ", \"10.10.250.128/25\"]", "]")],
                    want: |o| matches!(o, Outcome::Fail { .. }),
                    observed_has: &[
                        "deny_prefixes lacks the managedHostBmc dc1-m02-ipmitor-01 prefix 10.10.250.128/25",
                    ],
                },
            ],
        );
    }

    /// A placeholder values file passes `collaterals.present` but cannot be checked: the
    /// loader names it instead of failing every rule about its keys.
    #[test]
    fn an_empty_values_file_is_named_not_checked() {
        let ex = Example::new();
        std::fs::write(ex.path(PREREQS), "").expect("the copy is writable");
        let err = ex
            .try_run(helm_prereqs_rules)
            .expect_err("an empty file is an error");
        assert!(
            format!("{err:#}").contains("values.yaml is empty"),
            "{err:#}"
        );
    }

    #[test]
    fn helm_prereqs_outcomes() {
        run_cases(
            helm_prereqs_rules,
            &[
                Case {
                    name: "the fixture passes",
                    splices: &[],
                    want: |o| matches!(o, Outcome::Pass(_)),
                    observed_has: &["rules across 5 files, all hold"],
                },
                Case {
                    name: "Kea's provisioning server off the PXE VIP fails naming both",
                    splices: &[(
                        CORE,
                        "provisioningServer: 10.10.126.197",
                        "provisioningServer: 10.10.126.190",
                    )],
                    want: |o| matches!(o, Outcome::Fail { .. }),
                    observed_has: &[
                        "provisioningServer 10.10.126.190, the nico-pxe VIP is 10.10.126.197",
                    ],
                },
                Case {
                    name: "a Postgres writer in the TOML lifts the Vault seed rule when nothing pins the store",
                    splices: &[
                        (CORE, "writer = \"vault\"", "writer = \"postgres\""),
                        (
                            PREREQS,
                            "    - path: \"machines/all_dpus/factory_default/bmc-metadata-items/root\"\n      data:\n        UsernamePassword:\n          username: \"root\"\n          password: \"0penBmc\"\n",
                            "",
                        ),
                    ],
                    want: |o| !matches!(o, Outcome::Fail { .. }),
                    observed_has: &[],
                },
                Case {
                    name: "an empty per-pod annotation is a placeholder, not a VIP",
                    splices: &[(
                        CORE,
                        "      - metallb.universe.tf/loadBalancerIPs: 10.10.126.201\n",
                        "      - metallb.universe.tf/loadBalancerIPs: \"\"\n",
                    )],
                    want: |o| matches!(o, Outcome::Fail { .. }),
                    observed_has: &["nico-ntp has 2 per-pod VIPs for 3 replicas"],
                },
                Case {
                    name: "postgresql.enabled false lifts the database-host rule when nothing pins the store",
                    splices: &[
                        (
                            PREREQS,
                            "postgresql:\n  enabled: true\n",
                            "postgresql:\n  enabled: false\n",
                        ),
                        (
                            PREREQS,
                            "host: \"nico-pg-cluster",
                            "host: \"other-pg-cluster",
                        ),
                    ],
                    want: |o| !matches!(o, Outcome::Fail { .. }),
                    observed_has: &[],
                },
                Case {
                    name: "an unbound service switched off reads as external DNS when nothing pins it",
                    splices: &[
                        (
                            CORE,
                            "unbound:\n  externalService:\n    enabled: true\n    annotations:\n      metallb.universe.tf/loadBalancerIPs: 10.10.126.202\n",
                            "unbound:\n  externalService:\n    enabled: false\n",
                        ),
                        (
                            CORE,
                            "nameservers: 10.10.126.202",
                            "nameservers: 10.10.0.11",
                        ),
                    ],
                    want: |o| !matches!(o, Outcome::Fail { .. }),
                    observed_has: &[],
                },
                Case {
                    name: "the v2.0.x site-agent address names stand in for CORE_GRPC_ADDRESS",
                    splices: &[(
                        "helm-prereqs/values/nico-site-agent.yaml",
                        "CORE_GRPC_ADDRESS:",
                        "CARBIDE_ADDRESS:",
                    )],
                    want: |o| !matches!(o, Outcome::Fail { .. }),
                    observed_has: &[],
                },
                Case {
                    name: "a forward-addr with a trailing comment is read without it",
                    splices: &[(
                        CORE,
                        "          forward-addr: 10.10.0.12\n",
                        "          forward-addr: 10.10.0.12   # provisional\n",
                    )],
                    want: |o| !matches!(o, Outcome::Fail { .. }),
                    observed_has: &[],
                },
                Case {
                    name: "raw local-data records in a localConfig conf stand in for the localData list",
                    splices: &[
                        (
                            CORE,
                            "  localData:\n    - name: carbide-api.forge\n      addresses: [10.10.126.208]\n    - name: nico-api.forge\n      addresses: [10.10.126.208]\n    - name: carbide-pxe.forge\n      addresses: [10.10.126.197]\n    - name: nico-pxe.forge\n      addresses: [10.10.126.197]\n    - name: carbide-static-pxe.forge\n      addresses: [10.10.126.197]\n    - name: carbide-ntp.forge\n      addresses: [10.10.126.199, 10.10.126.200, 10.10.126.201]\n    - name: nico-ntp.forge\n      addresses: [10.10.126.199, 10.10.126.200, 10.10.126.201]\n    - name: unbound.forge\n      addresses: [10.10.126.202]\n",
                            "",
                        ),
                        (
                            CORE,
                            "          forward-addr: 10.10.0.12\n",
                            "          forward-addr: 10.10.0.12\n    nico_local.conf: |\n      server:\n          local-zone: \"forge.\" static\n          local-data: \"carbide-api.forge. 300 IN A 10.10.126.208\"\n          local-data: \"nico-api.forge. 300 IN A 10.10.126.208\"\n          local-data: \"carbide-pxe.forge. 300 IN A 10.10.126.197\"\n          local-data: \"nico-pxe.forge. 300 IN A 10.10.126.197\"\n          local-data: \"carbide-static-pxe.forge. 300 IN A 10.10.126.197\"\n          local-data: \"carbide-ntp.forge. 300 IN A 10.10.126.199\"\n          local-data: \"carbide-ntp.forge. 300 IN A 10.10.126.200\"\n          local-data: \"carbide-ntp.forge. 300 IN A 10.10.126.201\"\n          local-data: \"nico-ntp.forge. 300 IN A 10.10.126.199\"\n          local-data: \"nico-ntp.forge. 300 IN A 10.10.126.200\"\n          local-data: \"nico-ntp.forge. 300 IN A 10.10.126.201\"\n          local-data: \"unbound.forge. 300 IN A 10.10.126.202\"   # the resolver\n",
                        ),
                    ],
                    want: |o| !matches!(o, Outcome::Fail { .. }),
                    observed_has: &[],
                },
                Case {
                    name: "a localData entry without addresses warns",
                    splices: &[(
                        CORE,
                        "    - name: unbound.forge\n      addresses: [10.10.126.202]\n",
                        "    - name: unbound.forge\n      addresses: []\n",
                    )],
                    want: |o| matches!(o, Outcome::Warn { .. }),
                    observed_has: &["unbound localData unbound.forge has no addresses"],
                },
                Case {
                    name: "BF4 pinned without its seed entry fails",
                    splices: &[(
                        "site-def.yaml",
                        "  build-host: Yes\n",
                        "  build-host: Yes\n  managed-host-dpu-generation: BF4 generic\n",
                    )],
                    want: |o| matches!(o, Outcome::Fail { .. }),
                    observed_has: &[
                        "vault.kvSeeds lacks machines/all_dpus/factory_default/bmc-metadata-items/bf4",
                    ],
                },
                Case {
                    name: "a BF4 generation the TOML selects needs the bf4 seed when nothing pins it",
                    splices: &[
                        (
                            "site-def.yaml",
                            "  dpu: { model: BlueField-3 B3220, generation: BF3 }\n",
                            "",
                        ),
                        (
                            CORE,
                            "      [secrets]\n",
                            "      [dpf.deployments.bf4_generic]\n      bluefield_software = \"x\"\n\n      [secrets]\n",
                        ),
                    ],
                    want: |o| matches!(o, Outcome::Fail { .. }),
                    observed_has: &[
                        "vault.kvSeeds lacks machines/all_dpus/factory_default/bmc-metadata-items/bf4",
                    ],
                },
                Case {
                    name: "a BF4 that is only a candidate under another deployer does not demand the seed",
                    splices: &[
                        (
                            "site-def.yaml",
                            "  dpu: { model: BlueField-3 B3220, generation: BF3 }\n",
                            "",
                        ),
                        (
                            "site-def.yaml",
                            "command: ./setup.sh -y --core-values values/nico-core.yaml --metallb-config values/metallb-config.yaml --skip-rms",
                            "command: python3 tools/deploy-site.py --site site1",
                        ),
                        (
                            CORE,
                            "      [secrets]\n",
                            "      [dpf.deployments.bf4_generic]\n      bluefield_software = \"x\"\n\n      [secrets]\n",
                        ),
                    ],
                    want: |o| !matches!(o, Outcome::Fail { .. }),
                    observed_has: &[],
                },
                Case {
                    name: "REST enabled in the values while the command skips it fails when nothing pins the group",
                    splices: &[
                        (
                            "site-def.yaml",
                            "  optional-components: [REST group]",
                            "  # optional-components left to the command",
                        ),
                        (
                            "site-def.yaml",
                            "command: ./setup.sh -y --core-values values/nico-core.yaml --metallb-config values/metallb-config.yaml --skip-rms",
                            "command: ./setup.sh -y --core-values values/nico-core.yaml --metallb-config values/metallb-config.yaml --skip-rms --skip-rest",
                        ),
                    ],
                    want: |o| matches!(o, Outcome::Fail { .. }),
                    observed_has: &["rest.enabled true, setup.command says no REST group"],
                },
                Case {
                    name: "REST unticked skips the REST rows",
                    splices: &[
                        (
                            "site-def.yaml",
                            "optional-components: [REST group]",
                            "optional-components: []",
                        ),
                        (PREREQS, "rest:\n  enabled: true", "rest:\n  enabled: false"),
                    ],
                    want: |o| matches!(o, Outcome::Pass(_)),
                    observed_has: &["skipped REST rows"],
                },
            ],
        );
    }

    /// A REST values file a site does not deploy is not read: with the REST group unticked
    /// a broken one leaves the other rows standing.
    #[test]
    fn an_unticked_rest_file_is_not_read() {
        let ex = Example::new();
        ex.splice(
            "site-def.yaml",
            "  optional-components: [REST group]",
            "  optional-components: []",
        );
        ex.splice(
            PREREQS,
            "rest:\n  enabled: true\n",
            "rest:\n  enabled: false\n",
        );
        ex.splice(
            "helm-prereqs/values/nico-rest.yaml",
            "nico-rest-api:",
            "nico-rest-api: [",
        );
        let o = ex
            .try_run(helm_prereqs_rules)
            .expect("the REST file is not read");
        assert!(!matches!(o, Outcome::Fail { .. }), "{o:?}");
    }

    /// An advertisement restricted to nodes or peers covers no pool for certain.
    #[test]
    fn a_restricted_advertisement_leaves_coverage_unevaluated() {
        let ex = Example::new();
        ex.splice(
            "helm-prereqs/values/metallb-config.yaml",
            "spec:\n  ipAddressPools:\n    - vip-pool-internal\n    - vip-pool-external\n",
            "spec:\n  nodeSelectors:\n    - matchLabels:\n        kubernetes.io/hostname: sc1\n  ipAddressPools:\n    - vip-pool-internal\n    - vip-pool-external\n",
        );
        let o = ex.run(metallb_rules);
        assert!(observed(&o).contains("by selectors not evaluated"), "{o:?}");
    }

    /// The managed DPUs take one loopback each from `[pools.lo-ip]`; a pool below the count
    /// fails the pools row.
    #[test]
    fn a_managed_dpu_loopback_pool_too_small_for_the_dpus_fails() {
        let ex = Example::new();
        ex.splice(
            CORE,
            "      [pools.lo-ip]\n      type = \"ipv4\"\n      prefix = \"10.10.62.0/24\"\n",
            "      [pools.lo-ip]\n      type = \"ipv4\"\n      prefix = \"10.10.62.0/26\"\n",
        );
        match ex.run(pools) {
            Outcome::Fail { observed, fix } => {
                assert!(
                    observed.contains("managed DPU loopbacks: 64 for 128 dpus"),
                    "{observed}"
                );
                assert!(fix.contains("[pools.lo-ip]"), "{fix}");
            }
            other => panic!("expected a failure: {other:?}"),
        }
    }

    /// Overlapping ranges hand out each address once: the count is the union, not the sum.
    #[test]
    fn overlapping_loopback_ranges_are_counted_once() {
        let ex = Example::new();
        ex.splice(
            CORE,
            "      [pools.lo-ip]\n      type = \"ipv4\"\n      prefix = \"10.10.62.0/24\"\n",
            "      [pools.lo-ip]\n      type = \"ipv4\"\n      ranges = [{ start = \"10.10.62.1\", end = \"10.10.62.80\" }, { start = \"10.10.62.41\", end = \"10.10.62.120\" }]\n",
        );
        let o = ex.run(pools);
        assert!(
            matches!(o, Outcome::Fail { .. })
                && observed(&o).contains("managed DPU loopbacks: 120 for 128 dpus"),
            "{o:?}"
        );
    }

    /// Over L2 as over BGP, a pool that holds a VIP has to be covered by an advertisement.
    #[test]
    fn l2_mode_requires_an_advertisement_that_covers_each_vip_pool() {
        use serde::Deserialize as _;
        let ex = Example::new();
        let path = ex.path("helm-prereqs/values/metallb-config.yaml");
        let text = std::fs::read_to_string(&path).unwrap();
        let pools: Vec<serde_yaml::Value> = serde_yaml::Deserializer::from_str(&text)
            .map(|d| serde_yaml::Value::deserialize(d).unwrap())
            .filter(|v| v.get("kind").and_then(serde_yaml::Value::as_str) == Some("IPAddressPool"))
            .collect();
        let write = |l2: &str| {
            let mut docs: Vec<String> = pools
                .iter()
                .map(|p| serde_yaml::to_string(p).unwrap())
                .collect();
            docs.push(l2.to_string());
            std::fs::write(&path, docs.join("---\n")).unwrap();
        };
        write(
            "apiVersion: metallb.io/v1beta1\nkind: L2Advertisement\nmetadata:\n  name: unused\nspec:\n  ipAddressPools: [no-such-pool]\n",
        );
        match ex.run(metallb_rules) {
            Outcome::Fail { observed, .. } => {
                assert!(
                    observed.contains("no L2Advertisement covers it"),
                    "{observed}"
                )
            }
            other => panic!("an uncovered pool is a failure: {other:?}"),
        }
        write(
            "apiVersion: metallb.io/v1beta1\nkind: L2Advertisement\nmetadata:\n  name: all\nspec: {}\n",
        );
        match ex.run(metallb_rules) {
            Outcome::Warn { observed, .. } => assert!(observed.contains("L2 mode"), "{observed}"),
            other => panic!("an advertisement over every pool is the L2 warning: {other:?}"),
        }
        write("");
        match ex.run(metallb_rules) {
            Outcome::Fail { observed, .. } => assert!(
                observed.contains("nothing announces it: no BGPPeer, no L2Advertisement"),
                "{observed}"
            ),
            other => panic!("pools without peer or advertisement fail: {other:?}"),
        }
    }

    /// `kubectl apply` reads a `kind: List`; so does the MetalLB loader, item by item.
    #[test]
    fn a_kind_list_manifest_is_read_item_by_item() {
        use serde::Deserialize as _;
        let ex = Example::new();
        let path = ex.path("helm-prereqs/values/metallb-config.yaml");
        let text = std::fs::read_to_string(&path).unwrap();
        let items: Vec<serde_yaml::Value> = serde_yaml::Deserializer::from_str(&text)
            .map(|d| serde_yaml::Value::deserialize(d).unwrap())
            .filter(|v| !v.is_null())
            .collect();
        let mut list = serde_yaml::Mapping::new();
        list.insert("apiVersion".into(), "v1".into());
        list.insert("kind".into(), "List".into());
        list.insert("items".into(), serde_yaml::Value::Sequence(items));
        std::fs::write(&path, serde_yaml::to_string(&list).unwrap()).unwrap();
        let o = ex.run(metallb_rules);
        assert!(matches!(o, Outcome::Pass(_)), "{o:?}");
    }

    #[test]
    fn canonical_text_equates_address_spellings_only() {
        assert_eq!(canonical("fd00:0:0:0::/64"), canonical("fd00::/64"));
        assert_eq!(
            canonical("10.10.0.21,fd00:0:0:0::21"),
            canonical("10.10.0.21, fd00::21")
        );
        assert_ne!(canonical("10.10.0.21"), canonical("10.10.0.22"));
        assert_eq!(canonical("site1"), "site1");
        assert_eq!(
            canonical("1024500..1024600, 1024700..1024800"),
            canonical("1024700..1024800, 1024500..1024600"),
            "the same ranges in another order are the same pool"
        );
    }

    /// Two collaterals may share a file name; the same-file rule tells them apart by path.
    #[test]
    fn files_sharing_a_name_are_still_two_files() {
        let ex = Example::new();
        std::fs::copy(
            ex.path("control-plane-dpu/site.yaml"),
            ex.path("control-plane-dpu/nico-core.yaml"),
        )
        .unwrap();
        ex.splice(
            "site-def.yaml",
            "cpDpuSiteYaml: ./control-plane-dpu/site.yaml",
            "cpDpuSiteYaml: ./control-plane-dpu/nico-core.yaml",
        );
        let o = ex.run(prefixes);
        assert!(matches!(o, Outcome::Pass(_)), "{o:?}");
    }

    /// `setup.sh --metallb-config` takes a kustomize directory; the loader reads its plain
    /// resources and refuses one it would have to render.
    #[test]
    fn a_kustomize_directory_of_plain_resources_loads() {
        let ex = Example::new();
        let dir = ex.path("helm-prereqs/values/metallb");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::rename(
            ex.path("helm-prereqs/values/metallb-config.yaml"),
            dir.join("metallb.yaml"),
        )
        .unwrap();
        std::fs::write(
            dir.join("kustomization.yaml"),
            "resources:\n  - metallb.yaml\n",
        )
        .unwrap();
        ex.splice(
            "site-def.yaml",
            "metallbConfig: ./helm-prereqs/values/metallb-config.yaml",
            "metallbConfig: ./helm-prereqs/values/metallb",
        );
        let o = ex.run(metallb_rules);
        assert!(matches!(o, Outcome::Pass(_)), "{o:?}");
        // Generator settings without generators and build annotations change no resource.
        std::fs::write(
            dir.join("kustomization.yaml"),
            "apiVersion: kustomize.config.k8s.io/v1beta1\nkind: Kustomization\ngeneratorOptions:\n  disableNameSuffixHash: true\nbuildMetadata: [originAnnotations]\nresources:\n  - metallb.yaml\n",
        )
        .unwrap();
        let o = ex.run(metallb_rules);
        assert!(matches!(o, Outcome::Pass(_)), "{o:?}");
        std::fs::write(
            dir.join("kustomization.yaml"),
            "resources:\n  - metallb.yaml\npatches:\n  - path: p.yaml\n",
        )
        .unwrap();
        let err = ex.try_run(metallb_rules).unwrap_err().to_string();
        assert!(err.contains("`patches` cannot be read directly"), "{err}");
    }

    /// A site whose MetalLB another tool renders names no `metallbConfig`: its rows skip,
    /// the rows that read it as one input of several still run, and a `--metallb-config`
    /// switch with nothing to compare against is a failure.
    #[test]
    fn an_unnamed_metallb_config_skips_its_rows() {
        let ex = Example::new();
        ex.splice(
            "site-def.yaml",
            "  metallbConfig: ./helm-prereqs/values/metallb-config.yaml\n",
            "",
        );
        match ex.run(metallb_rules) {
            Outcome::Skip(s) => assert!(s.contains("metallbConfig"), "{s}"),
            o => panic!("expected a skip: {o:?}"),
        }
        for (name, check) in [
            ("prefixes", prefixes as crate::harness::CheckFn),
            ("helm-prereqs", helm_prereqs_rules),
            ("collaterals", collaterals_present),
        ] {
            let o = ex.run(check);
            assert!(!matches!(o, Outcome::Fail { .. }), "{name}: {o:?}");
        }
        let o = ex.run(setup_command);
        assert!(
            observed(&o).contains(
                "--metallb-config values/metallb-config.yaml given, collaterals names no such file"
            ),
            "{o:?}"
        );
    }

    #[test]
    fn derived_outcomes() {
        run_cases(
            derived_decisions,
            &[
                Case {
                    name: "a pin outside an ambiguous entry's candidates fails",
                    splices: &[
                        ("site-def.yaml", "generation: BF3 }", "generation: BF4 }"),
                        (
                            "site-def.yaml",
                            "  build-host: Yes\n",
                            "  build-host: Yes\n  managed-host-dpu-generation: BF3\n",
                        ),
                    ],
                    want: |o| matches!(o, Outcome::Fail { .. }),
                    observed_has: &[
                        "managed-host-dpu-generation: pinned BF3, files say one of BF4 generic | BF4 Astra",
                    ],
                },
                Case {
                    name: "the example agrees with its files",
                    splices: &[],
                    want: |o| matches!(o, Outcome::Pass(_)),
                    observed_has: &["33 derived"],
                },
                Case {
                    name: "an entry the command implied is the reader's to refine, not a pin",
                    splices: &[(
                        "site-def.yaml",
                        "    KUBECONFIG: ~/.kube/site1\n",
                        "    KUBECONFIG: ~/.kube/site1\n    NICO_SERVICEMONITORS: \"true\"\n",
                    )],
                    want: |o| matches!(o, Outcome::Pass(_)),
                    observed_has: &["33 derived"],
                },
                Case {
                    name: "dpf pinned Skip against the example command fails naming both",
                    splices: &[("site-def.yaml", "  # dpf: Install ", "  dpf: Skip ")],
                    want: |o| matches!(o, Outcome::Fail { .. }),
                    observed_has: &["dpf: pinned Skip, files say Install"],
                },
                Case {
                    name: "credential-store pinned to what the TOML holds passes",
                    splices: &[(
                        "site-def.yaml",
                        "  # credential-store: Vault KV ",
                        "  credential-store: Vault KV ",
                    )],
                    want: |o| matches!(o, Outcome::Pass(_)),
                    observed_has: &["all agree"],
                },
                Case {
                    name: "a command that does not run setup.sh leaves its entries ambiguous",
                    splices: &[(
                        "site-def.yaml",
                        "command: ./setup.sh -y --core-values values/nico-core.yaml --metallb-config values/metallb-config.yaml --skip-rms",
                        "command: python3 tools/deploy.py --site dc1",
                    )],
                    want: |o| matches!(o, Outcome::Warn { .. }),
                    observed_has: &[
                        "dpf: Install | Skip",
                        "observability: Full local stack | No pipeline",
                    ],
                },
                Case {
                    name: "a multi-select pin that lacks what the files select fails",
                    splices: &[
                        (
                            "site-def.yaml",
                            "command: ./setup.sh -y --core-values values/nico-core.yaml --metallb-config values/metallb-config.yaml --skip-rms",
                            "command: python3 tools/deploy.py --site dc1",
                        ),
                        (
                            CORE,
                            "      [secrets]\n",
                            "      [tracing]\n      enabled = true\n\n      [secrets]\n",
                        ),
                    ],
                    want: |o| matches!(o, Outcome::Fail { .. }),
                    observed_has: &["optional-components: pinned REST group, files select Tracing"],
                },
                Case {
                    name: "no [secrets] table leaves credential-store ambiguous and warns",
                    splices: &[(
                        CORE,
                        "      [secrets]\n      backends = [\"vault\"]\n      writer = \"vault\"\n",
                        "",
                    )],
                    want: |o| matches!(o, Outcome::Warn { .. }),
                    observed_has: &["credential-store: Vault KV | Postgres envelope-encrypted"],
                },
            ],
        );
    }
}
