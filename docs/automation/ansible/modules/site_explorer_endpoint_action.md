# nvidia.infra_controller.site_explorer_endpoint_action – manage Site Explorer

Trigger Site Explorer Endpoint Action

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `action` | `str` | No | Site Explorer endpoint action to trigger. Choices: `ClearError`, `ReExplore`. |
| `endpoint_ids` | `list` | No | BMC IP addresses to target when target is EndpointIds. |
| `id` | `str` | No | ID of the resource. Used for lookup. |
| `site_id` | `str` | No | ID of the Site whose explored endpoints are targeted. |
| `state` | `str` | No | Desired state of the resource. Choices: `present`. |
| `target` | `str` | No | Endpoint set to target. Choices: `All`, `EndpointIds`. |
| `wait` | `bool` | No | Wait for the resource to reach the desired state. |
| `wait_timeout` | `int` | No | Timeout in seconds for wait operations. |

## Examples

```yaml
- name: Create a Site Explorer
  nvidia.infra_controller.site_explorer_endpoint_action:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: present

```
