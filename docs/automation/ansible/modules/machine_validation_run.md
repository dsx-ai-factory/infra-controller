# nvidia.infra_controller.machine_validation_run – manage Machine

Create Machine validation runs.

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `allowed_tests` | `list` | No | Validation test names allowed for this run. |
| `contexts` | `list` | No | Contexts used to select validation tests. |
| `id` | `str` | No | ID of the resource. Used for lookup. |
| `machine_id` | `str` | Yes | ID path parameter: machine_id. |
| `run_unverified_tests` | `bool` | No | Whether unverified validation tests may run. |
| `state` | `str` | No | Desired state of the resource. Choices: `present`. |
| `tags` | `list` | No | Tags used to select validation tests. |

## Examples

```yaml
- name: Create a Machine
  nvidia.infra_controller.machine_validation_run:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: present
    machine_id: "{{ machine_id }}"

```
