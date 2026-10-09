# MAT dual-stack packet test

This opt-in test joins machine-a-tron's (MAT's) control API tenant observations to
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

## Read active tenant configurations

The read-only MAT control route `GET /machines/tenant-networks` returns a JSON
array of cached tenant network observations, sorted by DPU ID. It uses the existing
control server's address and TLS configuration and requires no additional MAT or
Helm setting. Reading this route does not fetch new configuration from NICo or
change simulator state. An empty array means no primary DPU has a cached tenant
configuration.

MAT refreshes each observation after a successful network-configuration fetch.
Only primary DPUs with an instance ID, tenant interfaces, and the admin network
disabled are included. MAT clears the observation when it observes a return to
the admin network, loses the machine configuration, detaches the NIC, powers off
the DPU, or stops its actor. A new MAT process starts without cached observations.
A failed configuration fetch can leave the previous observation visible; consumers
must check `updated_at` for freshness.

Each observation has schema version `1`. It records `updated_at` (UTC), DPU and
instance IDs, the host interface ID (nullable), managed-host and instance-network
configuration versions, virtualization type (nullable numeric protobuf enum), and
tenant interfaces. Interfaces include canonical family-tagged `addresses`, numeric
function type, L2 status, VPC VNI, and whether an NSG is present. Deprecated address
fields, credentials, and tenant user data are excluded.

The packet test selects exactly one observation for each requested instance ID.
It rejects unknown schema versions and observations older than 120 seconds or
dated in the future. It fetches the API again at the end and checks freshness and
configuration identity; an unavailable API or a changed or missing observation
fails the run. These two checks do not prove continuous availability between them.

## Run on an isolated Linux VM

The VM needs Python 3.11 or newer (for RFC 3339 UTC timestamp parsing), `ip`, `sysctl`, `ip6tables`, `unshare`, `mount`, `ping`,
`iptables`, `curl`, and the compiled NICo DHCP server. Both `net.ipv4.ip_forward`
and `net.ipv6.conf.all.forwarding` must already be `1`;
the test does not change global forwarding settings. Run from the repository root.
Build the DHCP server from the revision being tested:

```bash
cargo build --locked --profile ci-tests -p carbide-dhcp-server --bin forge-dhcp-server
```

Set `mat_url` to the HTTP(S) base URL of the reachable MAT control server and
`instance0` and `instance1` to the UUIDs of the two instances. If MAT runs in a
cluster, forward its control service port to the VM before running the test.
Set `dhcp_server` to the absolute path of the resulting
`target/ci-tests/forge-dhcp-server` binary, accounting for `CARGO_TARGET_DIR` if set.
Choose a new absolute `output` directory that does not yet exist:

```bash
sudo -n python3 dev/machine-a-tron/network/packet_test.py \
  --mat-url "$mat_url" --instance-id "$instance0" --instance-id "$instance1" \
  --dhcp-server "$dhcp_server" --output "$output"
```

Exactly two distinct `--instance-id` arguments are required. All other required
arguments appear above. `--mat-url` accepts HTTP or HTTPS and must not contain
credentials, a query, or a fragment. Requests bypass environment proxy settings
and use a ten-second timeout. HTTPS uses system trust by default; `--mat-ca`
selects a PEM CA bundle. For a self-signed test endpoint, `--mat-insecure`
explicitly disables certificate verification. These TLS flags are mutually
exclusive and require HTTPS.

Default execution needs no Internet access. During each run it creates
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
  --mat-url "$mat_url" --instance-id "$instance0" --instance-id "$instance1" \
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

## Create, release, and recreate instances

`lifecycle_test.py` runs the packet test twice around a real Core instance
lifecycle. It creates an FNN VPC and one prefix per family, allocates two instances,
waits for Ready, synchronized network configuration and fresh MAT observations,
and exercises the DHCP, peer, Internet and route-recovery checks above. It then
releases both instances, waits for Core and MAT to forget them and for prefix
capacity to recover, and allocates new instance IDs on the same hosts while
explicitly requesting the previous IPv4 and IPv6 addresses. Both generations must
pass. Finally it releases the instances, waits for both prefixes to drain and be
removed, and deletes the VPC.

