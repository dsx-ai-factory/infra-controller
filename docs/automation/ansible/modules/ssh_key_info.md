# nvidia.infra_controller.ssh_key_info – query SSH Key

Retrieve all SSH Keys

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `id` | `str` | No | ID of the resource to retrieve. |
| `query` | `str` | No | Search for matches across all SSH Keys. Input will be matched against the name field |
| `ssh_key_group_id` | `str` | No | ID of the SSH Key Group |

## Examples

```yaml
- name: List all SSH Key resources
  nvidia.infra_controller.ssh_key_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"

- name: Get a specific SSH Key by ID
  nvidia.infra_controller.ssh_key_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    id: "{{ resource_id }}"
```
