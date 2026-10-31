# nvidia.infra_controller.expected_switch – manage Expected Switch

Create, update, and delete Expected Switches.

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `bmc_ip_address` | `str` | No | Optional BMC IP address (IPv4 or IPv6). When set, pre-allocates a reserved IP for the BMC. It must not be unspecified, multicast, or IPv4 limited broadcast. |
| `bmc_mac_address` | `str` | No | MAC address of the Expected Switch's BMC (Baseboard Management Controller). Required when creating a new resource. |
| `default_bmc_password` | `str` | No | Password for accessing the Expected Switch's BMC (up to 255 characters; the BMC's password policy still applies) |
| `default_bmc_username` | `str` | No | Username for accessing the Expected Switch's BMC (no API length limit; the BMC's username policy still applies) |
| `description` | `str` | No | Description of this component |
| `host_id` | `int` | No | Host ID within the tray |
| `id` | `str` | No | ID of the resource. Used for lookup. |
| `labels` | `dict` | No | User-defined key-value pairs for organizing and categorizing Expected Switches |
| `manufacturer` | `str` | No | Manufacturer of this component |
| `model` | `str` | No | Model of this component |
| `name` | `str` | No | Display name for this component |
| `nv_os_password` | `str` | No | NvOS password for the Expected Switch |
| `nv_os_username` | `str` | No | NvOS username for the Expected Switch |
| `nvos_interfaces` | `list` | No | Required NVOS ethernet ports of the Expected Switch. Mark one entry primary to choose eth0; when none is marked, the first entry on a multi-port switch defaults to primary and a single-port switch is always its own primary. The deprecated `nvosMacAddresses` field is ignored even when both fields are supplied. Required when creating a new resource. |
| `nvos_mac_addresses` | `list` | No | Deprecated and ignored. This field never configures NVOS ports or primary selection, regardless of its contents. Use `nvosInterfaces`, including in replace-all entries. Supplying both fields is allowed; only `nvosInterfaces` takes effect. |
| `rack_id` | `str` | No | Optional rack identifier for this component |
| `site_id` | `str` | No | ID of the site the Expected Switch belongs to. Required when creating a new resource. |
| `slot_id` | `int` | No | Slot ID within the rack |
| `state` | `str` | No | Desired state of the resource. Choices: `present`, `absent`. |
| `switch_serial_number` | `str` | No | Serial number of the Expected Switch. Required when creating a new resource. |
| `tray_idx` | `int` | No | Tray index within the rack |
| `wait` | `bool` | No | Wait for the resource to reach the desired state. |
| `wait_timeout` | `int` | No | Timeout in seconds for wait operations. |

## Examples

```yaml
- name: Create a Expected Switch
  nvidia.infra_controller.expected_switch:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: present
    name: "my-expected-switch"
    bmc_mac_address: "{{ bmc_mac_address }}"
    nvos_interfaces: "{{ nvos_interfaces }}"
    site_id: "{{ site_id }}"
    switch_serial_number: "{{ switch_serial_number }}"

- name: Delete a Expected Switch
  nvidia.infra_controller.expected_switch:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: absent
    name: "my-expected-switch"
```
