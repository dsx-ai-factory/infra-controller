# nvidia.infra_controller.expected_power_shelf – manage Expected Power Shelf

Retrieve all Expected Power Shelves

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `bmc_ip_address` | `str` | No | Optional BMC IP address (IPv4 or IPv6). When set, pre-allocates a reserved IP for the BMC. |
| `bmc_mac_address` | `str` | No | MAC address of the Expected Power Shelf's BMC (Baseboard Management Controller) |
| `default_bmc_password` | `str` | No | Password for accessing the Expected Power Shelf's BMC |
| `default_bmc_username` | `str` | No | Username for accessing the Expected Power Shelf's BMC |
| `description` | `str` | No | Description of this component |
| `host_id` | `int` | No | Host ID within the tray |
| `id` | `str` | No | ID of the resource. Used for lookup. |
| `labels` | `dict` | No | User-defined key-value pairs for organizing and categorizing Expected Power Shelves |
| `manufacturer` | `str` | No | Manufacturer of this component |
| `model` | `str` | No | Model of this component |
| `name` | `str` | No | Display name for this component |
| `rack_id` | `str` | No | Optional rack identifier for this component |
| `shelf_serial_number` | `str` | No | Serial number of the Expected Power Shelf |
| `site_id` | `str` | No | ID of the site the Expected Power Shelf belongs to |
| `slot_id` | `int` | No | Slot ID within the rack |
| `state` | `str` | No | Desired state of the resource. Choices: `present`, `absent`. |
| `tray_idx` | `int` | No | Tray index within the rack |
| `wait` | `bool` | No | Wait for the resource to reach the desired state. |
| `wait_timeout` | `int` | No | Timeout in seconds for wait operations. |

## Examples

```yaml
- name: Create a Expected Power Shelf
  nvidia.infra_controller.expected_power_shelf:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: present
    name: "my-expected-power-shelf"

- name: Delete a Expected Power Shelf
  nvidia.infra_controller.expected_power_shelf:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: absent
    name: "my-expected-power-shelf"
```
