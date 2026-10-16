# nvidia.infra_controller.vpc_prefix – manage VPC Prefix

Retrieve all VPC Prefixes

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `id` | `str` | No | ID of the resource. Used for lookup. |
| `ip_block_id` | `str` | No |  |
| `name` | `str` | No |  |
| `prefix` | `str` | No | Exact network-aligned IPv4 or IPv6 CIDR to reserve. Accepted IPv6 text is canonicalized before persistence and response. IPv4 accepts `/8` through `/31`. IPv6 accepts `/8` through `/63` when the FNN VPC has `slaacEnabled=true`, or `/8` through `/126` otherwise. |
| `prefix_length` | `int` | No |  |
| `site_id` | `str` | No | Scope filter: site_id. |
| `state` | `str` | No | Desired state of the resource. Choices: `present`, `absent`. |
| `vpc_id` | `str` | No |  |
| `wait` | `bool` | No | Wait for the resource to reach the desired state. |
| `wait_timeout` | `int` | No | Timeout in seconds for wait operations. |

## Examples

```yaml
- name: Create a VPC Prefix
  nvidia.infra_controller.vpc_prefix:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: present
    name: "my-vpc-prefix"

- name: Delete a VPC Prefix
  nvidia.infra_controller.vpc_prefix:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: absent
    name: "my-vpc-prefix"
```
