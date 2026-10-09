# nvidia.infra_controller.ssh_key_group – manage SSH Key Group

Retrieve all SSH Key Groups

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `description` | `str` | No | Description of the SSHKeyGroup |
| `id` | `str` | No | ID of the resource. Used for lookup. |
| `name` | `str` | No | Name of the SSHKeyGroup |
| `site_ids` | `list` | No | List of Site objects |
| `ssh_key_ids` | `list` | No | List of SSHKeyID objects |
| `state` | `str` | No | Desired state of the resource. Choices: `present`, `absent`. |
| `version` | `str` | No | Version of the SSH Key Group being modified must be provided |
| `wait` | `bool` | No | Wait for the resource to reach the desired state. |
| `wait_timeout` | `int` | No | Timeout in seconds for wait operations. |

## Examples

```yaml
- name: Create a SSH Key Group
  nvidia.infra_controller.ssh_key_group:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: present
    name: "my-ssh-key-group"

- name: Delete a SSH Key Group
  nvidia.infra_controller.ssh_key_group:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: absent
    name: "my-ssh-key-group"
```
