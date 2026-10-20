# nvidia.infra_controller.credential_bmc – manage BMC Credential

Create Or Update BMC Credential

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `id` | `str` | No | ID of the resource. Used for lookup. |
| `kind` | `str` | No | Which BMC credential to store. Choices: `SiteWideRoot`, `BMCRoot`. |
| `mac_address` | `str` | No | BMC MAC address. Required for kind BMCRoot, ignored for SiteWideRoot. |
| `password` | `str` | No | Credential password. |
| `site_id` | `str` | No | ID of the Site where the credential is stored. |
| `state` | `str` | No | Desired state of the resource. Choices: `present`. |
| `username` | `str` | No | Optional username; Core defaults to "root" for BMCRoot when omitted. |
| `wait` | `bool` | No | Wait for the resource to reach the desired state. |
| `wait_timeout` | `int` | No | Timeout in seconds for wait operations. |

## Examples

```yaml
- name: Create a BMC Credential
  nvidia.infra_controller.credential_bmc:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: present

```
