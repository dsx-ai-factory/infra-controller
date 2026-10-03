# nvidia.infra_controller.site – manage Site

Retrieve all Sites

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `capabilities` | `dict` | No | Modify Site capabilities. Can only be updated by Provider. Partial update allowed, only specify capabilities that should be updated. |
| `contact` | `dict` | No | Site contact information |
| `description` | `str` | No | Description for the Site |
| `id` | `str` | No | ID of the resource. Used for lookup. |
| `is_serial_console_enabled` | `bool` | No | Enable/disable Serial Console. Can only be updated by Provider. Modifying this attribute has no actual effect on SOL. It will be removed in a future API version. |
| `is_serial_console_ssh_keys_enabled` | `bool` | No | Enable/disable Serial Console access using SSH Keys. Previously updateable only by Tenants, modifying this value is no longer supported, update SSH Key Groups to remove Site instead. |
| `location` | `dict` | No | Site location information |
| `name` | `str` | No | Name for the Site |
| `renew_registration_token` | `bool` | No | Set to true to issue a new registration token. Can only be updated by Provider |
| `serial_console_hostname` | `str` | No | Hostname to reach Serial Console for the Site |
| `serial_console_idle_timeout` | `int` | No | Maximum idle time in seconds before Serial Console is disconnected. Can only be updated by Provider. Modifying this attribute has no actual effect on SOL. It will be removed in a future API version. |
| `serial_console_max_session_length` | `int` | No | Maximum length of Serial Console session in seconds. Can only be updated by Provider. Modifying this attribute has no actual effect on SOL. It will be removed in a future API version. |
| `state` | `str` | No | Desired state of the resource. Choices: `present`, `absent`. |
| `wait` | `bool` | No | Wait for the resource to reach the desired state. |
| `wait_timeout` | `int` | No | Timeout in seconds for wait operations. |

## Examples

```yaml
- name: Create a Site
  nvidia.infra_controller.site:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: present
    name: "my-site"

- name: Delete a Site
  nvidia.infra_controller.site:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: absent
    name: "my-site"
```
