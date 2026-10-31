# nvidia.infra_controller.vpc_prefix_info – query VPC Prefix

Retrieve all VPC Prefixes

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `id` | `str` | No | ID of the resource to retrieve. |
| `include_usage_stats` | `bool` | No | When true, each VPC Prefix with IPv4 includes IPv4 usage statistics using the same structure as IP Block usage. Usage is derived from associated Ethernet interfaces and their IPv4 addresses. IP usage counts two addresses per interface, while prefix usage counts each distinct `/31` containing an assigned IPv4 address. |
| `query` | `str` | No | Search for matches across all VPC Prefixes. Input will be matched against name and status fields |
| `site_id` | `str` | No | Filter VPC Prefixes by Site, required if the vpcId query parameter is not specified |
| `status` | `str` | No | Filter VPC Prefixes by Status |
| `vpc_id` | `str` | No | Filter VPC Prefixes by VPC |

## Examples

```yaml
- name: List all VPC Prefix resources
  nvidia.infra_controller.vpc_prefix_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"

- name: Get a specific VPC Prefix by ID
  nvidia.infra_controller.vpc_prefix_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    id: "{{ resource_id }}"
```
