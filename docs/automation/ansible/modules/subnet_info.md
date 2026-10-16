# nvidia.infra_controller.subnet_info – query Subnet

Retrieve all Subnets

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `id` | `str` | No | ID of the resource to retrieve. |
| `include_usage_stats` | `bool` | No | When true, each Subnet object includes usage statistics using the same structure as IP Block usage. Prefix and IP usage data is derived by evaluating associated Ethernet interfaces. Each Interface associated with a Subnet consumes a single IP. In addition, one gateway and one broadcast IP address are reserved per Subnet. |
| `query` | `str` | No | Search for matches across all Subnets. Input will be matched against name, description, and status fields |
| `site_id` | `str` | No | Filter subnets by Site, required if the vpcId query parameter is not specified |
| `status` | `str` | No | Filter Subnets by Status |
| `vpc_id` | `str` | No | Filter subnets by VPC |

## Examples

```yaml
- name: List all Subnet resources
  nvidia.infra_controller.subnet_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"

- name: Get a specific Subnet by ID
  nvidia.infra_controller.subnet_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    id: "{{ resource_id }}"
```
