# Boot Designated Machines Without NICo DHCP, DNS or API

- **Issue:** [#7568](https://github.com/dsx-ai-factory/infra-controller/issues/7568)
- **Status:** Scoping

This document scopes the work. It proposes no code changes.

---

## 1. Summary

NICo must let a site admin designate selected machines as **boot independent**.
A boot-independent machine and its DPU must complete a full power-on cycle and
reach the network when `nico-api`, `nico-dhcp`, `nico-dns` and `unbound` are all
unavailable.

The main use case is a cold start. NICo manages the hosts and DPUs that run the
NICo control plane. When the cluster is fully down, these hosts cannot start,
because every network step depends on the control plane that they must host.

The designation must stay per machine. If all machines were boot independent,
a repair or a reallocation would frequently cause an IP conflict. A move of a
host to a different network would then need a manual reset at the machine.

Four dependencies block a cold start today:

| # | Dependency | Effect when the control plane is down |
|---|---|---|
| 1 | The host BMC and the DPU BMC get their address from DHCP. | The BMC has no address after a BMC reboot. The lease state does not help. |
| 2 | The DPU OOB interface gets its address from DHCP. | The DPU ARM OS has no management address. |
| 3 | The DPU agent fetches the HBN configuration from `nico-api` on every start. | The DPU comes up with no HBN configuration. The agent then erases the stored configuration. |
| 4 | Authoritative DNS and recursive DNS both run in the cluster. | No name resolution. The DPU agent fails at start, because it resolves `carbide-pxe.forge` before it runs. |

Items 1 to 3 need new persistent, machine-local configuration. Item 4 needs a
design decision, because NICo has no DNS data path that survives a control-plane
outage.

---

## 2. What the code does today

### 2.1 Address allocation is a database record, not a device configuration

NICo already has a per-interface IP allocation policy. See
[`ExpectedInterfaceIpAllocation`](https://github.com/dsx-ai-factory/infra-controller/blob/main/crates/api-model/src/expected_machine.rs#L322)
with the values `Dynamic`, `Fixed` and `Retained`, and the host-BMC compatibility
enum [`BmcIpAllocationType`](https://github.com/dsx-ai-factory/infra-controller/blob/main/crates/api-model/src/expected_machine.rs#L144).

`Fixed` and `Retained` only pin the address **in the NICo database**. The device
still learns the address through DHCP. The allocation decision runs in
[`ensure_dhcp_address_for_family`](https://github.com/dsx-ai-factory/infra-controller/blob/main/crates/api-core/src/dhcp/discover.rs#L258)
and in
[`discover_dhcp`](https://github.com/dsx-ai-factory/infra-controller/blob/main/crates/api-core/src/dhcp/discover.rs#L539).

Therefore a `Fixed` BMC with a stable address still cannot boot when `nico-dhcp`
or `nico-api` is down. This is the central gap. The new work must push the
address **into the device**.

### 2.2 The DHCP data path has no offline mode

`nico-dhcp` is a Kea hooks library. Every DISCOVER and REQUEST goes to `nico-api`
over gRPC. The hook cache in
[`crates/dhcp/src/cache.rs`](https://github.com/dsx-ai-factory/infra-controller/blob/main/crates/dhcp/src/cache.rs#L40)
holds negative answers only. The operator documentation states this explicitly in
[`docs/provisioning/ip-and-network-configuration.md`](https://github.com/dsx-ai-factory/infra-controller/blob/main/docs/provisioning/ip-and-network-configuration.md):
the cache is "a degraded-mode safety net, not a fallback pool".

The DPU-local DHCP server is different. It runs in DPU mode and answers from a
file on the DPU. See
[`crates/dhcp-server/src/modes/dpu.rs`](https://github.com/dsx-ai-factory/infra-controller/blob/main/crates/dhcp-server/src/modes/dpu.rs#L98)
and the `host.yaml` path in
[`crates/agent/src/dhcp.rs`](https://github.com/dsx-ai-factory/infra-controller/blob/main/crates/agent/src/dhcp.rs#L32).
This local path already works without `nico-api`, as long as the file stays on
disk. Work item W4 protects that file.

### 2.3 The DPU OOB address is hard-coded to DHCP

The iPXE command line for a DPU sets `ip=dhcp` and `bfnet=oob_net0:dhcp`. See
[`crates/api-core/src/ipxe.rs#L221`](https://github.com/dsx-ai-factory/infra-controller/blob/main/crates/api-core/src/ipxe.rs#L221).
The value is a literal in a `format!` call. It has no per-machine variant.

The DPU cloud-init payload is rendered in
[`render_user_data`](https://github.com/dsx-ai-factory/infra-controller/blob/main/crates/pxe/src/routes/cloud_init/dpu.rs#L94).
It carries the API URL, the PXE URL and the HBN bridge settings, but no OOB
address configuration.

### 2.4 The DPU agent erases HBN when it cannot reach the API

This is the behaviour that the issue calls out: "NICo specifically prevents this
behavior so stale nodes do not come back from the dead".

The agent fetches the network configuration in
[`PeriodicConfigFetcher`](https://github.com/dsx-ai-factory/infra-controller/blob/main/crates/agent/src/periodic_config_fetcher.rs#L41).
The field `netconf` is an in-memory `ArcSwapOption`. Nothing is written to disk.
A `NotFound` answer clears it at
[line 258](https://github.com/dsx-ai-factory/infra-controller/blob/main/crates/agent/src/periodic_config_fetcher.rs#L258).

The main loop reads that value. The `None` arm at
[`crates/agent/src/main_loop.rs#L1539`](https://github.com/dsx-ai-factory/infra-controller/blob/main/crates/agent/src/main_loop.rs#L1539)
waits one iteration and then calls
[`ethernet_virtualization::reset`](https://github.com/dsx-ai-factory/infra-controller/blob/main/crates/agent/src/ethernet_virtualization.rs#L1503).
That function writes blank DHCP files and deletes `var/support/nvue_startup.yaml`.

Two different conditions produce the same `None`:

- The API answered `NotFound`. The DPU was deleted. A reset is correct.
- The API was unreachable, and no earlier fetch succeeded in this process. After
  a cold-start reboot this is always true.

The second condition is the cold-start failure. The agent deletes a usable HBN
configuration while the control plane is down.

### 2.5 DNS has no path that survives a control-plane outage

`nico-dns` is authoritative only. It answers each query with a `lookup_record`
gRPC call to `nico-api`. See
[`crates/dns/src/lib.rs`](https://github.com/dsx-ai-factory/infra-controller/blob/main/crates/dns/src/lib.rs#L21)
and the handler
[`lookup_record`](https://github.com/dsx-ai-factory/infra-controller/blob/main/crates/api-core/src/handlers/dns.rs#L334).
It keeps a negative cache only, in
[`crates/dns/src/negative_cache.rs`](https://github.com/dsx-ai-factory/infra-controller/blob/main/crates/dns/src/negative_cache.rs).
It serves A, AAAA and PTR records. It does not recurse and it does not support
zone transfer.

Recursive DNS is a separate `unbound` deployment in the same cluster. See
[`helm/charts/unbound`](https://github.com/dsx-ai-factory/infra-controller/blob/main/helm/charts/unbound/values.yaml).

Both services therefore fail together with the cluster. This matches the issue
comment that both recursive and authoritative DNS are in scope.

DNS is also a hard start dependency for the DPU agent.
[`ServiceAddresses::build`](https://github.com/dsx-ai-factory/infra-controller/blob/main/crates/agent/src/util.rs#L172)
resolves `carbide-pxe.forge` and propagates the error. The agent does not start
if that name does not resolve.

### 2.6 NICo can already write BMC settings through Redfish

The Redfish client trait already contains a write method for BMC time settings:
[`set_ntp_servers`](https://github.com/dsx-ai-factory/infra-controller/blob/main/crates/redfish/src/libredfish/instrumented.rs#L396).
The pre-ingestion state machine calls it from
[`crates/preingestion-manager/src/lib.rs#L1793`](https://github.com/dsx-ai-factory/infra-controller/blob/main/crates/preingestion-manager/src/lib.rs#L1793).

NICo also reads the BMC `EthernetInterfaces` collection during exploration. See
[`crates/bmc-explorer/src/manager.rs#L72`](https://github.com/dsx-ai-factory/infra-controller/blob/main/crates/bmc-explorer/src/manager.rs#L72)
and
[`get_manager_ethernet_interface`](https://github.com/dsx-ai-factory/infra-controller/blob/main/crates/redfish/src/libredfish/instrumented.rs#L351).

No code writes `IPv4StaticAddresses`, `DHCPv4.DHCPEnabled` or `StaticNameServers`.
The read path and the state machine hook both exist, so the new write path can
reuse them.

---

## 3. Proposed work items

The items are ordered. W1 is a prerequisite for W2, W3 and W4.

### W1 — Add the boot-independent designation

Add a per-machine operator flag. The `ExpectedMachine` declaration is the correct
owner, because it is the existing per-machine, operator-supplied record that
already holds the IP allocation policy and the BMC credentials.

Scope:

- A new field on
  [`ExpectedMachineData`](https://github.com/dsx-ai-factory/infra-controller/blob/main/crates/api-model/src/expected_machine.rs#L968)
  and on the protobuf message in
  [`crates/rpc/proto/forge.proto`](https://github.com/dsx-ai-factory/infra-controller/blob/main/crates/rpc/proto/forge.proto).
- A nullable column, added by one forward migration in
  [`crates/api-db/migrations/`](https://github.com/dsx-ai-factory/infra-controller/blob/main/crates/api-db/migrations).
- Validation in
  [`crates/api-core/src/handlers/expected_machine.rs`](https://github.com/dsx-ai-factory/infra-controller/blob/main/crates/api-core/src/handlers/expected_machine.rs#L180).
  Reject the designation when an interface policy is `Dynamic`. A boot-independent
  machine needs a `Fixed` or `Retained` address for every management interface.
- New flags on `nico-admin-cli em add` and `em patch`, in
  [`crates/admin-cli/src/expected_machines/add/args.rs`](https://github.com/dsx-ai-factory/infra-controller/blob/main/crates/admin-cli/src/expected_machines/add/args.rs#L195)
  and
  [`patch/args.rs`](https://github.com/dsx-ai-factory/infra-controller/blob/main/crates/admin-cli/src/expected_machines/patch/args.rs#L234).
  Regenerate the CLI documentation.
- The REST contract in
  [`rest-api/openapi/spec.yaml`](https://github.com/dsx-ai-factory/infra-controller/blob/main/rest-api/openapi/spec.yaml).

Open question Q1 applies here: machine-level flag, or a fourth value in
`ExpectedInterfaceIpAllocation`.

### W2 — Write a static address into the host BMC and the DPU BMC

Push the address that NICo already reserved into the BMC, so that the BMC does
not need DHCP after a reset.

Scope:

- New Redfish write methods beside
  [`set_ntp_servers`](https://github.com/dsx-ai-factory/infra-controller/blob/main/crates/redfish/src/libredfish/instrumented.rs#L396):
  set `DHCPv4.DHCPEnabled = false`, set `IPv4StaticAddresses`, set
  `StaticNameServers`, and the matching IPv6 properties. Add a read-back method
  for verification, and a method that restores DHCP.
- A vendor compatibility layer in
  [`crates/bmc-vendor`](https://github.com/dsx-ai-factory/infra-controller/blob/main/crates/bmc-vendor).
  Redfish `EthernetInterface` support is not uniform across BMC vendors.
- A new state in the pre-ingestion state machine in
  [`crates/preingestion-manager/src/lib.rs`](https://github.com/dsx-ai-factory/infra-controller/blob/main/crates/preingestion-manager/src/lib.rs#L420),
  beside `SetNtpServers`. Use the same bounded retry shape.
- Reconciliation. The static value must follow a later address change. The
  address write path in
  [`crates/api-core/src/handlers/machine_interface_address.rs`](https://github.com/dsx-ai-factory/infra-controller/blob/main/crates/api-core/src/handlers/machine_interface_address.rs#L305)
  is the trigger point.
- Rollback. Decommissioning and factory reset must restore DHCP on the BMC,
  otherwise a repaired machine keeps a stale static address and causes the IP
  conflict that the issue warns about. See
  [`crates/machine-controller/src/handler/decommissioning.rs`](https://github.com/dsx-ai-factory/infra-controller/blob/main/crates/machine-controller/src/handler/decommissioning.rs)
  and
  [`factory_reset.rs`](https://github.com/dsx-ai-factory/infra-controller/blob/main/crates/machine-controller/src/handler/factory_reset.rs).

Risk: a wrong static write makes the BMC unreachable over the network. Recovery
then needs physical access. Verify the read-back before the change is committed,
and keep the operation behind the W1 designation.

### W3 — Give the DPU OOB interface a static address

Scope:

- Make the `bfnet=oob_net0:dhcp` literal at
  [`crates/api-core/src/ipxe.rs#L221`](https://github.com/dsx-ai-factory/infra-controller/blob/main/crates/api-core/src/ipxe.rs#L221)
  conditional. For a designated DPU, render the static BlueField kickstart form
  with the address, the mask and the gateway that NICo reserved.
- Add the same address to the DPU cloud-init context in
  [`render_user_data`](https://github.com/dsx-ai-factory/infra-controller/blob/main/crates/pxe/src/routes/cloud_init/dpu.rs#L94),
  and write a persistent interface configuration in the template
  [`pxe/templates/user-data`](https://github.com/dsx-ai-factory/infra-controller/blob/main/pxe/templates/user-data).
  The kickstart value only covers the installer. The installed DPU OS needs its
  own persistent configuration.
- Carry the address in the `DiscoveryInstructions` message, so the PXE service
  does not need a second API call.

### W4 — Keep the DPU HBN configuration across a control-plane outage

This item removes the behaviour described in section 2.4.

Scope:

- Persist the last successful `ManagedHostNetworkConfigResponse` to disk in
  [`crates/agent/src/periodic_config_fetcher.rs`](https://github.com/dsx-ai-factory/infra-controller/blob/main/crates/agent/src/periodic_config_fetcher.rs#L236),
  and load it at agent start when the first fetch fails. Today the value exists
  only in memory.
- Separate the two `None` conditions. Keep `NotFound` as a true "this DPU is
  gone" signal. Treat a transport error as "unknown", and do not clear the cache.
  The `None` arm at
  [`crates/agent/src/main_loop.rs#L1539`](https://github.com/dsx-ai-factory/infra-controller/blob/main/crates/agent/src/main_loop.rs#L1539)
  must only call
  [`reset`](https://github.com/dsx-ai-factory/infra-controller/blob/main/crates/agent/src/ethernet_virtualization.rs#L1503)
  for the first condition.
- For a designated DPU, never reset on an unreachable API, even after a long
  outage. Deliver the designation to the DPU in the cloud-init agent
  configuration written by
  [`generate_forge_agent_config`](https://github.com/dsx-ai-factory/infra-controller/blob/main/crates/pxe/src/routes/cloud_init/dpu.rs#L77),
  so the agent can apply the rule before it reaches the API.
- Confirm that the HBN configuration file `var/support/nvue_startup.yaml` and the
  DHCP file `var/support/forge-dhcp/conf/host.yaml` are on persistent DPU storage.
  If they are not, the persistence from the first bullet must cover them.

Safety note: the existing behaviour prevents a stale node from rejoining the
network. The new behaviour must keep that protection for every machine that is
not designated. Keep the retention bounded, and record the age of the applied
configuration in the DPU health report.

### W5 — Make DNS answerable during a control-plane outage

This item needs a design decision first. Three candidate approaches:

1. **Static resolver configuration for designated machines.** Write the resolver
   addresses into the BMC with `StaticNameServers` (part of W2) and into the DPU
   OS (part of W3). Point them at an off-cluster resolver. This is the smallest
   change, but it moves the dependency to site infrastructure.
2. **A positive cache in `nico-dns`.** Extend
   [`crates/dns/src/negative_cache.rs`](https://github.com/dsx-ai-factory/infra-controller/blob/main/crates/dns/src/negative_cache.rs)
   to a persistent snapshot of the zone, and serve it when `nico-api` is
   unreachable. This keeps the answers inside NICo, but `nico-dns` still runs in
   the cluster that is down.
3. **A secondary off-cluster authoritative server.** `nico-dns` has no zone
   transfer support today. This is the largest change.

Approach 1 is the recommended first step, because it removes the start
dependency described in section 2.5 and needs no new DNS data path. Approaches 2
and 3 should be tracked separately.

Independently of the choice, make the DPU agent start without DNS. Today
[`ServiceAddresses::build`](https://github.com/dsx-ai-factory/infra-controller/blob/main/crates/agent/src/util.rs#L172)
fails when `carbide-pxe.forge` does not resolve. The PXE address is not needed
to keep HBN running. Degrade instead of failing, as the NTP branch in the same
function already does.

### W6 — Optional: a local reservation file for `nico-dhcp`

If a designated machine still uses DHCP for one interface, the Kea hook cannot
answer while `nico-api` is down. A persistent reservation file for designated
machines only, read by
[`crates/dhcp/src/lib.rs`](https://github.com/dsx-ai-factory/infra-controller/blob/main/crates/dhcp/src/lib.rs),
would close that gap.

This item has lower priority. W2 and W3 remove the need for DHCP on the
designated machines. Do this item only if a device cannot accept a static
configuration.

### W7 — Guard rails, observability and documentation

- Metrics: a gauge for the number of designated machines, and a counter for
  failed static pushes. Follow the `Event` rules in
  [`AGENTS.md`](https://github.com/dsx-ai-factory/infra-controller/blob/main/AGENTS.md).
- Health: report the designation and the age of the applied DPU configuration in
  the DPU health report.
- Documentation: a new section in
  [`docs/provisioning/ip-and-network-configuration.md`](https://github.com/dsx-ai-factory/infra-controller/blob/main/docs/provisioning/ip-and-network-configuration.md),
  an update to
  [`docs/provisioning/expected-machine-interfaces.md`](https://github.com/dsx-ai-factory/infra-controller/blob/main/docs/provisioning/expected-machine-interfaces.md),
  and a cold-start procedure. Record the IP-conflict risk and the manual steps
  that a network move needs.

---

## 4. Files that probably change

| Area | File | Change |
|---|---|---|
| Model | [`crates/api-model/src/expected_machine.rs`](https://github.com/dsx-ai-factory/infra-controller/blob/main/crates/api-model/src/expected_machine.rs#L322) | Designation field; validation against the allocation policy |
| Protobuf | [`crates/rpc/proto/forge.proto`](https://github.com/dsx-ai-factory/infra-controller/blob/main/crates/rpc/proto/forge.proto) | Designation field; OOB address in `DiscoveryInstructions` |
| Database | [`crates/api-db/migrations/`](https://github.com/dsx-ai-factory/infra-controller/blob/main/crates/api-db/migrations) | One nullable column |
| API handler | [`crates/api-core/src/handlers/expected_machine.rs`](https://github.com/dsx-ai-factory/infra-controller/blob/main/crates/api-core/src/handlers/expected_machine.rs#L180) | Accept and validate the designation |
| iPXE | [`crates/api-core/src/ipxe.rs`](https://github.com/dsx-ai-factory/infra-controller/blob/main/crates/api-core/src/ipxe.rs#L221) | Static `bfnet` form for a designated DPU |
| PXE cloud-init | [`crates/pxe/src/routes/cloud_init/dpu.rs`](https://github.com/dsx-ai-factory/infra-controller/blob/main/crates/pxe/src/routes/cloud_init/dpu.rs#L94) | OOB address and designation in the template context |
| PXE template | [`pxe/templates/user-data`](https://github.com/dsx-ai-factory/infra-controller/blob/main/pxe/templates/user-data) | Persistent OOB interface configuration |
| Redfish | [`crates/redfish/src/libredfish/instrumented.rs`](https://github.com/dsx-ai-factory/infra-controller/blob/main/crates/redfish/src/libredfish/instrumented.rs#L396) | Static address, resolver and DHCP-disable write methods |
| Redfish | [`crates/redfish/src/libredfish/mod.rs`](https://github.com/dsx-ai-factory/infra-controller/blob/main/crates/redfish/src/libredfish/mod.rs#L140) | Trait additions |
| BMC vendors | [`crates/bmc-vendor`](https://github.com/dsx-ai-factory/infra-controller/blob/main/crates/bmc-vendor) | Per-vendor property differences |
| Pre-ingestion | [`crates/preingestion-manager/src/lib.rs`](https://github.com/dsx-ai-factory/infra-controller/blob/main/crates/preingestion-manager/src/lib.rs#L420) | New state that writes the static BMC configuration |
| Address lifecycle | [`crates/api-core/src/handlers/machine_interface_address.rs`](https://github.com/dsx-ai-factory/infra-controller/blob/main/crates/api-core/src/handlers/machine_interface_address.rs#L305) | Re-push on an address change |
| Teardown | [`crates/machine-controller/src/handler/decommissioning.rs`](https://github.com/dsx-ai-factory/infra-controller/blob/main/crates/machine-controller/src/handler/decommissioning.rs) | Restore DHCP on the BMC |
| DPU agent | [`crates/agent/src/periodic_config_fetcher.rs`](https://github.com/dsx-ai-factory/infra-controller/blob/main/crates/agent/src/periodic_config_fetcher.rs#L41) | Persist and reload the last good configuration |
| DPU agent | [`crates/agent/src/main_loop.rs`](https://github.com/dsx-ai-factory/infra-controller/blob/main/crates/agent/src/main_loop.rs#L1539) | Do not reset on an unreachable API |
| DPU agent | [`crates/agent/src/ethernet_virtualization.rs`](https://github.com/dsx-ai-factory/infra-controller/blob/main/crates/agent/src/ethernet_virtualization.rs#L1503) | Guard the reset behind the new condition |
| DPU agent | [`crates/agent/src/util.rs`](https://github.com/dsx-ai-factory/infra-controller/blob/main/crates/agent/src/util.rs#L172) | Start without DNS |
| DNS | [`crates/dns/src/negative_cache.rs`](https://github.com/dsx-ai-factory/infra-controller/blob/main/crates/dns/src/negative_cache.rs) | Optional positive cache, after the W5 decision |
| CLI | [`crates/admin-cli/src/expected_machines/`](https://github.com/dsx-ai-factory/infra-controller/blob/main/crates/admin-cli/src/expected_machines) | New flags; regenerated documentation |
| REST | [`rest-api/openapi/spec.yaml`](https://github.com/dsx-ai-factory/infra-controller/blob/main/rest-api/openapi/spec.yaml) | New attribute; regenerated SDK |
| Documentation | [`docs/provisioning/ip-and-network-configuration.md`](https://github.com/dsx-ai-factory/infra-controller/blob/main/docs/provisioning/ip-and-network-configuration.md) | Designation, limits and cold-start procedure |

---

## 5. Open questions

| # | Question | Why it matters |
|---|---|---|
| Q1 | Is the designation a machine-level flag, or a fourth `ExpectedInterfaceIpAllocation` value? | A machine-level flag matches the words of the issue. A policy value reuses the existing per-interface validation. |
| Q2 | Which W5 approach is selected? | Approach 1 is small and uses site infrastructure. Approaches 2 and 3 keep the answers inside NICo and are much larger. |
| Q3 | Does the DPU keep `nvue_startup.yaml` across a reboot on every supported platform? | W4 is much smaller if the file is already persistent. |
| Q4 | Which BMC vendors must support the static Redfish write at first delivery? | Redfish `EthernetInterface` write support is not uniform. The first delivery can be limited to the control-plane hardware. |
| Q5 | How long may a DPU keep a stale HBN configuration when the API is unreachable? | An unbounded retention weakens the protection against a stale node. A short limit breaks a long cold start. |
| Q6 | Must a designated machine still accept a DHCP answer when the control plane returns? | This decides whether W6 is needed and how a network move is performed. |
| Q7 | Does the host OS of a designated machine also need a static address? | The issue names the BMC and the DPU. A site-controller host usually gets its address from the parent datacenter. |

---

## Appendix A — Cold-start dependency chain

```text
      power on
          |
          v
  +---------------+        DHCP         +-----------+       gRPC      +----------+
  |   Host BMC    |-------------------->| nico-dhcp |---------------->| nico-api |
  +---------------+                     +-----------+                 +----------+
          |                                   ^                            |
          | power on host                     |                            | SQL
          v                                   |                            v
  +---------------+        DHCP               |                      +----------+
  |    DPU BMC    |---------------------------+                      | Postgres |
  +---------------+                           |                      +----------+
          |                                   |                            ^
          v                                   |                            |
  +---------------+        DHCP               |                            |
  | DPU OOB (ARM) |---------------------------+                            |
  +---------------+                                                        |
          |                                                                |
          | agent start                                                    |
          v                                                                |
  +---------------+   resolve carbide-pxe.forge   +---------+   gRPC       |
  |   DPU agent   |------------------------------>| unbound |              |
  +---------------+                               +---------+              |
          |                                            |                   |
          | get_periodic_dpu_config (gRPC)             | forward           |
          v                                            v                   |
  +---------------+                              +-----------+             |
  |      HBN      |<--- NVUE config ---          | nico-dns  |-------------+
  +---------------+                              +-----------+
          |
          v
   host network up

  Everything right of the first column runs inside the cluster that the
  host in the first column must start. This is the cold-start deadlock.
```

## Appendix B — DPU agent configuration flow, today and proposed

```text
TODAY
                           +-------------------------------+
   agent start ----------->| PeriodicConfigFetcher::new    |
                           | initial synchronous fetch     |
                           +-------------------------------+
                                     |            |
                       fetch ok      |            |   fetch failed, or NotFound
                                     v            v
                           netconf = Some(cfg)   netconf = None   (memory only)
                                     |            |
                                     v            v
                           +---------------+   +-------------------------+
                           | apply HBN,    |   | iteration 1: seen_blank |
                           | DHCP, FMDS    |   | iteration 2: reset()    |
                           +---------------+   | - blank DHCP files      |
                                               | - delete nvue_startup   |
                                               +-------------------------+
                                                        |
                                                        v
                                             host network stays down

PROPOSED (W4)
                           +-------------------------------+
   agent start ----------->| load persisted last-good cfg  |
                           +-------------------------------+
                                     |
                                     v
                           +-------------------------------+
                           | fetch from nico-api           |
                           +-------------------------------+
                              |          |            |
                    fetch ok  |  NotFound|  unreachable
                              v          v            v
                     persist + apply   reset()   keep last-good cfg,
                                       (unchanged) apply it, report its age
                                                        |
                                                        v
                                              host network comes up
```
