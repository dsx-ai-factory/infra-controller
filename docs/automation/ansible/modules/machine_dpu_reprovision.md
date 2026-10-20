# nvidia.infra_controller.machine_dpu_reprovision – manage Machine

Reprovision Machine DPUs

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `acknowledge_attached_instance` | `bool` | No | Acknowledges that an Instance is currently attached to the Machine and this action may disrupt Tenant workload on the Instance. |
| `id` | `str` | No | ID of the resource. Used for lookup. |
| `machine_id` | `str` | Yes | ID path parameter: machine_id. |
| `mode` | `str` | No | Use `Set` to start reprovisioning, `Clear` to remove a pending request, or `Restart` to restart DPUs that already have a request. Restart accepts a host Machine ID only. Choices: `Set`, `Clear`, `Restart`. |
| `state` | `str` | No | Desired state of the resource. Choices: `present`. |
| `update_firmware` | `bool` | No | Deprecated compatibility field. Firmware is always verified and updated during reprovisioning. |
| `wait` | `bool` | No | Wait for the resource to reach the desired state. |
| `wait_timeout` | `int` | No | Timeout in seconds for wait operations. |

## Examples

```yaml

```
