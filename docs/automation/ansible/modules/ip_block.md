# nvidia.infra_controller.ip_block – manage IP Block

Create, update, and delete IP Blocks.

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `description` | `str` | No | Description of the IP Block |
| `id` | `str` | No | ID of the resource. Used for lookup. |
| `name` | `str` | No | Name of the IP Block. Required when creating a new resource. |
| `prefix` | `str` | No | Either IPv4 or IPv6 address. Required when creating a new resource. |
| `prefix_length` | `int` | No | Min: 1, Max: 32 for IPv4, 128 for IPv6. Required when creating a new resource. |
| `protocol_version` | `str` | No | Version of the ip network ipv4 or ipv6. Required when creating a new resource. Choices: `IPv4`, `IPv6`. |
| `routing_type` | `str` | No | Routing type of the IP Block. Required when creating a new resource. Choices: `Public`, `DatacenterOnly`. |
| `site_id` | `str` | No | ID of the site. Required when creating a new resource. |
| `state` | `str` | No | Desired state of the resource. Choices: `present`, `absent`. |
| `wait` | `bool` | No | Wait for the resource to reach the desired state. |
| `wait_timeout` | `int` | No | Timeout in seconds for wait operations. |

## Examples

```yaml
- name: Create a IP Block
  nvidia.infra_controller.ip_block:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: present
    name: "my-ip-block"
    prefix: "{{ prefix }}"
    prefix_length: "{{ prefix_length }}"
    protocol_version: "{{ protocol_version }}"
    routing_type: "{{ routing_type }}"
    site_id: "{{ site_id }}"

- name: Delete a IP Block
  nvidia.infra_controller.ip_block:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: absent
    name: "my-ip-block"
```
