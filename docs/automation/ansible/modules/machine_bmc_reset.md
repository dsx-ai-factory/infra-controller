# nvidia.infra_controller.machine_bmc_reset – manage Machine

Reset Machine BMC

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `acknowledge_attached_instance` | `bool` | No | Acknowledges that an Instance is currently attached to the Machine and this action may disrupt Tenant workload on the Instance. |
| `id` | `str` | No | ID of the resource. Used for lookup. |
| `machine_id` | `str` | Yes | ID path parameter: machine_id. |
| `state` | `str` | No | Desired state of the resource. Choices: `present`. |
| `use_ipmi_tool` | `bool` | No | Reset the BMC via ipmitool instead of Redfish. The request may be silently ignored while the BMC is in lockdown mode. |
| `wait` | `bool` | No | Wait for the resource to reach the desired state. |
| `wait_timeout` | `int` | No | Timeout in seconds for wait operations. |

## Examples

```yaml

```
