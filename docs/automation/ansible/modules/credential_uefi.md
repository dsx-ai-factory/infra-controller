# nvidia.infra_controller.credential_uefi – manage UEFI Credential

Create UEFI Credential

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `id` | `str` | No | ID of the resource. Used for lookup. |
| `kind` | `str` | No | Which site-default UEFI credential to create. Choices: `Host`, `DPU`. |
| `password` | `str` | No | Credential password. |
| `site_id` | `str` | No | ID of the Site where the credential is stored. |
| `state` | `str` | No | Desired state of the resource. Choices: `present`. |
| `wait` | `bool` | No | Wait for the resource to reach the desired state. |
| `wait_timeout` | `int` | No | Timeout in seconds for wait operations. |

## Examples

```yaml
- name: Create a UEFI Credential
  nvidia.infra_controller.credential_uefi:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: present

```
