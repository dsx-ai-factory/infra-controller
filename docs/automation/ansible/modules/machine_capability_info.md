# nvidia.infra_controller.machine_capability_info – query Machine Capability

Manage Machine Capability resources.

## Parameters

No additional parameters beyond the [authentication fragment](../getting-started.md).

## Examples

```yaml
- name: List all Machine Capability resources
  nvidia.infra_controller.machine_capability_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
```
