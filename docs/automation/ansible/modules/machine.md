# nvidia.infra_controller.machine – manage Machine

Retrieve all Machines

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `clear_instance_type` | `bool` | No | Set to true to clear the existing Instance Type. Cannot be specified if Instance Type ID is specified. Can only be set by Provider. |
| `health_issue` | `dict` | No | Required when `onlineRepair.enabled` is true. Must not be set when exiting online repair (`onlineRepair.enabled` false). |
| `id` | `str` | No | ID of the resource. Used for lookup. |
| `instance_type_id` | `str` | No | Update the Instance Type of the Machine. Cannot be specified when clearing Instance Type. Can only be updated by Provider. |
| `labels` | `dict` | No | Machine labels will be overwritten, include existing labels to preserve them. Can be updated by Provider or privileged Tenant. |
| `maintenance_message` | `str` | No | Optional message describing the reason for moving Machine into maintenance mode. Can be updated by Provider or privileged Tenant. |
| `online_repair` | `dict` | No | Request to enter/exit online repair |
| `set_maintenance_mode` | `bool` | No | Set to `true` to enable maintenance mode and to `false` to disable maintenance mode. Can be set by Provider or privileged Tenant. |
| `state` | `str` | No | Desired state of the resource. Choices: `present`, `absent`. |
| `wait` | `bool` | No | Wait for the resource to reach the desired state. |
| `wait_timeout` | `int` | No | Timeout in seconds for wait operations. |

## Examples

```yaml
- name: Delete a Machine
  nvidia.infra_controller.machine:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: absent
    id: "{{ resource_id }}"
```
