# nvidia.infra_controller.audit_info – query Audit

Retrieve all Audit Log Entries

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `failed_only` | `bool` | No | Return only audit log entries that have failed status code (>= 400) |
| `id` | `str` | No | ID of the resource to retrieve. |

## Examples

```yaml
- name: List all Audit resources
  nvidia.infra_controller.audit_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"

- name: Get a specific Audit by ID
  nvidia.infra_controller.audit_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    id: "{{ resource_id }}"
```
