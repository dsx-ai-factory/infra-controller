# nvidia.infra_controller.vpc_peering – manage VPC Peering

Retrieve all VPC peerings

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `id` | `str` | No | ID of the resource. Used for lookup. |
| `site_id` | `str` | No | ID of the Site where the peering exists |
| `state` | `str` | No | Desired state of the resource. Choices: `present`, `absent`. |
| `vpc1_id` | `str` | No | ID of the first VPC in the peering |
| `vpc2_id` | `str` | No | ID of the second VPC to peer with |
| `wait` | `bool` | No | Wait for the resource to reach the desired state. |
| `wait_timeout` | `int` | No | Timeout in seconds for wait operations. |

## Examples

```yaml
- name: Create a VPC Peering
  nvidia.infra_controller.vpc_peering:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: present

- name: Delete a VPC Peering
  nvidia.infra_controller.vpc_peering:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: absent
    id: "{{ resource_id }}"
```
