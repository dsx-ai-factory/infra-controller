# Configure Expected Machine Interfaces <Badge intent="info">v2.1</Badge> <Badge intent="launch" minimal>New</Badge>

Expected Machine interface declarations tell NICo how to allocate addresses for
host, DPU OS, DPU BMC, and host BMC interfaces during ingestion. Use the
`interfaces` array in an Expected Machine manifest to identify each interface
by MAC address, assign its role, and select an IP allocation policy.

Configure the network segments that contain these addresses before you upload
the Expected Machine manifest. See [IP and Network Configuration](ip-and-network-configuration.md)
for site network configuration and DHCP relay requirements.

## Interface Fields

Each `interfaces` entry supports these fields:

| Field | Required | Description |
|---|---|---|
| `mac_address` | Yes | MAC address that identifies the interface. |
| `role` | No | Interface role: `host`, `dpu_os`, `dpu_bmc`, or `host_bmc`. The default is `host`. |
| `ip_allocation` | No | Address policy: `dynamic`, `fixed`, or `retained`. NICo can also infer the policy as described in [IP Allocation Policies](#ip-allocation-policies). |
| `fixed_ip` | For `fixed` | Address that NICo reserves for this interface. |
| `network_segment_type` | No | Optional segment-type guard. Admin CLI manifest and RPC JSON values are Tenant `0`, Admin `1`, Underlay `2`, and HostInband `3`. The corresponding model names are `tenant`, `admin`, `underlay`, and `host_inband`. |
| `primary` | No | Marks a `host` interface as the host boot interface. At most one `host` interface can set this to `true`. |
| `nic_type` | No | Legacy segment hint. Use `network_segment_type` for new configurations. |
| `fixed_mask`, `fixed_gateway` | No | Compatibility metadata. These fields do not select the managed segment or allocate the address. |

## Interface Roles

All roles support all three IP allocation policies.

| Role | Interface | Primary behavior | Default policy |
|---|---|---|---|
| `host` | Host OS interface | Can be the one explicitly configured primary host interface. | `dynamic` |
| `dpu_os` | DPU ARM OS interface | Primary for its DPU. | `dynamic` |
| `dpu_bmc` | DPU BMC interface | Never primary. | `dynamic` |
| `host_bmc` | Host BMC interface | Never primary. Its MAC must match the top-level `bmc_mac_address`. | `retained` |

An Expected Machine can contain multiple `host`, `dpu_os`, and `dpu_bmc`
entries, but it can contain only one `host_bmc` entry. If you repeat a MAC
address to reserve separate IPv4 and IPv6 addresses, every entry for that MAC
must use the same role. The one-entry limit means `host_bmc` cannot use this
duplicate-entry form.

The interface entry whose `mac_address` matches the top-level
`bmc_mac_address` must use the `host_bmc` role.

For host boot selection and DPU management policy, see
[Boot Interfaces and DPU Policies](boot-interfaces-and-dpu-modes.md).

## IP Allocation Policies

NICo uses the current Expected Machine configuration for each family's first
successful DHCP or Static allocation. IPv4 and IPv6 initialize independently.
The allocation behavior below requires the
[ExpectedMachine allocation implementation](https://github.com/dsx-ai-factory/infra-controller/pull/6012).

Choose one policy for each declared interface:

| Policy | Behavior |
|---|---|
| `dynamic` | NICo allocates an address from the segment selected by the DHCP relay or DHCPv6 link address. |
| `fixed` | NICo reserves `fixed_ip` as Static before serving DHCP for that address family. An explicit Fixed policy requires the address to belong to a configured managed prefix. |
| `retained` | NICo selects an address through DHCP and immediately stores it as Static. It remains reserved until explicit removal or deletion of the machine-interface record. |

`dynamic` and `retained` entries cannot include `fixed_ip`. A `fixed` entry
must include it.

NICo uses these defaults when `ip_allocation` is omitted:

| Configuration | Inferred policy |
|---|---|
| `fixed_ip` is present | `fixed` |
| `role` is `host_bmc` and `fixed_ip` is absent | `retained` |
| Any other role without `fixed_ip` | `dynamic` |

The `fixed_ip` inference preserves manifests created before explicit allocation
policies were available.

For repeated MAC entries, DHCP prefers a Fixed declaration for the requested
family, then an addressless Dynamic or Retained declaration. Equally applicable
declarations keep their list order. A Fixed declaration for only the other
family supplies interface metadata, but the requested family uses ordinary
DHCP; it does not inherit a Fixed reservation or retention.

### Address Allocation Lifetime

After a family has a DHCP or Static allocation, Expected Machine edits leave
its address and allocation type unchanged, even before machine association.
The first allocation of the other family still uses the current configuration.

Creating or associating an interface without allocating an address does not
select its allocation policy. A failed allocation also leaves the family free
to use corrected configuration on its next attempt.

An inferred SLAAC address does not prevent a first DHCP or Static allocation,
which can replace it. DHCPv6 `INFO_REQUEST` observation alone does not select
an allocation policy. However, NICo can create a Fixed reservation during that
request. The reservation counts as the family's first Static allocation even
though the response contains no address.

After lease expiry or explicit Static-address removal, NICo uses ordinary
DHCP for subsequent pool allocation while the machine-interface record remains.
It does not reapply Fixed or Retained configuration. Reserved segments still
reject clients without reservations.

A retained address is not written back to the Expected Machine configuration.
Deleting the interface record removes its addresses and per-family allocation
history. A recreated interface uses the current configuration, so later
ingestion can select a different Retained address.

Existing DHCP allocations keep their stored type during upgrade, including
allocations awaiting conversion to Static under the earlier Retained behavior.
Removal history begins when the updated address-removal code runs. NICo cannot
reconstruct earlier removals. If older code removed a family's allocation,
NICo can treat its next allocation as the first and apply the current
Expected Machine configuration.

To change an existing allocation, update the Expected Machine configuration
first, then explicitly assign or remove the address. The `assign-address`
command can replace a DHCP, SLAAC, or Static address. The `remove-address`
command removes a Static address; it does not cause NICo to reapply the
Expected Machine allocation policy:

```bash
# Find the interface ID and its current addresses.
nico-admin-cli -a <api-url> machine-interfaces show
nico-admin-cli -a <api-url> machine-interfaces show-addresses <interface-id>

# Replace one Fixed address with another.
nico-admin-cli -a <api-url> machine-interfaces assign-address \
  <interface-id> <new-fixed-ip>

# Remove a Static address before returning the interface to Dynamic allocation.
nico-admin-cli -a <api-url> machine-interfaces remove-address \
  <interface-id> <current-static-ip>
```

Delete an unassociated preallocated interface with
`machine-interfaces delete <interface-id>` when the complete row must be
recreated during ingestion. See the
[machine interface command reference](../manuals/nico-admin-cli/commands/machine-interfaces/machine-interfaces.md)
for details.

Use whole-machine force deletion only when the machine requires complete
reingestion and the targeted commands cannot restore the configured state.
Force deletion removes the managed host and can remove all data and BMC
interface records:

```bash
nico-admin-cli -a <api-url> machine force-delete \
  --machine <machine-id> \
  --delete-interfaces \
  --delete-bmc-interfaces
```

Confirm that the machine has no tenant instance and follow the
[Force Delete Playbook](../playbooks/force_delete.md).

## Network Segment Selection

NICo selects a segment before it applies the optional
`network_segment_type` guard:

- For `dynamic` and `retained`, the DHCPv4 relay address or DHCPv6 link address
  selects the configured segment.
- For `fixed`, the managed prefix that contains `fixed_ip` selects the segment.

Except for the legacy `host` case described below, NICo rejects a request when
the selected segment has a different `network_segment_type`. The role does not
imply a segment type. For example, a `dpu_bmc` interface can request an
`underlay` guard, but the guard is still checked against the segment selected
from the DHCP request or fixed address.

A legacy `host` declaration that omits `ip_allocation` uses
`network_segment_type` only to narrow DHCP segment selection. It does not use
the field as a Fixed-address guard.

For new `fixed` declarations, put `fixed_ip` inside a configured managed
prefix. Older `host` declarations that omit `ip_allocation` retain the
`static-assignments` fallback. An inferred Fixed `host_bmc` declaration without
a segment guard can also use that fallback. Set `ip_allocation` explicitly for
new configurations.

### Allow Only Reserved Addresses

Set a network segment's `allocation_strategy` to `reserved` when that segment
must serve only addresses reserved in advance:

```toml
[networks.oob]
type = "underlay"
prefix = "198.51.100.0/24"
gateway = "198.51.100.1"
mtu = 1500
reserve_first = 5
allocation_strategy = "reserved"
```

On a reserved segment, configure a `fixed` Expected Machine reservation before
the interface sends DHCP. `dynamic` and `retained` declarations cannot acquire
their first address on a reserved segment because NICo does not create an
address when no reservation exists. The default segment allocation strategy is
`dynamic`.

## Configure All Interface Roles

The following `expected_machines.json` file uses the admin CLI manifest format.
The fixed DPU OS address assumes `192.0.2.0/24` is a configured Admin prefix.
This format uses protobuf enum numbers for `network_segment_type`: Tenant `0`,
Admin `1`, Underlay `2`, and HostInband `3`.

```json
{
  "expected_machines": [
    {
      "bmc_mac_address": "02:00:00:00:00:01",
      "bmc_username": "root",
      "bmc_password": "<bmc-password>",
      "chassis_serial_number": "SERIAL-1",
      "interfaces": [
        {
          "mac_address": "02:00:00:00:00:10",
          "role": "host",
          "ip_allocation": "dynamic",
          "network_segment_type": 3,
          "primary": true
        },
        {
          "mac_address": "02:00:00:00:00:20",
          "role": "dpu_os",
          "ip_allocation": "fixed",
          "fixed_ip": "192.0.2.20",
          "network_segment_type": 1
        },
        {
          "mac_address": "02:00:00:00:00:21",
          "role": "dpu_bmc",
          "ip_allocation": "retained",
          "network_segment_type": 2
        },
        {
          "mac_address": "02:00:00:00:00:01",
          "role": "host_bmc",
          "ip_allocation": "retained",
          "network_segment_type": 2
        }
      ]
    }
  ]
}
```

Apply the complete table:

```bash
nico-admin-cli -a <api-url> em replace-all --filename expected_machines.json
```

The inline `--interfaces` option uses the same generated RPC JSON
representation. The `role` and `ip_allocation` fields accept the names shown
above, and `network_segment_type` uses the same enum numbers.

> **Security:** Values passed to `--bmc-password` can appear in shell history
> and process listings. Substitute credentials only in a protected
> administrative environment and follow your site's secret-handling policy.

```bash
nico-admin-cli -a <api-url> em add \
  --bmc-mac-address 02:00:00:00:00:01 \
  --bmc-username root \
  --bmc-password '<bmc-password>' \
  --chassis-serial-number SERIAL-1 \
  --interfaces '[{"mac_address":"02:00:00:00:00:20","role":"dpu_os","ip_allocation":"fixed","fixed_ip":"192.0.2.20","network_segment_type":1}]'
```

## Configure Host BMC Allocation

Keep the host BMC identity and credentials at the top level:

- `bmc_mac_address`
- `bmc_username`
- `bmc_password`
- `bmc_retain_credentials`

Use the matching `host_bmc` interface entry for its network allocation policy
and optional segment guard. Existing top-level `bmc_ip_address` and
`bmc_ip_allocation` fields remain supported. When both forms are present,
explicit top-level BMC address and allocation values override the nested
`host_bmc` values.

The top-level Auto compatibility policy selects Fixed when `bmc_ip_address` is
present and Retained otherwise.

In an admin CLI JSON file, `bmc_ip_allocation` uses `Auto`, `Dynamic`, `Fixed`,
or `Retained`. The `--bmc-ip-allocation` command-line option uses lowercase
values.

## Update and Clear Interfaces

`em patch --interfaces` replaces the complete interface list for that Expected
Machine. An empty array clears the list. Omitting the option preserves the
stored list.

```bash
# Replace the complete list.
nico-admin-cli -a <api-url> em patch \
  --bmc-mac-address 02:00:00:00:00:01 \
  --interfaces '[{"mac_address":"02:00:00:00:00:10","role":"host","ip_allocation":"dynamic","primary":true}]'

# Clear the list.
nico-admin-cli -a <api-url> em patch \
  --bmc-mac-address 02:00:00:00:00:01 \
  --interfaces '[]'
```

`em update --filename` preserves the stored list when `interfaces` is omitted
or `null`, and clears it when `interfaces` is `[]`. `em replace-all` has no
stored list to preserve, so omitted or `null` results in an empty list.

Within a replacement list, omitting `role` for a matching stored MAC preserves
its role. Set `role` to `unspecified` to reset it to `host`. Omitting
`ip_allocation` preserves the stored policy when the presence of `fixed_ip`
does not change. Set `ip_allocation` to `unspecified` to infer the policy
again. Omitting `network_segment_type` clears the stored segment guard.

Clearing the nested interface list does not clear top-level Host BMC address or
allocation fields.

## Backward Compatibility

Existing configurations do not need conversion:

- `host_nics` remains an input alias for `interfaces`.
- `--host_nics` remains a CLI alias for `--interfaces`.
- An entry without `role` remains a `host` interface.
- An entry with `fixed_ip` and no `ip_allocation` remains a fixed reservation.
- Top-level Host BMC allocation fields remain supported.

Use either `interfaces` or `host_nics` within one Expected Machine object, not
both. A manifest can use the old spelling for some machines and the new
spelling for others.
