# nvidia.infra_controller.credential_rotation – manage Credential Rotation

Rotate Credential.

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `credential_type` | `str` | Yes | Credential family to rotate. Choices: `BMC`, `HostUEFI`, `DPUUEFI`, `NVOS`, `LockdownIKM`. |
| `id` | `str` | No | ID of the resource. Used for lookup. |
| `password` | `str` | No | Explicit rotate-to password. When omitted, a strong password is auto-generated. Never returned. |
| `reason` | `str` | No | Free-form operator note recorded with the rotation. Must not contain secrets. |
| `site_id` | `str` | Yes | ID of the Site whose credential family is rotated. |
| `state` | `str` | No | Desired state of the resource. Choices: `present`. |

## Examples

```yaml
- name: Rotate Credential
  nvidia.infra_controller.credential_rotation:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: present
    credential_type: "{{ credential_type }}"
    site_id: "{{ site_id }}"

```
