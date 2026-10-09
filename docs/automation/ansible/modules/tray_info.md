# nvidia.infra_controller.tray_info – query Tray

Retrieve all Trays

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `id` | `str` | No | Filter by component ID. Can be specified multiple times to filter on more than one Tray ID. |
| `rack_id` | `str` | No | Filter by Rack ID |
| `rack_name` | `str` | No | Filter by Rack name |
| `site_id` | `str` | Yes | ID of the Site to retrieve Trays from |
| `slot_id` | `int` | No | Restrict to trays at this rack slot (matches `position.slotId`). Requires `rackId` or `rackName`. Composes with the rest of the filter via AND. |
| `type` | `str` | No | Filter by tray type. When `id` is specified, the type disambiguates component IDs shared by different component types. Choices: `Compute`, `NVSwitch`, `PowerShelf`. |

## Examples

```yaml
- name: List all Tray resources
  nvidia.infra_controller.tray_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    site_id: "{{ site_id }}"

- name: Get a specific Tray by ID
  nvidia.infra_controller.tray_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    site_id: "{{ site_id }}"
    id: "{{ resource_id }}"
```
