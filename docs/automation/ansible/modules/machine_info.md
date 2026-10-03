# nvidia.infra_controller.machine_info – query Machine

Retrieve all Machines

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `capability_name` | `str` | No | Filter Machines by Capability Name. Can be specified multiple times to filter on more than one Capability Name. |
| `capability_type` | `str` | No | Filter Machines by Capability Type |
| `has_instance` | `bool` | No | Filter Machines that are assigned to an Instance. siteId must be specified when using this param. |
| `has_instance_type` | `bool` | No | Filter Machines that have been assigned an Instance Type. |
| `hw_sku_device_type` | `str` | No | Filter Machines by hardware SKU Device Type. Example values: "gpu", "cpu", "storage", "cache" |
| `id` | `str` | No | Filter Machines by ID. Can be specified multiple times to filter on more than one ID. |
| `include_metadata` | `bool` | No | Include Machine metadata e.g. BMC, DPU, GPU and Interface data. Can only be requested by Provider. |
| `instance_type_id` | `str` | No | Filter Machines by Instance Type ID. Can be specified multiple times to filter on more than one Instance Type ID. |
| `is_missing_on_site` | `bool` | No | Filter Machines that are missing on Site. |
| `query` | `str` | No | Provide query to search for matches. Input will be matched against Machine ID, vendor, product name, hostname and status |
| `site_id` | `str` | No | Filter Machines by Site ID |
| `status` | `str` | No | Filter Machines by Status. Can be specified multiple times to filter on more than one Status. |
| `tenant_id` | `str` | No | Filter Machines by ID of tenant of assigned instance. Can be specified multiple times to filter on more than one Tenant ID. |

## Examples

```yaml
- name: List all Machine resources
  nvidia.infra_controller.machine_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"

- name: Get a specific Machine by ID
  nvidia.infra_controller.machine_info:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    id: "{{ resource_id }}"
```
