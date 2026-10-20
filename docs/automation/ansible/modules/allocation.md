# nvidia.infra_controller.allocation – manage Allocation

Retrieve all Allocations

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `allocation_constraints` | `list` | No | List of Allocation Constraint objects |
| `description` | `str` | No | Detailed description for the Allocation |
| `id` | `str` | No | ID of the resource. Used for lookup. |
| `name` | `str` | No | Concise and descriptive name for the Allocation |
| `site_id` | `str` | No | ID of the Site where resources should be allocated |
| `state` | `str` | No | Desired state of the resource. Choices: `present`, `absent`. |
| `tenant_id` | `str` | No | ID of the Tenant that should receive the Allocation |
| `wait` | `bool` | No | Wait for the resource to reach the desired state. |
| `wait_timeout` | `int` | No | Timeout in seconds for wait operations. |

## Examples

```yaml
- name: Create a Allocation
  nvidia.infra_controller.allocation:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: present
    name: "my-allocation"

- name: Delete a Allocation
  nvidia.infra_controller.allocation:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: absent
    name: "my-allocation"
```
