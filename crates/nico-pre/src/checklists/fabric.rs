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

//! `nico-pre-fabric`: the fabric from a site controller and its DPU, against the datacenter worksheet.

use crate::harness::{Check, todo};

pub(crate) const CHECKS: &[Check] = &[
    Check {
        id: "fabric.bgp.sessions",
        expected: "BGP sessions to the ToRs established with the expected ASNs",
        detail: "Reads: on the controller's DPU, over `siteControllers[].dpuSsh` or locally from \
                 media on the controller: FRR `show bgp summary json` for both uplinks and both \
                 address families; the ASN layout (the controller's ASN by `nodeId`, the ToR ASN \
                 from `datacenterAsn` or `torPeers`).\n\
                 Pass: a session Established on each uplink for IPv4 and for IPv6 where \
                 configured, with the local and remote ASNs the layout states; observed `sc1: 2 \
                 uplinks, 2 AFIs Established, local 4244766861, remote 65034`.\n\
                 Fail: a session not Established, or a different ASN; observed names uplink, \
                 state and ASNs; fix: the ToR config or the ASN layout.\n\
                 Skip: when `no DPU` and the host runs no FRR (MetalLB peers then; see \
                 `platform.metallb.ready`); a controller without `dpuSsh` when not run from \
                 media, reported as deferred.\n\
                 Test: fixture JSON from `show bgp summary json`; cases: both Established passes; \
                 one Idle fails; a remote ASN differing fails naming both; `no DPU` skips.",
        run: todo,
    },
    Check {
        id: "fabric.mtu.path",
        expected: "the path MTU holds on every segment",
        detail: "Reads: `network.pathMtu` and every segment's gateway (admin, controlPlaneOob, \
                 managedHostBmc, hostInband); ICMP echo with do-not-fragment from the vantage \
                 point of each segment: the site controller when run from media, else \
                 `hosts.probeHosts.admin`, `hosts.bmcProbeHost` and \
                 `hosts.probeHosts.hostInband[]`.\n\
                 Pass: a do-not-fragment probe of a `pathMtu`-byte IP packet (headers included) \
                 is answered on every segment; a larger path is not a fault, since `pathMtu` is \
                 a floor the segments rely on; observed `5 segments: 9000 passes`.\n\
                 Fail: the `pathMtu` probe dropped on a segment; observed names the segment and \
                 the largest size that passed; fix: the switch MTU.\n\
                 Skip: a segment with no vantage point stated, reported as deferred.\n\
                 Test: the probe against a veth pair with a set MTU in a Linux-only test (ignored \
                 on macOS); cases: equal passes; MTU 1500 on the link fails naming 1500; an \
                 unreachable segment defers.",
        run: todo,
    },
    Check {
        id: "fabric.evpn.route-targets",
        expected: "the route targets of the table are imported and exported as stated",
        detail: "Reads: with SMN on the control-plane DPU, over `dpuSsh` or from media: FRR `show \
                 bgp l2vpn evpn route json` and the rendered startup configuration on the DPU; \
                 `network.fnn.routeTargets` and the three EVPN route targets (managedNodeBmc, \
                 siteController, adminNetwork).\n\
                 Pass: the rendered configuration active and every route target of the table \
                 imported and exported as stated, with routes received for the three; observed `3 \
                 RTs received; 4 imports, 4 exports as stated`.\n\
                 Fail: a route target missing from import or export, or no routes received for \
                 one; observed names the RT and the direction; fix: the ToR or the site yaml.\n\
                 Skip: when `smn` is `SMN on the ToR` or `No SMN`.\n\
                 Test: fixture FRR JSON and a rendered configuration; cases: all present passes; \
                 one RT absent from import fails naming it; no routes for `adminNetwork` fails; \
                 SMN on the ToR skips.",
        run: todo,
    },
    Check {
        id: "fabric.uplinks.lldp",
        expected: "both uplinks up with LLDP neighbours on the expected ToRs",
        detail: "Reads: on the DPU over `dpuSsh` or from media (the host itself without a DPU): \
                 `lldpcli show neighbors -f json`; `siteControllers[].uplinks` (port, tor, \
                 torPort) for the expected neighbours.\n\
                 Pass: both uplinks up with an LLDP neighbour on each, matching the expected ToR \
                 when named; observed `sc1: p0 -> dc1-tor-01 Ethernet1/1, p1 -> dc1-tor-02 \
                 Ethernet1/1`.\n\
                 Warn: a single uplink by design (one neighbour); `uplinks` absent, so only the \
                 neighbour count is checked.\n\
                 Fail: an uplink down or without a neighbour; observed names the port; fix: \
                 cabling or LLDP on the ToR.\n\
                 Skip: never.\n\
                 Test: fixture `lldpcli` JSON; cases: two neighbours passes; one missing fails; a \
                 wrong ToR fails naming both; a single uplink warns.",
        run: todo,
    },
    Check {
        id: "fabric.evpn.peering",
        expected: "EVPN peering per the decision: a session to each ToR with type 5 routes, or each route server answering over multiple hops",
        detail: "Reads: the `evpn-overlay-peering` decision; on the DPU over `dpuSsh` or from \
                 media, FRR `show bgp l2vpn evpn summary json` and the type 5 route count; the \
                 route servers from the TOML.\n\
                 Pass: `Option 1`: an EVPN session Established to each ToR with type 5 routes \
                 received; `Option 2`: each route server Established over multiple hops, at least \
                 two servers; observed per peer.\n\
                 Fail: a peer not Established, zero type 5 routes, fewer than two route servers; \
                 fix: the peer config.\n\
                 Skip: when `no DPU` without an overlay.\n\
                 Test: fixture JSON per option; cases: `Option 1` with both ToRs Established \
                 passes; one Idle fails; `Option 2` with two servers passes; one server fails \
                 naming the count.",
        run: todo,
    },
    Check {
        id: "fabric.loopback.filter",
        expected: "the loopback filter rejects a test prefix outside the allowed set",
        detail: "Reads: on the DPU over `dpuSsh` or from media, the FRR prefix lists and route \
                 maps applied to the uplink sessions; `forgeDpuLoopbackPrefix` and the allowed \
                 set; a test prefix outside it.\n\
                 Pass: the filter rejects the test prefix and permits the loopback prefix (a dry \
                 evaluation of the route map, or a test static advertised and checked with \
                 `advertised-routes`); observed `test prefix 192.0.2.0/24 rejected; loopback /26 \
                 permitted`.\n\
                 Fail: the test prefix permitted, or the loopback prefix rejected; fix: the \
                 prefix list.\n\
                 Skip: when `no DPU`.\n\
                 Test: fixture prefix lists; cases: the correct list passes; a permit-any list \
                 fails; the loopback prefix missing from the list fails naming it.",
        run: todo,
    },
    Check {
        id: "fabric.default-route",
        expected: "the default route present per the chosen mechanism",
        detail: "Reads: `network.defaultRoute` (`bgp` or `static`); on the DPU over `dpuSsh` and \
                 on the host over SSH: `ip -j route show default` and FRR `show ip route \
                 0.0.0.0/0 json`.\n\
                 Pass: a default route present from the stated mechanism on each vantage point; \
                 observed `sc1: default via bgp from 2 nexthops`.\n\
                 Fail: no default, or one from another mechanism (a static where BGP was \
                 planned); fix: the ToR advertisement or the static.\n\
                 Skip: when `defaultRoute` is not stated.\n\
                 Test: fixture route JSON; cases: a BGP default passes; none fails; a static \
                 where BGP was planned fails naming both; `defaultRoute` absent skips.",
        run: todo,
    },
    Check {
        id: "fabric.bgp.leaf-password",
        expected: "the leaf BGP session password establishes to each leaf",
        detail: "Reads: the leaf password by source (the site yaml's leaf-password fields), the \
                 leaf addresses; the FRR session state per leaf (`show bgp neighbors json`) and \
                 whether a password is in force.\n\
                 Pass: the session to each leaf Established with the password in force; observed \
                 `2 leaves Established with password`.\n\
                 Fail: a session stuck in Connect or Active with the password set (a mismatch), \
                 or Established without a password while one is configured; fix: align the \
                 password on the leaf.\n\
                 Skip: when no leaf password is configured.\n\
                 Test: fixture neighbour JSON; cases: Established with password passes; Active \
                 with password fails naming the mismatch; no password configured skips.",
        run: todo,
    },
    Check {
        id: "fabric.dns.resolution",
        expected: "site-zone forward and reverse lookups and apiHostname resolve from the admin and a BMC segment",
        detail: "Reads: from `hosts.probeHosts.admin` and `hosts.bmcProbeHost` (or the controller \
                 from media): forward and reverse lookups in `site.dnsZone` through `nameServer`; \
                 `apiHostname` against the API VIP (`kubernetes.apiVip` or the pinned \
                 `serviceVips.api`); with `External DNS`, every `.forge` name through the \
                 external resolver.\n\
                 Pass: every lookup answers the expected record within the timeout; observed \
                 `site1.dc1: 4 lookups ok; api -> 10.10.126.193`.\n\
                 Fail: NXDOMAIN, SERVFAIL, a timeout, or `apiHostname` resolving elsewhere; \
                 observed names the query and the answer; fix: the zone.\n\
                 Skip: the `.forge` rows when the resolver is `Built-in unbound`; a segment with \
                 no vantage point stated, reported as deferred.\n\
                 Test: a stub DNS server in the test; cases: all answer passes; `apiHostname` \
                 pointing elsewhere fails naming both addresses; NXDOMAIN on the reverse lookup \
                 fails; `Built-in unbound` skips the forge rows.",
        run: todo,
    },
    Check {
        id: "fabric.ntp.sync",
        expected: "NTP answers from the admin and a BMC segment with stratum below 16 and offset under a second",
        detail: "Reads: `network.ntpServers`; an SNTP query from `hosts.probeHosts.admin` and \
                 `hosts.bmcProbeHost` (or the controller from media).\n\
                 Pass: each server answers with stratum below 16 and offset under one second; \
                 observed `2 servers: stratum 2, offset 3 ms`.\n\
                 Fail: no answer, stratum 16 (unsynchronised), or offset over a second; observed \
                 per server; fix: the NTP source.\n\
                 Skip: a segment with no vantage point stated, reported as deferred.\n\
                 Test: a local SNTP responder in the test with set stratum and offset; cases: \
                 stratum 2 passes; 16 fails; offset 2 s fails; no answer fails naming the \
                 timeout.",
        run: todo,
    },
    Check {
        id: "fabric.dhcp.relay",
        expected: "a discover from each BMC and hostInband segment reaches the planned DHCP VIP with Option 82, and no other server answers",
        detail: "Reads: the BMC and hostInband segments; the planned nico-dhcp VIP \
                 (`serviceVips.dhcp` or derived), held on a site controller or the probe host for \
                 the check; a DHCPDISCOVER sent from `hosts.bmcProbeHost` and each \
                 `hosts.probeHosts.hostInband[]` entry.\n\
                 Pass: the discover arrives at the VIP from every segment with Option 82 (circuit \
                 and remote id) and no other server offers; observed `3 segments relayed with \
                 Option 82; no foreign offers`.\n\
                 Fail: a segment whose discover does not arrive, arrives without Option 82, or \
                 draws an offer from another server (named by address); fix: the relay on the \
                 ToR, or the rogue server.\n\
                 Skip: when the VIP cannot be held before setup.sh, reported as such (the check \
                 then runs after install); a segment with no probe host stated, reported as \
                 deferred.\n\
                 Test: a Linux-only test with two network namespaces and a relay (ignored on \
                 macOS); cases: relayed with Option 82 passes; without Option 82 fails; a second \
                 responder fails naming it.",
        run: todo,
    },
    Check {
        id: "fabric.registry.tls",
        expected: "the registry's TLS handshake succeeds from a DPU-network vantage point",
        detail: "Reads: `hosts.registryHost` from a DPU-network vantage point (the DPU over \
                 `dpuSsh`, or from media on the controller); `caSource`.\n\
                 Pass: the TLS handshake completes with a chain to `caSource` and `/v2/` answers; \
                 observed `registry.site1 from 10.10.63.130: TLS ok`.\n\
                 Fail: handshake failure, an untrusted chain, or no route; fix: the CA or the \
                 route.\n\
                 Skip: when `dpf` is `Skip` (no DPU pulls images).\n\
                 Test: a local TLS server with a test CA; cases: trusted passes; a wrong CA fails \
                 naming the issuer; refused fails; `Skip` skips.",
        run: todo,
    },
];
