# nvidia.infra_controller.ssh_key_group_info – query SSH Key Group

Retrieve all SSH Key Groups

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `id` | `str` | No | ID of the resource to retrieve. |
| `instance_id` | `str` | No | Filter SSH Key Groups by Instance ID |
| `query` | `str` | No | Search for matches across all SSH Key Groups. Input will be matched against the name field |
| `site_id` | `str` | No | Filter SSH Key Groups by Site ID |
| `status` | `str` | No | Status filter for the SSH Key Groups Choices: `Syncing`, `Synced`, `Error`, `Deleting`. |

## Examples

```yaml
- name: List all SSH Key Group resources
  nvidia.infra_controller.ssh_key_group_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"

- name: Get a specific SSH Key Group by ID
  nvidia.infra_controller.ssh_key_group_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    id: "{{ resource_id }}"
```
