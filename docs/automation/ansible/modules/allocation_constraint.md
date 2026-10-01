# nvidia.infra_controller.allocation_constraint – manage Allocation

Update Allocation Constraint

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `allocation_id` | `str` | Yes | Scope filter: allocation_id. |
| `constraint_value` | `int` | No | Value of the Allocation Constraint. For InstanceType, this value represents number of Machines allocated for Tenant. For IPBlock, this value represents the prefix Length of the IP Block. |
| `id` | `str` | No | ID of the resource. Used for lookup. |
| `state` | `str` | No | Desired state of the resource. Choices: `present`. |
| `wait` | `bool` | No | Wait for the resource to reach the desired state. |
| `wait_timeout` | `int` | No | Timeout in seconds for wait operations. |

## Examples

```yaml

```
