# nvidia.infra_controller.expected_machine – manage Expected Machine

Retrieve all Expected Machines

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `bmc_ip_address` | `str` | No | Optional BMC IP address (IPv4 or IPv6). When set, pre-allocates a reserved IP for the BMC. |
| `bmc_mac_address` | `str` | No | MAC address of the Expected Machine's BMC (Baseboard Management Controller) |
| `chassis_serial_number` | `str` | No | Serial number of the Expected Machine's chassis |
| `default_bmc_password` | `str` | No | Password for accessing the Expected Machine's BMC |
| `default_bmc_username` | `str` | No | Username for accessing the Expected Machine's BMC |
| `description` | `str` | No | Description of this component |
| `fallback_dpu_serial_numbers` | `list` | No | Serial numbers of the Expected Machine's fallback DPUs (Data Processing Units) |
| `host_id` | `int` | No | Host ID within the tray |
| `host_lifecycle_profile` | `dict` | No | Optional per-host lifecycle profile |
| `id` | `str` | No | ID of the resource. Used for lookup. |
| `is_dpf_enabled` | `bool` | No | When true, this host is eligible for DPF-based provisioning. Optional. Omission or null uses an effective default of true. Set false explicitly to disable DPF-based provisioning. |
| `labels` | `dict` | No | User-defined key-value pairs for organizing and categorizing Expected Machines |
| `manufacturer` | `str` | No | Manufacturer of this component |
| `model` | `str` | No | Model of this component |
| `name` | `str` | No | Display name for this component |
| `rack_id` | `str` | No | Optional rack identifier for this component |
| `site_id` | `str` | No | ID of the site the Expected Machine belongs to |
| `sku_id` | `str` | No | Optional ID of the SKU to associate with this Expected Machine |
| `slot_id` | `int` | No | Slot ID within the rack |
| `state` | `str` | No | Desired state of the resource. Choices: `present`, `absent`. |
| `tray_idx` | `int` | No | Tray index within the rack |
| `wait` | `bool` | No | Wait for the resource to reach the desired state. |
| `wait_timeout` | `int` | No | Timeout in seconds for wait operations. |

## Examples

```yaml
- name: Create a Expected Machine
  nvidia.infra_controller.expected_machine:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: present
    name: "my-expected-machine"

- name: Delete a Expected Machine
  nvidia.infra_controller.expected_machine:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: absent
    name: "my-expected-machine"
```
