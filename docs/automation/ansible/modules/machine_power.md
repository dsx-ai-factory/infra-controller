# nvidia.infra_controller.machine_power – manage Machine

Machine power control

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `acknowledge_attached_instance` | `bool` | No | Acknowledges that an Instance is currently attached to the Machine and this action may disrupt Tenant workload on the Instance. |
| `action` | `str` | No | Redfish power control action to apply to the Machine. ACPowercycle is not supported on Viking systems. Choices: `true`, `GracefulShutdown`, `ForceOff`, `GracefulRestart`, `ForceRestart`, `ACPowercycle`. |
| `id` | `str` | No | ID of the resource. Used for lookup. |
| `machine_id` | `str` | Yes | ID path parameter: machine_id. |
| `state` | `str` | No | Desired state of the resource. Choices: `present`. |
| `wait` | `bool` | No | Wait for the resource to reach the desired state. |
| `wait_timeout` | `int` | No | Timeout in seconds for wait operations. |

## Examples

```yaml

```
