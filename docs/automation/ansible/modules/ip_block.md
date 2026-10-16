# nvidia.infra_controller.ip_block – manage IP Block

Retrieve all IP Blocks

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `description` | `str` | No | Description of the IP Block |
| `id` | `str` | No | ID of the resource. Used for lookup. |
| `name` | `str` | No | Name of the IP Block |
| `prefix` | `str` | No | Either IPv4 or IPv6 address |
| `prefix_length` | `int` | No | Min: 1, Max: 32 for IPv4, 128 for IPv6 |
| `protocol_version` | `str` | No | Version of the ip network ipv4 or ipv6 Choices: `IPv4`, `IPv6`. |
| `routing_type` | `str` | No | Routing type of the IP Block Choices: `Public`, `DatacenterOnly`. |
| `site_id` | `str` | No | ID of the site |
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

- name: Delete a IP Block
  nvidia.infra_controller.ip_block:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: absent
    name: "my-ip-block"
```
