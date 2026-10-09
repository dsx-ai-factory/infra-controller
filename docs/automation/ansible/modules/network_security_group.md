# nvidia.infra_controller.network_security_group – manage Network Security Group

Retrieve all Network Security Groups

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `description` | `str` | No | Description of the Network Security Group |
| `id` | `str` | No | ID of the resource. Used for lookup. |
| `labels` | `dict` | No | User-defined key-value labels for the Network Security Group |
| `name` | `str` | No | Name of the Network Security Group |
| `rules` | `list` | No | Rules that belong to the Network Security Group |
| `site_id` | `str` | No | ID of the Site |
| `state` | `str` | No | Desired state of the resource. Choices: `present`, `absent`. |
| `stateful_egress` | `bool` | No | Egress rules with protocol and destination ports defined but without source ports defined should automatically be made stateful. |
| `wait` | `bool` | No | Wait for the resource to reach the desired state. |
| `wait_timeout` | `int` | No | Timeout in seconds for wait operations. |

## Examples

```yaml
- name: Create a Network Security Group
  nvidia.infra_controller.network_security_group:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: present
    name: "my-network-security-group"

- name: Delete a Network Security Group
  nvidia.infra_controller.network_security_group:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: absent
    name: "my-network-security-group"
```
