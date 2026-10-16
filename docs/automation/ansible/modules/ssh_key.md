# nvidia.infra_controller.ssh_key – manage SSH Key

Retrieve all SSH Keys

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `id` | `str` | No | ID of the resource. Used for lookup. |
| `name` | `str` | No | Name cannot match that name an existing SSH Key |
| `public_key` | `str` | No | Must be an SSH key of type: RSA, ECDSA or ED25519 |
| `ssh_key_group_id` | `str` | No | ID of the SSH Key Group this key should be attached to |
| `state` | `str` | No | Desired state of the resource. Choices: `present`, `absent`. |
| `wait` | `bool` | No | Wait for the resource to reach the desired state. |
| `wait_timeout` | `int` | No | Timeout in seconds for wait operations. |

## Examples

```yaml
- name: Create a SSH Key
  nvidia.infra_controller.ssh_key:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: present
    name: "my-ssh-key"

- name: Delete a SSH Key
  nvidia.infra_controller.ssh_key:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: absent
    name: "my-ssh-key"
```
