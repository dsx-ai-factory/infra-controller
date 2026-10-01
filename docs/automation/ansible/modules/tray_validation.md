# nvidia.infra_controller.tray_validation – manage Tray

Manage Tray resources.

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `id` | `str` | No | ID of the resource. When provided, targets a single resource. |
| `manufacturer` | `str` | No | Filter trays by manufacturer |
| `name` | `str` | No | Filter trays by name |
| `rack_id` | `str` | No | Scope to a specific Rack by Rack ID (mutually exclusive with rackName) |
| `rack_name` | `str` | No | Scope to a specific Rack by name (mutually exclusive with rackId) |
| `site_id` | `str` | Yes | ID of the Site |
| `slot_id` | `str` | No | Restrict validation to trays at this rack slot (matches `position.slotId`). Requires `rackId` or `rackName`. Composes with the rest of the filter via AND. |
| `type` | `str` | No | Filter trays by type. When `id` is specified, the type disambiguates component IDs shared by different component types. |

## Examples

```yaml
- name: Run validation on all tray resources
  nvidia.infra_controller.tray_validation:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"

- name: Run validation on a specific tray
  nvidia.infra_controller.tray_validation:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    id: "{{ resource_id }}"
```
