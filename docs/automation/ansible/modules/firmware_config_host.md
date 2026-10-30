# nvidia.infra_controller.firmware_config_host – manage Host Firmware Config

Create or Update Host Firmware Config

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `components` | `list` | No | Component firmware entries to create or merge. Required when creating a new resource. |
| `explicit_start_needed` | `bool` | No | Optional. When `true`, host firmware updates for this vendor/model require an explicit start. Omitted on update leaves the stored value unchanged. |
| `id` | `str` | No | ID of the resource. Used for lookup. |
| `model` | `str` | No | Hardware model for the configuration (for example `DGXH100`). Required when creating a new resource. Required when deleting the resource. |
| `ordering` | `list` | No | Update order for configured components. Required on first create. Must include every configured component and must be updated when adding a new component on subsequent PUTs. |
| `site_id` | `str` | No | ID of the Site where the host firmware config is stored. Required when creating a new resource. Required when deleting the resource. |
| `state` | `str` | No | Desired state of the resource. Choices: `present`, `absent`. |
| `vendor` | `str` | No | Hardware vendor for the configuration (for example `Nvidia`, `Dell`). Required when creating a new resource. Required when deleting the resource. |

## Examples

```yaml
- name: Create a Host Firmware Config
  nvidia.infra_controller.firmware_config_host:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: present
    components: "{{ components }}"
    model: "{{ model }}"
    site_id: "{{ site_id }}"
    vendor: "{{ vendor }}"
    ordering: "{{ ordering }}"

- name: Delete a Host Firmware Config
  nvidia.infra_controller.firmware_config_host:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: absent
    model: "{{ model }}"
    site_id: "{{ site_id }}"
    vendor: "{{ vendor }}"
```
