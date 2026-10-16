# nvidia.infra_controller.task_rule – manage Rule

List Operation Rules

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `description` | `str` | No | Optional free-form description. |
| `id` | `str` | No | ID of the resource. Used for lookup. |
| `name` | `str` | No | Human-readable name of the rule. |
| `operation_code` | `str` | No | Operation code within the operation type. For `PowerControl`, accepted values are `power_on`, `force_power_on`, `power_off`, `force_power_off`, `restart`, `force_restart`, `warm_reset`, and `cold_reset`. For `FirmwareControl`, accepted values are `upgrade`, `downgrade`, and `rollback`. The server validates the code against the selected type. |
| `operation_type` | `str` | No | Operation type the rule applies to. Choices: `PowerControl`, `FirmwareControl`. |
| `rule_definition` | `dict` | No | Executable definition of a rule. Structurally identical to Flow's own rule schema, so an existing YAML rule file maps across field for field. The fields declared here use `camelCase` (`componentType`, `mainOperation`, `pollInterval`) rather than the `snake_case` of Flow's YAML; keys inside the free-form `parameters` map pass through unchanged and stay `snake_case` (`expected_status`, `component_types`). |
| `site_id` | `str` | No | ID of the Site to create the rule on. |
| `state` | `str` | No | Desired state of the resource. Choices: `present`, `absent`. |
| `wait` | `bool` | No | Wait for the resource to reach the desired state. |
| `wait_timeout` | `int` | No | Timeout in seconds for wait operations. |

## Examples

```yaml
- name: Create a Rule
  nvidia.infra_controller.task_rule:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: present
    name: "my-task-rule"

- name: Delete a Rule
  nvidia.infra_controller.task_rule:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: absent
    name: "my-task-rule"
```
