# nvidia.infra_controller.credential_rotation – manage Credential Rotation

Get Credential Rotation Status

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `credential_type` | `str` | No | Credential family to rotate. Choices: `BMC`, `HostUEFI`, `DPUUEFI`, `NVOS`, `LockdownIKM`. |
| `id` | `str` | No | ID of the resource. Used for lookup. |
| `password` | `str` | No | Explicit rotate-to password. When omitted, a strong password is auto-generated. Never returned. |
| `reason` | `str` | No | Free-form operator note recorded with the rotation. Must not contain secrets. |
| `site_id` | `str` | No | ID of the Site whose credential family is rotated. |
| `state` | `str` | No | Desired state of the resource. Choices: `present`. |
| `wait` | `bool` | No | Wait for the resource to reach the desired state. |
| `wait_timeout` | `int` | No | Timeout in seconds for wait operations. |

## Examples

```yaml
- name: Create a Credential Rotation
  nvidia.infra_controller.credential_rotation:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: present

```
