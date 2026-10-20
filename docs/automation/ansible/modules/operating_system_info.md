# nvidia.infra_controller.operating_system_info – query Operating System

Retrieve all Operating Systems

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `id` | `str` | No | ID of the resource to retrieve. |
| `query` | `str` | No | Provide query to search for matches. Input will be matched against name, description and status fields |
| `site_id` | `str` | No | Filter Operating Systems by Site ID. Can be specified multiple times to filter on more than one ID. |
| `status` | `str` | No | Filter Operating Systems by Status. Can be specified multiple times to filter on more than one status. |
| `type` | `str` | No | Filter Operating Systems by Type Choices: `Image`, `iPXE`, `TemplatedIpxe`. |

## Examples

```yaml
- name: List all Operating System resources
  nvidia.infra_controller.operating_system_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"

- name: Get a specific Operating System by ID
  nvidia.infra_controller.operating_system_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    id: "{{ resource_id }}"
```
