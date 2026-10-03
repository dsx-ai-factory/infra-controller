# nvidia.infra_controller.instance_batch – batch create Instance

Instance is a Machine provisioned with an Operating System by a Tenant and attached to one or more VPC Prefixes or Subnets.

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `always_boot_with_custom_ipxe` | `bool` | No | When set to true, the iPXE script specified by OS or overridden here will always be run when rebooting the Instances. OS must be of iPXE type. |
| `auto_network` | `bool` | No | When true, asks NICo to auto-resolve each Instance's network interfaces from the host's underlay (HostInband) network segments. Intended for instances on zero-DPU hosts (or hosts with their DPU in NIC mode). When true: (1) the target VPC's `networkVirtualizationType` MUST be `FLAT`, (2) `interfaces` MUST be empty or omitted, and (3) `secondaryVpcIds` MUST be empty or omitted. |
| `count` | `int` | Yes | Number of instances to create in this batch. Minimum 2, maximum 18 (limited by topology domain size) |
| `description` | `str` | No | Description applied to all instances in the batch, optional |
| `dpu_extension_service_deployments` | `list` | No | DPU Extension Services to deploy to all instances in the batch |
| `infiniband_interfaces` | `list` | No | InfiniBand interface configuration shared across all instances |
| `instance_type_id` | `str` | Yes | ID of the Instance Type to use for all Instances in the batch |
| `interfaces` | `list` | No | Interface configuration shared across all instances. At least one interface must be specified unless `autoNetwork` is true. Interfaces must all be Subnet-backed or all be VPC-backed; VPC-backed interfaces may use an explicit `vpcPrefixId` or ask the Controller to select a prefix using `vpcId` and `ipFamilies`. Each batch member is resolved independently and may use a different prefix. Only one network can be attached over a physical interface. Interface `ipAddress` is not supported for batch instance creation requests. Mutually exclusive with `autoNetwork`: when `autoNetwork` is true this list MUST be empty. |
| `ipxe_script` | `str` | No | Override iPXE script specified in OS, must be specified if Operating System is not specified |
| `labels` | `dict` | No | Key-value objects to be applied to all instances (shared across all instances) |
| `machine_label_selector` | `dict` | No | Optional exact-match selector applied to Machine labels during placement. Property names are arbitrary Machine label keys rather than predefined selector fields. Every supplied key/value pair must match (AND semantics). An omitted or empty object does not restrict placement. The selector constrains placement only; it is not persisted on the created Instances. A non-empty object requires the Tenant to have effective `targetedInstanceCreation` capability for the selected Site; otherwise the request is rejected with 403. Selection occurs before topology optimization. When `topologyOptimized` is true, all selected Machines must both match the selector and belong to the same NVLink domain. If too few matching Machines are available, the request is rejected with 409. |
| `name_prefix` | `str` | Yes | Prefix for instance names. Instances will be named with this prefix followed by a random 6-character suffix (e.g., "worker" becomes "worker-abc123") |
| `network_security_group_id` | `str` | No | ID of a Network Security Group to attach to all instances |
| `nv_link_interfaces` | `list` | No | NVLink interface configuration shared across all instances. A subset of GPUs may be specified. Each item references one GPU index (`deviceInstance`) and one NVLink Logical Partition. Different interfaces may reference different NVLink Logical Partitions. |
| `operating_system_id` | `str` | No | Must be specified if iPXE Script field is empty |
| `phone_home_enabled` | `bool` | No | When set to true, the Instances will be enabled with the Phone Home service. |
| `power_profile` | `str` | No | Power profile to apply to every Instance in the batch. A non-empty value requires the Site's `dpsPowerManagement` capability to be `true`. |
| `secondary_vpc_ids` | `list` | No | IDs of additional VPCs the Instances should attach to through non-primary interfaces. This field may only be specified when every entry in `interfaces` uses `vpcPrefixId` or `vpcId`. IDs must be unique, must be valid UUIDs, and must not include the primary `vpcId`. |
| `spectrum_x_attachments` | `list` | No | SpectrumX Partition attachments shared across all Instances in the batch. Each `device` and `deviceInstance` pair may appear only once, irrespective of `virtualFunctionId`. |
| `ssh_key_group_ids` | `list` | No | SSH Key Group IDs that will provide Serial over LAN access to all instances |
| `tenant_id` | `str` | Yes | ID of the Tenant creating the Instances |
| `topology_optimized` | `bool` | No | When true (default), all instances must be allocated on machines within the same NVLink domain. When false, instances can be spread across different NVLink domains. |
| `user_data` | `str` | No | User data applied to all instances. Can only be specified if allowOverride is set to true in Operating System. Limited to 32768 bytes (32 KiB), measured on the effective value NICo stores rather than the text submitted. Operating System defaults are inherited first, and when phone-home is configured the document is re-serialized with a `phone_home` block added. Re-serialization normalizes indentation and can grow the document, so a request just under the limit may still be rejected. |
| `vpc_id` | `str` | Yes | ID of the VPC the Instances should belong to |

## Examples

```yaml
- name: Batch create Instance resources
  nvidia.infra_controller.instance_batch:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    count: "{{ count }}"
    instance_type_id: "{{ instance_type_id }}"
    name_prefix: "{{ name_prefix }}"
    tenant_id: "{{ tenant_id }}"
    vpc_id: "{{ vpc_id }}"
```
