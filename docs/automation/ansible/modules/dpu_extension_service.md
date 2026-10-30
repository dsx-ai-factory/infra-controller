# nvidia.infra_controller.dpu_extension_service – manage DPU Extension Service

Create, update, and delete DPU Extension Services.

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `credentials` | `dict` | No | Credentials to download resources specified in DPU Extension Service data; unsupported for DpfHelmChart |
| `data` | `str` | No | Deployment specification as a string, limited to 131072 UTF-8 bytes. Use a YAML/JSON Pod manifest for KubernetesPod or JSON-encoded Helm configuration for DpfHelmChart. The object schema documents the Helm structure; requests must still send a string. Required when creating a new resource. |
| `description` | `str` | No | Optional description for the DPU Extension Service |
| `dpu_target` | `str` | No | Required for DpfHelmChart services and unsupported for KubernetesPod services Choices: `Primary`, `AllActive`, `All`. |
| `id` | `str` | No | ID of the resource. Used for lookup. |
| `name` | `str` | No | Name for the DPU Extension Service. Must be unique for a given Tenant. Required when creating a new resource. |
| `observability` | `dict` | No | Observability configuration for the DPU Extension Service version; unsupported for DpfHelmChart |
| `service_type` | `str` | No | Type of the DPU Extension Service. Required when creating a new resource. Choices: `KubernetesPod`, `DpfHelmChart`. |
| `site_id` | `str` | No | ID for the Site the DPU Extension Service belongs to. Required when creating a new resource. |
| `state` | `str` | No | Desired state of the resource. Choices: `present`, `absent`. |
| `wait` | `bool` | No | Wait for the resource to reach the desired state. |
| `wait_timeout` | `int` | No | Timeout in seconds for wait operations. |

## Examples

```yaml
- name: Create a DPU Extension Service
  nvidia.infra_controller.dpu_extension_service:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: present
    name: "my-dpu-extension-service"
    data: "{{ data }}"
    service_type: "{{ service_type }}"
    site_id: "{{ site_id }}"

- name: Delete a DPU Extension Service
  nvidia.infra_controller.dpu_extension_service:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: absent
    name: "my-dpu-extension-service"
```
