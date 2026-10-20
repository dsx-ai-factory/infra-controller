# nvidia.infra_controller.vpc – manage VPC

Retrieve all VPCs

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `description` | `str` | No | Optional description for the VPC |
| `id` | `str` | No | ID of the resource. Used for lookup. |
| `labels` | `dict` | No | String key-value pairs describing VPC labels. Up to 10 key-value pairs can be specified |
| `name` | `str` | No | Name of the VPC |
| `network_security_group_id` | `str` | No | ID of the Network Security Group to attach to the VPC |
| `network_virtualization_type` | `str` | No | Network virtualization type of the VPC. If no value is specified, then defaults to `FNN` if Site has native networking enabled, or `ETHERNET_VIRTUALIZER` if native networking is disabled. Flat VPCs hold instances on zero-DPU hosts (or hosts with their DPU in NIC mode) and are never auto-selected -- `FLAT` must be specified explicitly. Choices: `ETHERNET_VIRTUALIZER`, `FNN`, `FLAT`. |
| `nv_link_logical_partition_id` | `str` | No | ID of the default NVLink Logical Partition that GPUs for all Instances in the VPC will attach to |
| `power_resource_group` | `str` | No | Power resource group to associate with the VPC. A non-empty value requires the Site's `dpsPowerManagement` capability to be `true`. |
| `routing_profile` | `str` | No | Specify a Site-configured routing profile returned by GET `/tenant/current/routing-profile` for the VPC. Only supported when `networkVirtualizationType` is set to `FNN`, or when `networkVirtualizationType` is omitted and Site has Native Networking enabled. Requires Tenant to have elevated privilege. |
| `routing_profile_overrides` | `dict` | No | Routing-profile properties to overlay on the resolved named profile. Only supported for FNN VPCs and requires `TargetedInstanceCreation` to be effective for the Tenant at the VPC's Site. `routingProfile` may be omitted when the Site and Tenant configuration select a named profile. |
| `site_id` | `str` | No | ID of the Site where the VPC should be created |
| `slaac_enabled` | `bool` | No | When true, Core allocates a `/64` to each instance interface that includes IPv6 and retains the prefix without assigning a concrete IPv6 host address. It is supported only for FNN VPCs and fixed during creation. False or omission disables SLAAC. Before persistence, REST requires `vpcSlaac` in the latest successfully stored configuration inventory for the selected Site. Periodic Site inventory reports whether Core supports this feature, so the stored value can lag a Core rollout. False or missing `vpcSlaac` returns 412 before REST persistence or workflow dispatch. This flag does not verify DPU agent versions. When a new API server release is deployed, DPU agents roll forward, and instance network configuration may fail transiently until eligible agents converge. NICo does not yet configure router advertisements (RAs); that support is tracked by https://github.com/NVIDIA/infra-controller/issues/2398. |
| `state` | `str` | No | Desired state of the resource. Choices: `present`, `absent`. |
| `vni` | `int` | No | Explicitly requested VNI for the VPC |
| `wait` | `bool` | No | Wait for the resource to reach the desired state. |
| `wait_timeout` | `int` | No | Timeout in seconds for wait operations. |

## Examples

```yaml
- name: Create a VPC
  nvidia.infra_controller.vpc:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: present
    name: "my-vpc"

- name: Delete a VPC
  nvidia.infra_controller.vpc:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: absent
    name: "my-vpc"
```
