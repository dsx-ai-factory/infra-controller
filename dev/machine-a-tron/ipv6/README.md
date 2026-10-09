# MAT IPv6 packet test

This opt-in test joins machine-a-tron's (MAT's) tenant configuration snapshots to
real NICo DHCPv6 exchanges and Linux packet forwarding. It supports two distinct
instances in the same FNN VPC, each with one physical routed interface, IPv4
`/31` and IPv6 `/127` linknets, and `/32` and `/128` host bindings. It rejects
NSGs, virtual functions, L2 segments, overlapping linknets, SLAAC, and single-family
configurations rather than claiming to emulate them.

The test creates temporary namespaces for both endpoints. It exercises
Solicit/Advertise/Request/Reply against `forge-dhcp-server`, checks the assigned
IPv6 addresses and DNS option, learns default routes through simulated Router
Advertisements, and verifies reciprocal IPv6 ping and HTTP. The HTTP probe uses
`peer.mat.test`, resolved through the DNS server advertised by DHCPv6. It then
withdraws each default route, verifies that peer traffic fails, restores the
route, and verifies recovery.

This is a software test for [IPv6 support](https://github.com/dsx-ai-factory/infra-controller/issues/84).
It does not run a tenant OS, the production DPU agent, FRR, HBN, EVPN, or hardware
offload. IPv4 addresses are static. The small test DHCP client installs one lease;
it does not renew or rebind. The RA and DNS servers are fixtures. MAT's synthetic
network status remains independent of the packet-test result.

## Export active tenant configurations

Add this top-level setting to the MAT TOML configuration and restart MAT:

```toml
tenant_network_snapshot_dir = "/tmp/mat-tenant-network"
```

Omission disables export. MAT creates the directory if necessary; use a dedicated
writable directory for each MAT process. Before creating or restoring devices,
MAT removes existing `<machine-id>.json` snapshots from that directory; removal
errors fail startup. Each primary DPU with an active tenant
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
`curl`, and the compiled NICo DHCP server. IPv6 forwarding must already be enabled;
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
sudo -n python3 dev/machine-a-tron/ipv6/packet_test.py \
  --snapshot "$snapshot0" --snapshot "$snapshot1" \
  --dhcp-server "$dhcp_server" --output "$output"
```

Exactly two `--snapshot` arguments are required. All other required arguments
appear above. Default execution needs no Internet access. It temporarily creates
veths, addresses and connected routes, per-namespace resolver files, processes,
and scoped IPv6 forwarding rules. MTU is fixed at 1280; this does not validate the
configured production MTU. Normal completion, errors, Ctrl-C, and SIGTERM clean up
these resources. SIGKILL or VM failure cannot run cleanup. A local file lock
prevents concurrent runs using overlapping tenant address space.

`result.json` reports PASS or FAIL, lease details, routes, connectivity results,
and cleanup errors. DHCP, DNS, RA, and HTTP logs are written beside it. Preflight
errors occur before the output directory is created. Failures after setup starts
write a failing result and exit nonzero; cleanup errors also fail the run.

For optional Internet access, add an HTTPS target. If the lab requires NAT66,
explicitly supply its egress interface:

```bash
sudo -n python3 dev/machine-a-tron/ipv6/packet_test.py \
  --snapshot "$snapshot0" --snapshot "$snapshot1" \
  --dhcp-server "$dhcp_server" --output "$output" \
  --internet-url https://www.google.com --nat66-interface eth0
```

`--internet-url` defaults to absent and must be HTTPS. Curl disables proxies,
checks certificates, and fails on HTTP errors. `--nat66-interface` defaults to
absent and requires `--internet-url`; when provided, NAT66 is restricted to the
two endpoint `/128` addresses. Without it, the test adds no translation, so the
VM's routing must provide return reachability. NAT66 success does not prove that
a public tenant prefix is routed through the fabric.

The DNS fixtures answer `peer.mat.test` locally and forward other names to
`--dns-upstream`, a numeric IPv4 or IPv6 address on UDP port 53. Its default is
`127.0.0.53`, the VM's systemd-resolved stub. This default requires that resolver
for Internet-name lookups and does not prove IPv6-only dependencies. DHCPv6
advertises each simulated router's IPv6 linknet address as the endpoint's DNS
server; this is fixture configuration, not verification of site DNS settings.

## Regression checks

```bash
python3 -m unittest discover -s dev/machine-a-tron/ipv6 -v
cargo test --locked --profile ci-tests -p carbide-machine-a-tron --lib
```
