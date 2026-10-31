# nvidia.infra_controller.subnet – manage Subnet

Create, update, and delete Subnets.

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `description` | `str` | No | Description of the Subnet |
| `id` | `str` | No | ID of the resource. Used for lookup. |
| `ipv4_block_id` | `str` | No | ID of the Ready, derived Tenant IPv4 Block from an Allocation. Required when creating a new resource. |
| `name` | `str` | No | Name of the Subnet. Required when creating a new resource. |
| `prefix_length` | `int` | No | Length of the IPv4 prefix, from 8 through 30. Required when creating a new resource. |
| `site_id` | `str` | No | Scope filter: site_id. |
| `state` | `str` | No | Desired state of the resource. Choices: `present`, `absent`. |
| `vpc_id` | `str` | No | ID of the Ethernet virtualizer VPC containing the Subnet. Required when creating a new resource. |
| `wait` | `bool` | No | Wait for the resource to reach the desired state. |
| `wait_timeout` | `int` | No | Timeout in seconds for wait operations. |

## Examples

```yaml
- name: Create a Subnet
  nvidia.infra_controller.subnet:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: present
    name: "my-subnet"
    ipv4_block_id: "{{ ipv4_block_id }}"
    prefix_length: "{{ prefix_length }}"
    vpc_id: "{{ vpc_id }}"

- name: Delete a Subnet
  nvidia.infra_controller.subnet:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: absent
    name: "my-subnet"
```
