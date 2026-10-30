# nvidia.infra_controller.machine_chassis_reset – manage Machine

Reset Machine Chassis

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `chassis_id` | `str` | Yes | ID path parameter: chassis_id. |
| `id` | `str` | No | ID of the resource. Used for lookup. |
| `machine_id` | `str` | Yes | ID path parameter: machine_id. |
| `state` | `str` | No | Desired state of the resource. Choices: `present`. |

## Examples

```yaml
- name: Reset Machine Chassis
  nvidia.infra_controller.machine_chassis_reset:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: present
    chassis_id: "{{ chassis_id }}"
    machine_id: "{{ machine_id }}"
```
