# nvidia.infra_controller.credential_rotation_info – query Credential Rotation

Get Credential Rotation Status

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `credential_type` | `str` | Yes | Credential family to report. Choices: `BMC`, `HostUEFI`, `DPUUEFI`, `NVOS`, `LockdownIKM`. |
| `device_mac` | `str` | No | Report only this device's convergence, matched by MAC. |
| `site_id` | `str` | Yes | ID of the Site to query. |

## Examples

```yaml
- name: List all Credential Rotation resources
  nvidia.infra_controller.credential_rotation_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    credential_type: "{{ credential_type }}"
    site_id: "{{ site_id }}"
```
