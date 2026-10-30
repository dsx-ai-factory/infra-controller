# nvidia.infra_controller.allocation_constraint – manage Allocation

Update Allocation Constraint

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `allocation_id` | `str` | Yes | Scope filter: allocation_id. |
| `constraint_value` | `int` | Yes | Value of the Allocation Constraint. For InstanceType, this value represents number of Machines allocated for Tenant. For IPBlock, this value represents the prefix Length of the IP Block. |
| `id` | `str` | No | ID of the resource. Used for lookup. |
| `state` | `str` | No | Desired state of the resource. Choices: `present`. |

## Examples

```yaml
- name: Update Allocation Constraint
  nvidia.infra_controller.allocation_constraint:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: present
    allocation_id: "{{ allocation_id }}"
    constraint_value: "{{ constraint_value }}"
```
