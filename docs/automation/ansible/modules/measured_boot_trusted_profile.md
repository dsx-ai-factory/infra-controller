# nvidia.infra_controller.measured_boot_trusted_profile – manage Measured Boot Trusted Profile

Retrieve All Measured Boot Trusted Profile Approvals

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `approval_type` | `str` | No | Whether the approval is consumed once or persists for future reports. Choices: `Oneshot`, `Persist`. |
| `comments` | `str` | No | Optional operator comments about the approval. |
| `id` | `str` | No | ID of the resource. Used for lookup. |
| `pcr_registers` | `str` | No | Optional comma-separated PCR register selector. All registers are used when omitted. |
| `profile_id` | `str` | No | ID of the measured-boot system profile. |
| `site_id` | `str` | No | ID of the Site where the approval applies. |
| `state` | `str` | No | Desired state of the resource. Choices: `present`, `absent`. |
| `wait` | `bool` | No | Wait for the resource to reach the desired state. |
| `wait_timeout` | `int` | No | Timeout in seconds for wait operations. |

## Examples

```yaml
- name: Create a Measured Boot Trusted Profile
  nvidia.infra_controller.measured_boot_trusted_profile:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: present

- name: Delete a Measured Boot Trusted Profile
  nvidia.infra_controller.measured_boot_trusted_profile:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: absent
    id: "{{ resource_id }}"
```
