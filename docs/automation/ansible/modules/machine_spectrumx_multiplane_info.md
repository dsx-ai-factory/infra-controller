# nvidia.infra_controller.machine_spectrumx_multiplane_info – query Machine

Retrieve Machine Spectrum-X multiplane status

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `machine_id` | `str` | Yes | ID path parameter: machine_id. |

## Examples

```yaml
- name: List all Machine resources
  nvidia.infra_controller.machine_spectrumx_multiplane_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    machine_id: "{{ machine_id }}"
```
