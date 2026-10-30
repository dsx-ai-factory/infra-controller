# nvidia.infra_controller.machine_spectrumx_multiplane – manage Machine

Update Machine Spectrum-X multiplane status.

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `acknowledge_attached_instance` | `bool` | No | Must be true when an Instance is attached to the Machine. Acknowledges that maintenance changes may disrupt the Instance's Tenant workload. |
| `all_planes` | `bool` | No | Select all eight planes on each selected rail. If supplied, must be true. Supply exactly one of plane, allPlanes, or planeRange. |
| `all_rails` | `bool` | No | Select all four rails. If supplied, must be true. Supply exactly one of rail, allRails, or railRange. |
| `id` | `str` | No | ID of the resource. Used for lookup. |
| `machine_id` | `str` | Yes | ID path parameter: machine_id. |
| `maintenance_mode` | `bool` | Yes | True disables the selected links; false enables them. |
| `plane` | `int` | No | Single plane to update on each selected rail, from 0 through 7. Supply exactly one of plane, allPlanes, or planeRange. |
| `plane_range` | `dict` | No | Inclusive ascending range of planes to update on each selected rail. Both endpoints must be from 0 through 7. Supply exactly one of plane, allPlanes, or planeRange. |
| `rail` | `int` | No | Single rail to update, from 0 through 3. Supply exactly one of rail, allRails, or railRange. |
| `rail_range` | `dict` | No | Inclusive ascending range of rails to update. Both endpoints must be from 0 through 3. Supply exactly one of rail, allRails, or railRange. |
| `reference` | `str` | No | Omit or send null to preserve it; send an empty string to clear it. |
| `state` | `str` | No | Desired state of the resource. Choices: `present`. |

## Examples

```yaml
- name: Update Machine Spectrum-X multiplane maintenance
  nvidia.infra_controller.machine_spectrumx_multiplane:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: present
    machine_id: "{{ machine_id }}"
    maintenance_mode: "{{ maintenance_mode }}"
    all_rails: true
    all_planes: true
```