Use two unallocated MAT host IDs, an existing tenant with an FNN routing profile,
and unused site-admitted IPv4 and IPv6 prefixes. The script verifies that both
hosts belong to the selected MAT server and refuses hosts that already have an
instance. The simulated OS configuration is an iPXE exit script with phone-home
disabled; no tenant OS boots. Site prefixes, tenant configuration, simulated host
creation and lab gateway configuration remain operator prerequisites.

Provide `--rpc-command` as an absolute executable path to a locally configured
authentication wrapper. It receives the Forge gRPC method name as its only
argument and a ProtoJSON request on standard input. It must write only the
ProtoJSON response to standard output, send diagnostics to standard error and
exit nonzero on failure. Requests and responses use protobuf JSON field names
and enum names, not admin CLI JSON. A `grpcurl` wrapper can use `-emit-defaults`,
`-d @`, and the method `forge.Forge/$1` with the VM's configured TLS endpoint,
CA, client certificate and key. Keep credentials outside the repository. Each
wrapper invocation has a 60-second timeout.

Set the variables below to the wrapper, the two host IDs, tenant, prefixes,
MAT endpoint, compiled DHCP server, and a new absolute evidence directory:

```bash
sudo -n python3 dev/machine-a-tron/network/lifecycle_test.py \
  --rpc-command "$rpc_command" \
  --machine-id "$machine0" --machine-id "$machine1" \
  --tenant-org "$tenant_org" \
  --ipv4-prefix "$ipv4_prefix" --ipv6-prefix "$ipv6_prefix" \
  --mat-url "$mat_url" --dhcp-server "$dhcp_server" \
  --output "$output" --internet-url https://www.google.com
```

All arguments shown are required; exactly two distinct `--machine-id` values
are accepted. CIDRs must be canonical networks with at least two linknets
(IPv4 `/30` or wider and IPv6 `/64` through `/126`). Wider IPv6 prefixes are
excluded because their saturated capacity counters cannot prove full reclamation;
choose a supported subnet of the larger site prefix. `--timeout` is a finite positive
number of seconds for each reconciliation or cleanup stage, default `1200`.
Each packet-test subprocess is limited to 300 seconds. `--mat-ca`,
`--mat-insecure`, `--dns-upstream`, `--nat44-interface` and `--nat66-interface`
have the same contracts as the packet test. Internet access is required for the
lifecycle test, and both families must succeed.

To exercise tenant prefixes through a lab gateway, omit both VM NAT flags.
Configure the gateway to admit and translate the chosen source prefixes and
route return traffic through the VM. The VM forwards packets between the temporary
endpoint links and the gateway; NAT then occurs at the gateway. Admission alone
does not provide the return route. Restore temporary gateway changes after testing.
This demonstrates the software path for those prefixes, not production fabric
routing or hardware offload.

The driver writes its generated resource IDs before mutations and labels each
resource with `mat-network-lifecycle=<run UUID>`. Cleanup only deletes resources
with that ownership label. A lost allocation reply still triggers a lookup of
the recorded instance IDs. Cleanup waits for soft-deleted prefix rows to disappear
before deleting their parent VPC. A failure stops dependent cleanup and records
remaining IDs and errors in `result.json`; use those IDs for manual recovery.
Normal errors, Ctrl-C and SIGTERM attempt cleanup. SIGKILL or VM failure cannot
run it. Do not run multiple lifecycle tests against the same hosts concurrently.

The top-level `result.json` records both generations' IDs, addresses, packet result
and release/capacity checks. Each generation has its own packet-test evidence
subdirectory and log. PASS requires successful cleanup. Existing instances,
VPCs, site prefixes and the tenant are preserved.

## Regression checks

```bash
python3 -m unittest discover -s dev/machine-a-tron/network -v
cargo test --locked --profile ci-tests -p carbide-machine-a-tron --lib
```
