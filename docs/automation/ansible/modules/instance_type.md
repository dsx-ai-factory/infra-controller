# nvidia.infra_controller.instance_type – manage Instance Type

Retrieve all Instance Types

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `controller_machine_type` | `str` | No | Site Controller assigned Machine type |
| `description` | `str` | No | Description of the Instance Type |
| `id` | `str` | No | ID of the resource. Used for lookup. |
| `labels` | `dict` | No | User-defined key-value labels for the Instance Type |
| `machine_capabilities` | `list` | No | List of Machine Capabilities to match |
| `name` | `str` | No | Name of the Instance Type |
| `site_id` | `str` | No | ID of the site |
| `state` | `str` | No | Desired state of the resource. Choices: `present`, `absent`. |
| `wait` | `bool` | No | Wait for the resource to reach the desired state. |
| `wait_timeout` | `int` | No | Timeout in seconds for wait operations. |

## Examples

```yaml
- name: Create a Instance Type
  nvidia.infra_controller.instance_type:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: present
    name: "my-instance-type"

- name: Delete a Instance Type
  nvidia.infra_controller.instance_type:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: absent
    name: "my-instance-type"
```
