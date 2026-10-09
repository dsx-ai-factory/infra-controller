# MAT dual-stack packet test

This opt-in test joins machine-a-tron's (MAT's) tenant configuration snapshots to
real NICo DHCPv4/DHCPv6 exchanges and Linux packet forwarding. It supports two distinct
instances in the same FNN VPC, each with one physical routed interface, IPv4
`/31` and IPv6 `/127` linknets, and `/32` and `/128` host bindings. It rejects
NSGs, virtual functions, L2 segments, overlapping linknets, SLAAC, and single-family
configurations rather than claiming to emulate them.

The test creates temporary namespaces for both endpoints. Both address families
use the same real `forge-dhcp-server` process on each endpoint link:

| Check | IPv4 | IPv6 |
| --- | --- | --- |
| Address acquisition | Discover/Offer/Request/Ack | Solicit/Advertise/Request/Reply |
| Address configuration | DHCP subnet mask (`/31`) | DHCPv6 address (`/128`) |
| Default route | DHCP router option | Simulated Router Advertisement |
| DNS configuration | DHCP option 6 | DHCPv6 option 23 |
| Peer connectivity | Ping and name-based HTTP | Ping and name-based HTTP |
| Optional Internet probe | HTTPS over IPv4 | HTTPS over IPv6 |
| Route loss and recovery | Delete and restore the DHCP-provided route | Withdraw and restore the RA |

The DNS fixtures serve both A and AAAA records for `peer.mat.test`. Each family's
HTTP check uses only that family's DHCP-advertised DNS server. For each endpoint
and family, the test removes the default route, verifies peer traffic fails,
checks the other family still works, restores the route, and verifies recovery.
IPv4 route removal is a fixture operation, not a DHCP lease expiry or renewal.

This is a software test for [IPv6 support](https://github.com/dsx-ai-factory/infra-controller/issues/84).
It does not run a tenant OS, the production DPU agent, FRR, HBN, EVPN, or hardware
offload. The small DHCP clients install one lease; they do not renew or rebind.
The RA and DNS servers are fixtures. MAT's synthetic network status remains
independent of the packet-test result.

## Export active tenant configurations

Add this top-level setting to the MAT TOML configuration and restart MAT:

```toml
tenant_network_snapshot_dir = "/tmp/mat-tenant-network"
```

Omission disables export. MAT creates the directory if necessary; use a dedicated
writable directory for each MAT process. Before creating or restoring devices,
MAT creates the directory if absent, verifies it can create a snapshot file, and
removes existing `<machine-id>.json` snapshots. Preparation errors fail startup. Each primary DPU with an active tenant
writes `<dpu-id>.json` on every successful network-configuration fetch, before
reporting its synthetic observation. A write failure causes the observation to
retry. Files are atomically replaced, contain only selected network fields, and
exclude credentials and tenant user data. They are removed when MAT observes a
return to the admin network, stops the DPU, or shuts down normally. An abrupt kill
can leave files behind; consumers must check `updated_at` rather than treating file
existence as proof that an instance is active.

For the MAT Helm chart, `machineATron.tenantNetworkSnapshotDir` selects the same
path. The chart default is an empty string, which omits the TOML setting. A full
`configFiles.matConfigs` override replaces the generated TOML, so put the setting
in that override instead. The setting adds no privileges or host mount. With
DevSpace, copy the selected JSON files from the MAT container to the test VM just
before running the test. Keep their timestamps and contents intact.

The schema version is `1`. It records `updated_at`, DPU and instance IDs, the host
interface ID, managed-host and instance-network configuration versions,
virtualization type, and tenant interfaces. Interfaces include canonical
family-tagged `addresses`, function type, L2 status, VPC VNI, and whether an NSG is
present. Deprecated address fields are not exported. The packet test rejects
unknown schema versions and snapshots older than 120 seconds or dated in the
future. It also rechecks freshness and configuration identity at the end; a
changed or removed snapshot fails the run. Copied snapshots cannot prove that
MAT remained alive after the copy.

## Run on an isolated Linux VM

The VM needs Python 3, `ip`, `sysctl`, `ip6tables`, `unshare`, `mount`, `ping`,
`iptables`, `curl`, and the compiled NICo DHCP server. Both `net.ipv4.ip_forward`
and `net.ipv6.conf.all.forwarding` must already be `1`;
the test does not change global forwarding settings. Run from the repository root.
Build the DHCP server from the revision being tested:

```bash
cargo build --locked --profile ci-tests -p carbide-dhcp-server --bin forge-dhcp-server
```

Set `snapshot0` and `snapshot1` to absolute paths for two fresh MAT JSON files.
Set `dhcp_server` to the absolute path of the resulting
`target/ci-tests/forge-dhcp-server` binary, accounting for `CARGO_TARGET_DIR` if set.
Choose a new absolute `output` directory that does not yet exist:

```bash
sudo -n python3 dev/machine-a-tron/network/packet_test.py \
  --snapshot "$snapshot0" --snapshot "$snapshot1" \
  --dhcp-server "$dhcp_server" --output "$output"
```

Exactly two `--snapshot` arguments are required. All other required arguments
appear above. Default execution needs no Internet access. During each run it creates
veths, addresses and connected routes, per-namespace resolver files, processes,
and scoped IPv4/IPv6 forwarding rules. MTU is fixed at 1280; this does not validate the
configured production MTU. Normal completion, errors, Ctrl-C, and SIGTERM clean up
these resources. SIGKILL or VM failure cannot run cleanup. A local file lock
prevents concurrent runs using overlapping tenant address space.

`result.json` reports PASS or FAIL and cleanup errors. Each endpoint has `families`
entries keyed by `"4"` and `"6"`, containing its lease, DNS server, and learned
routes. Each connectivity check records its instance ID and numeric `family`.
DHCP, DNS, RA, and HTTP logs are written beside it. Preflight
errors occur before the output directory is created. Failures after setup starts
write a failing result and exit nonzero; cleanup errors also fail the run.

For optional Internet access, add an HTTPS target reachable over both families.
If the lab requires translation, explicitly supply each family's egress interface:

```bash
sudo -n python3 dev/machine-a-tron/network/packet_test.py \
  --snapshot "$snapshot0" --snapshot "$snapshot1" \
  --dhcp-server "$dhcp_server" --output "$output" \
  --internet-url https://www.google.com \
  --nat44-interface eth0 --nat66-interface eth0
```

`--internet-url` defaults to absent and must be HTTPS. Curl forces each family,
disables proxies, checks certificates, and fails on HTTP errors. Success requires
both endpoints to reach the target over both families; there is no family fallback.
`--nat44-interface` and `--nat66-interface` default to absent and each requires
`--internet-url`. Either may be supplied independently. Translation is restricted
to the two endpoint `/32` or `/128` source addresses, respectively. An omitted
interface adds no translation for that family, so the VM's routing must provide
return reachability. NAT success does not prove that public tenant prefixes are
routed through the fabric.

The DNS fixtures answer `peer.mat.test` locally and forward other names to
`--dns-upstream`, a numeric IPv4 or IPv6 address on UDP port 53. Its default is
`127.0.0.53`, the VM's systemd-resolved stub. This default requires that resolver
for Internet-name lookups and does not prove IPv6-only dependencies. DHCPv4 and DHCPv6
advertise each simulated router's address in the corresponding family as the
endpoint's DNS server; this is fixture configuration, not verification of site
DNS settings.

## Regression checks

```bash
python3 -m unittest discover -s dev/machine-a-tron/network -v
cargo test --locked --profile ci-tests -p carbide-machine-a-tron --lib
```
