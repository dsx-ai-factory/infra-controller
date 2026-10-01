# nvidia.infra_controller.domain_nvlink_power – manage Domain

Power control NVLink Domains

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `id` | `str` | Yes | ID of the resource. Used for lookup. |
| `override_readiness_check` | `bool` | No | When true, proceed even if one or more target components (or hosts on the owning rack for rack-scoped components) are reported as not ready by their persisted status. Intended for operator-supervised maintenance. |
| `rule_id` | `str` | No | Optional Operation Rule UUID. When set, pins this operation to the named rule and overrides Flow's default rule resolution. |
| `site_id` | `str` | No | ID of the Site |
| `state` | `str` | No | Desired state of the resource. Choices: `present`. |
| `wait` | `bool` | No | Wait for the resource to reach the desired state. |
| `wait_timeout` | `int` | No | Timeout in seconds for wait operations. |

## Examples

```yaml

```
