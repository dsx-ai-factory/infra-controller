# nvidia.infra_controller.operating_system – manage Operating System

Retrieve all Operating Systems

## Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `allow_override` | `bool` | No | Indicates if the user data can be overridden at Instance creation time |
| `deactivation_note` | `str` | No | Optional deactivation note if OS is inactive |
| `description` | `str` | No | Optional description of the Operating System |
| `id` | `str` | No | ID of the resource. Used for lookup. |
| `image_auth_token` | `str` | No | Auth token to retrieve the image from image URL, required if imageAuthType is specified |
| `image_auth_type` | `str` | No | Authentication type for image URL, if needed, e.g., basic/bearer/token; required if imageAuthToken is specified |
| `image_disk` | `str` | No | Optional whole-disk target that will be overwritten with the image. Accepts `smallest`, `/dev/nvme<controller>n<namespace>`, `/dev/sd<letters>`, `/dev/vd<letters>`, or `/dev/disk/by-id/<identifier>`. `smallest` selects the smallest enumerated whole disk, preferring one with an EFI partition to break a size tie. Partition aliases ending in `-part<digits>` are rejected. When omitted, null, or empty on creation, the Site prefers a disk with an EFI partition, then falls back to `/dev/nvme0n1` or `/dev/sda`. |
| `image_sha` | `str` | No | SHA hash of the image file, required for image-based OS |
| `image_url` | `str` | No | Original URL from which the Operating System image can be retrieved; required for image-based OS. Cannot be specified if ipxeScript is specified |
| `infrastructure_provider_id` | `str` | No | Deprecated: Infrastructure Provider is now inferred from org membership. |
| `ipxe_script` | `str` | No | Deprecated: raw iPXE Operating Systems are superseded by Templated iPXE (ipxeTemplateId). iPXE script or URL, only applicable for iPXE-based OS. Cannot be specified if imageUrl is specified. |
| `ipxe_template_artifacts` | `list` | No | Artifacts (kernel, initrd, ISO, ...) for the iPXE OS definition (Templated iPXE only). |
| `ipxe_template_id` | `str` | No | ID of the iPXE template to use; identifies a Templated iPXE Operating System. Mutually exclusive with ipxeScript and imageUrl. |
| `ipxe_template_parameters` | `list` | No | Parameters passed to the iPXE template (Templated iPXE only). |
| `is_active` | `bool` | No | Indicates if the Operating System is active |
| `is_cloud_init` | `bool` | No | Deprecated and ignored: whether the Operating System is cloud-init based. Value now derived from `userData`. |
| `name` | `str` | No | Name of the Operating System |
| `phone_home_enabled` | `bool` | No | Indicates whether the Phone Home service should be enabled or disabled for Operating System |
| `root_fs_id` | `str` | No | Root filesystem UUID; this or `rootFsLabel` is required for image-based OS |
| `root_fs_label` | `str` | No | Root filesystem label; this or `rootFsId` is required for image-based OS |
| `site_ids` | `list` | No | Target Site for the Operating System. For image-based and Templated iPXE Operating Systems exactly one Site is required, even though this field is an array. The list is fixed at creation and cannot be changed on update. Not applicable to raw iPXE OS. |
| `state` | `str` | No | Desired state of the resource. Choices: `present`, `absent`. |
| `tenant_id` | `str` | No | Deprecated: Tenant is now inferred from org membership. |
| `user_data` | `str` | No | User data for the Operating System. Limited to 32768 bytes (32 KiB), measured on the effective value NICo stores rather than the text submitted. Operating System defaults are inherited first, and when phone-home is configured the document is re-serialized with a `phone_home` block added. Re-serialization normalizes indentation and can grow the document, so a request just under the limit may still be rejected. |
| `wait` | `bool` | No | Wait for the resource to reach the desired state. |
| `wait_timeout` | `int` | No | Timeout in seconds for wait operations. |

## Examples

```yaml
- name: Create a Operating System
  nvidia.infra_controller.operating_system:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: present
    name: "my-operating-system"

- name: Delete a Operating System
  nvidia.infra_controller.operating_system:
    api_url: "{{ api_url }}"
    api_token: "{{ api_token }}"
    org: "{{ org }}"
    state: absent
    name: "my-operating-system"
```
