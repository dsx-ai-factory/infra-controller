# nvidia.infra_controller.vpc_prefix – manage VPC Prefix

Create, update, and delete VPC Prefixes.

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `id` | `str` | No | ID of the resource. Used for lookup. |
| `ip_block_id` | `str` | No | ID of the Ready tenant IPv4 or IPv6 IP Block to allocate the VPC Prefix from. The block must be at the FNN VPC's Site. Required when creating a new resource. |
| `name` | `str` | No | Human readable name for the VPC Prefix. Required when creating a new resource. |
| `prefix` | `str` | No | Exact network-aligned IPv4 or IPv6 CIDR to reserve. Accepted IPv6 text is canonicalized before persistence and response. IPv4 accepts `/8` through `/31`. IPv6 accepts `/8` through `/63` when the FNN VPC has `slaacEnabled=true`, or `/8` through `/126` otherwise. The CIDR must be contained by the selected IP Block, match its address family, and not overlap an existing reservation. An IPv4-mapped IPv6 CIDR is not accepted. Mutually exclusive with `prefixLength`. |
| `prefix_length` | `int` | No | Prefix length for automatic allocation. IPv4 accepts 8 through 31. IPv6 accepts 8 through 63 when the FNN VPC has `slaacEnabled=true`, or 8 through 126 otherwise. The selected IP Block must contain a prefix of the requested length. Mutually exclusive with `prefix`. |
| `site_id` | `str` | No | Scope filter: site_id. |
| `state` | `str` | No | Desired state of the resource. Choices: `present`, `absent`. |
| `vpc_id` | `str` | No | ID of the VPC. Required when creating a new resource. |
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
    ip_block_id: "{{ ip_block_id }}"
    vpc_id: "{{ vpc_id }}"
    prefix_length: "{{ prefix_length }}"

- name: Delete a VPC Prefix
  nvidia.infra_controller.vpc_prefix:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: absent
    name: "my-vpc-prefix"
```
