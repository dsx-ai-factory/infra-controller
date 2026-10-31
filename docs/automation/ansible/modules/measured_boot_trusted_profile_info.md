# nvidia.infra_controller.measured_boot_trusted_profile_info – query Measured Boot Trusted Profile

Retrieve All Measured Boot Trusted Profile Approvals

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `site_id` | `str` | Yes | ID of the Site |

## Examples

```yaml
- name: List all Measured Boot Trusted Profile resources
  nvidia.infra_controller.measured_boot_trusted_profile_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    site_id: "{{ site_id }}"
```
