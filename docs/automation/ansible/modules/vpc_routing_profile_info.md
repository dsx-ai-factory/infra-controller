# nvidia.infra_controller.vpc_routing_profile_info – query VPC

Retrieve VPC routing profile

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `vpc_id` | `str` | Yes | ID path parameter: vpc_id. |

## Examples

```yaml
- name: List all VPC resources
  nvidia.infra_controller.vpc_routing_profile_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    vpc_id: "{{ vpc_id }}"
```
