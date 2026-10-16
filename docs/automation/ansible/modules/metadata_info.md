# nvidia.infra_controller.metadata_info – query Metadata

Retrieve metadata about the API server

## Parameters

No additional parameters beyond the [authentication fragment](../getting-started.md).

## Examples

```yaml
- name: List all Metadata resources
  nvidia.infra_controller.metadata_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
```
