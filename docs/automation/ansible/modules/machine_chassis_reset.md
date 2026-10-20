# nvidia.infra_controller.machine_chassis_reset – manage Machine

Reset Machine Chassis

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `chassis_id` | `str` | Yes | ID path parameter: chassis_id. |
| `id` | `str` | No | ID of the resource. Used for lookup. |
| `machine_id` | `str` | Yes | ID path parameter: machine_id. |
| `state` | `str` | No | Desired state of the resource. Choices: `present`. |
| `wait` | `bool` | No | Wait for the resource to reach the desired state. |
| `wait_timeout` | `int` | No | Timeout in seconds for wait operations. |

## Examples

```yaml

```
