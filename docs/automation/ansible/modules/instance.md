# nvidia.infra_controller.instance – manage Instance

Retrieve all Instances

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `allow_unhealthy_machine` | `bool` | No | Set to true in order to target Machines are in maintenance or have health alerts preventing regular provision flow. Requires Targeted Instance Creation capability enabled for Tenant |
| `always_boot_with_custom_ipxe` | `bool` | No | When set to true, the iPXE script specified by OS or overridden here will always be run when rebooting the Instance. OS must be of iPXE type. |
| `apply_updates_on_reboot` | `bool` | No | When specified, pending Instance updates such as DPU reprovisioning are applied on reboot |
| `auto_network` | `bool` | No | When true, asks NICo to auto-resolve the Instance's network interfaces from the host's underlay (HostInband) network segments. Intended for instances on zero-DPU hosts (or hosts with their DPU in NIC mode). When true: (1) the target VPC's `networkVirtualizationType` MUST be `FLAT`, (2) `interfaces` MUST be empty or omitted, and (3) `secondaryVpcIds` MUST be empty or omitted. Resolved interfaces surface on the Instance's read response. |
| `description` | `str` | No | Description of the Instance, optional |
| `dpu_extension_service_deployments` | `list` | No | DPU Extension Services to deploy to the DPUs of this Instance |
| `id` | `str` | No | ID of the resource. Used for lookup. |
| `infiniband_interfaces` | `list` | No | Associate one or more Partitions with this Instance |
| `instance_type_id` | `str` | No | ID of the Instance Type to use for Instance |
| `interfaces` | `list` | No | At least one interface must be specified unless `autoNetwork` is true. Interfaces must all be Subnet-backed or all be VPC-backed; VPC-backed interfaces may use an explicit `vpcPrefixId` or ask the Controller to select a prefix using `vpcId` and `ipFamilies`. Only one network can be attached over a physical interface. If only one Subnet is specified, it will be attached over a physical interface regardless of `isPhysical`. Mutually exclusive with `autoNetwork`: when `autoNetwork` is true this list MUST be empty. |
| `ipxe_script` | `str` | No | Override iPXE script specified in OS, must be specified if Operating System is not specified |
| `is_repair_tenant` | `bool` | No | Should be set to true for Tenants who are performing investigation/repairing the Machine. Otherwise omit or set to false |
| `labels` | `dict` | No | User-defined key-value labels |
| `machine_health_issue` | `dict` | No | Information regarding issue with the underlying Machine experienced by Tenant |
| `machine_id` | `str` | No | ID of of specific Machine to use for Instance. Requires Targeted Instance Creation capability enabled for Tenant |
| `machine_label_selector` | `dict` | No | Optional exact-match selector applied to Machine labels during placement. Property names are arbitrary Machine label keys rather than predefined selector fields. Every supplied key/value pair must match (AND semantics). An omitted or empty object does not restrict placement. A non-empty object requires the Tenant to have effective `targetedInstanceCreation` capability for the selected Site; otherwise the request is rejected with 403. When `instanceTypeId` is supplied, NICo selects from Ready, unassigned Machines of that Instance Type that match the selector. When `machineId` is supplied, the specified Machine must match the selector or the request is rejected with 400. The selector constrains placement only; it is not persisted on the created Instance. |
| `name` | `str` | No | Name of the Instance |
| `network_security_group_id` | `str` | No | ID of the desired Network Security Group to attach to the Instance |
| `nv_link_interfaces` | `list` | No | Define Interfaces to associate Instance GPUs with NVLink Logical Partitions. A subset of GPUs may be specified (it is not required to include all GPUs). Each item references one GPU index (`deviceInstance`) and one NVLink Logical Partition. Different interfaces may reference different NVLink Logical Partitions. |
| `operating_system_id` | `str` | No | Must be specified if iPXE Script field is empty |
| `phone_home_enabled` | `bool` | No | When set to true, the Instance will be enabled with the Phone Home service. |
| `power_profile` | `str` | No | Power profile to apply to the Instance. A non-empty value requires the Site's `dpsPowerManagement` capability to be `true`. |
| `reboot_with_custom_ipxe` | `bool` | No | When specified along with triggerReboot, the Instance will boot using the custom iPXE specified by OS. If Instance has alwaysBootWithCustomIpxe flag set then this value will be ignored. |
| `secondary_vpc_ids` | `list` | No | IDs of additional VPCs the Instance should attach to through non-primary interfaces. This field may only be specified when every entry in `interfaces` uses `vpcPrefixId` or `vpcId`. IDs must be unique, must be valid UUIDs, and must not include the primary `vpcId`. |
| `spectrum_x_attachments` | `list` | No | Associate one or more SpectrumX Partitions with this Instance. Each `device` and `deviceInstance` pair may appear only once, irrespective of `virtualFunctionId`. |
| `ssh_key_group_ids` | `list` | No | Specify list of SSH Key Group IDs that will provide Serial over LAN access |
| `state` | `str` | No | Desired state of the resource. Choices: `present`, `absent`. |
| `tenant_id` | `str` | No | ID of the Tenant creating the Instance |
| `trigger_reboot` | `bool` | No | Trigger power cycle for Instance |
| `user_data` | `str` | No | Can only be specified if allowOverride is set to true in Operating System. Limited to 32768 bytes (32 KiB), measured on the effective value NICo stores rather than the text submitted. Operating System defaults are inherited first, and when phone-home is configured the document is re-serialized with a `phone_home` block added. Re-serialization normalizes indentation and can grow the document, so a request just under the limit may still be rejected. |
| `vpc_id` | `str` | No | ID of the VPC the Instance should belong to |
| `wait` | `bool` | No | Wait for the resource to reach the desired state. |
| `wait_timeout` | `int` | No | Timeout in seconds for wait operations. |

## Examples

```yaml
- name: Create a Instance
  nvidia.infra_controller.instance:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: present
    name: "my-instance"

- name: Delete a Instance
  nvidia.infra_controller.instance:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: absent
    name: "my-instance"
```
