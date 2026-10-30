# nvidia.infra_controller.measured_boot_trusted_profile – manage Measured Boot Trusted Profile

Create and delete Measured Boot Trusted Profile Approvals.

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `approval_type` | `str` | No | Whether the approval is consumed once or persists for future reports. Required when creating a new resource. Choices: `Oneshot`, `Persist`. |
| `comments` | `str` | No | Optional operator comments about the approval. |
| `id` | `str` | No | ID of the resource. Used for lookup. |
| `pcr_registers` | `str` | No | Optional comma-separated PCR register selector. All registers are used when omitted. |
| `profile_id` | `str` | No | ID of the measured-boot system profile. Required when creating a new resource. |
| `selector` | `str` | No | Whether id identifies the approval or the system profile. Required when deleting the resource. Choices: `ApprovalId`, `ProfileId`. |
| `site_id` | `str` | No | ID of the Site where the approval applies. Required when creating a new resource. Required when deleting the resource. |
| `state` | `str` | No | Desired state of the resource. Choices: `present`, `absent`. |

## Examples

```yaml
- name: Create a Measured Boot Trusted Profile
  nvidia.infra_controller.measured_boot_trusted_profile:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: present
    approval_type: "{{ approval_type }}"
    profile_id: "{{ profile_id }}"
    site_id: "{{ site_id }}"

- name: Delete a Measured Boot Trusted Profile
  nvidia.infra_controller.measured_boot_trusted_profile:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: absent
    id: "{{ resource_id }}"
    selector: "{{ selector }}"
    site_id: "{{ site_id }}"
```
