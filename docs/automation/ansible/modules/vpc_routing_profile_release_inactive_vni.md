# nvidia.infra_controller.vpc_routing_profile_release_inactive_vni – manage VPC

Release inactive VPC VNI

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `expected_inactive_vni` | `int` | No | Exact retained VNI observed with `ifVersionMatch`. It must match the VPC's inactive allocation and differ from its active VNI. |
| `id` | `str` | No | ID of the resource. Used for lookup. |
| `if_version_match` | `str` | No | Exact Core VPC version from routing-state inspection used for the release decision, in `V<counter>-T<microseconds>` format. Missing or malformed versions are rejected. A stale version returns 412; after an ambiguous release, that error does not prove whether the previous release committed. |
| `state` | `str` | No | Desired state of the resource. Choices: `present`. |
| `vpc_id` | `str` | Yes | ID path parameter: vpc_id. |
| `wait` | `bool` | No | Wait for the resource to reach the desired state. |
| `wait_timeout` | `int` | No | Timeout in seconds for wait operations. |

## Examples

```yaml
- name: Create a VPC
  nvidia.infra_controller.vpc_routing_profile_release_inactive_vni:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: present
    vpc_id: "{{ vpc_id }}"

```
