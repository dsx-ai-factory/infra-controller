# nvidia.infra_controller.tray_health_report_info – query Tray

Retrieve all Tray health reports

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `id` | `str` | Yes | ID of the resource to retrieve. |
| `site_id` | `str` | Yes | ID of the Site that owns the Tray |
| `type` | `str` | Yes | Tray type used to select the corresponding component health-report resource Choices: `Compute`, `NVSwitch`, `PowerShelf`. |

## Examples

```yaml
- name: List all Tray resources
  nvidia.infra_controller.tray_health_report_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    site_id: "{{ site_id }}"
    type: "{{ type }}"

- name: Get a specific Tray by ID
  nvidia.infra_controller.tray_health_report_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    site_id: "{{ site_id }}"
    type: "{{ type }}"
    id: "{{ resource_id }}"
```
