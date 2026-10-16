# nvidia.infra_controller.vpc_routing_profile – manage VPC

Retrieve VPC routing profile

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `id` | `str` | No | ID of the resource. Used for lookup. |
| `routing_profile` | `str` | No | Required Site-configured destination profile with the opposite internal setting from the current profile. The REST aliases `external`, `internal`, and `privileged-internal` map to Core's `EXTERNAL`, `INTERNAL`, and `PRIVILEGED_INTERNAL` names. Other configured names are passed unchanged; this is not a closed enum. |
| `state` | `str` | No | Desired state of the resource. Choices: `present`. |
| `vni` | `int` | No | Optional exact destination VNI. Omission or null reuses an allocation retained in the destination pool, otherwise allocates automatically. An exact request must match a retained destination allocation if present; otherwise it must name a free materialized entry in the destination pool, regardless of that entry's automatic-allocation setting. A mismatched, occupied, missing, or already active VNI is rejected without fallback. |
| `vpc_id` | `str` | Yes | ID path parameter: vpc_id. |
| `wait` | `bool` | No | Wait for the resource to reach the desired state. |
| `wait_timeout` | `int` | No | Timeout in seconds for wait operations. |

## Examples

```yaml

```
