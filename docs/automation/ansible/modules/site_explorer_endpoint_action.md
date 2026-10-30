# nvidia.infra_controller.site_explorer_endpoint_action – manage Site Explorer

Trigger Site Explorer Endpoint Action

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `action` | `str` | Yes | Site Explorer endpoint action to trigger. Choices: `ClearError`, `ReExplore`. |
| `endpoint_ids` | `list` | No | BMC IP addresses to target when target is EndpointIds. |
| `id` | `str` | No | ID of the resource. Used for lookup. |
| `site_id` | `str` | Yes | ID of the Site whose explored endpoints are targeted. |
| `state` | `str` | No | Desired state of the resource. Choices: `present`. |
| `target` | `str` | Yes | Endpoint set to target. Choices: `All`, `EndpointIds`. |

## Examples

```yaml
- name: Trigger Site Explorer Endpoint Action
  nvidia.infra_controller.site_explorer_endpoint_action:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: present
    action: "{{ action }}"
    site_id: "{{ site_id }}"
    target: "{{ target }}"

```
