# Image-Based Operating Systems

NICo can install a disk image directly onto a host's local boot disk. The
operating-system definition controls how NICo selects that disk and how it
identifies the root, boot, and EFI filesystems after imaging.

<Warning>
Image installation overwrites the complete selected disk.
</Warning>

## Configuring the Target Disk

The Core API and `nico-admin-cli` call this field `boot_disk`; the REST API
calls it `imageDisk`. Set the value according to the following table:

| Value | Behavior |
|---|---|
| `smallest` | Selects the smallest enumerated whole disk. If multiple disks have the same smallest size, NICo prefers one containing an existing EFI System Partition; otherwise it makes a deterministic name-based choice. |
| `/dev/nvme<controller>n<namespace>` | Selects an NVMe namespace, for example `/dev/nvme0n1`. |
| `/dev/sd<letters>` | Selects a SCSI-style whole disk, for example `/dev/sda` or `/dev/sdaa`. |
| `/dev/disk/by-id/<identifier>` | Resolves a stable Linux disk identifier to its backing device. The identifier cannot contain a slash or whitespace. |
| Omitted or empty on creation | Prefers the first deterministically ordered disk containing an EFI System Partition, then `/dev/nvme0n1`, then `/dev/sda`. |

An explicit device path is resolved before use. It must exist, be a block
device, and resolve to an `lsblk` type of `disk`; partitions and other block
device types are rejected.

The value is part of an operating-system definition, not a machine-specific
override. Prefer `/dev/disk/by-id/` only when that identifier exists consistently
on every eligible target. Use `smallest` when the intended boot disk is reliably
the smallest local whole disk across the fleet.

## Interface Behavior

The available configuration surfaces are:

| Surface | Create | Update |
|---|---|---|
| REST API | Set `imageDisk` on `OperatingSystemCreateRequest`. | Set `imageDisk` on `OperatingSystemUpdateRequest`. Omission or JSON `null` preserves the current value; an explicit empty string clears it and restores automatic disk selection. |
| `nico-admin-cli` | Set `--boot-disk` on `os-image create`. | The admin CLI does not currently expose `boot_disk` on `os-image update`; use the REST API to change an existing definition. |

For example, select the smallest disk when creating a Core OS image entry:

```bash
nico-admin-cli os-image create \
  --id 12345678-1234-5678-90ab-cdef01234567 \
  --url https://cloud-images.ubuntu.com/releases/jammy/release-20260826/ubuntu-22.04-server-cloudimg-amd64.img \
  --digest sha256:<image-sha256> \
  --tenant-org-id <tenant-org-id> \
  --boot-disk smallest
```

Or create a REST operating-system definition with a stable disk identifier:

```json
{
  "name": "ubuntu-22.04-image",
  "description": "Ubuntu 22.04 image-based installation",
  "tenantId": "7306ff7d-f2b4-472f-ba1c-3ec9c24967be",
  "siteIds": ["497f6eca-6276-4993-bfeb-53cbbbba6f08"],
  "imageUrl": "https://cloud-images.ubuntu.com/releases/jammy/release-20260826/ubuntu-22.04-server-cloudimg-amd64.img",
  "imageSha": "<image-sha256>",
  "imageDisk": "/dev/disk/by-id/nvme-Dell_BOSS-N1_VNOWW56VFCV0055601UT",
  "rootFsLabel": "cloudimg-rootfs"
}
```

<Note>
`boot_disk` is not applicable when `create_volume` is enabled for a block
storage source volume. Do not combine `--boot-disk` with `--create-volume`.
</Note>

## Filesystem Identity Checks

NICo uses the configured filesystem identifiers to locate filesystems after
the image is written:

- `rootfs_id` or `rootFsId`: root filesystem UUID
- `rootfs_label` or `rootFsLabel`: root filesystem label
- `bootfs_id` (`--bootfs-id`): optional `/boot` UUID
- `efifs_id` (`--efifs-id`): optional EFI filesystem UUID

When an OS image is created through Core—for example, with
`nico-admin-cli os-image create`—`--rootfs-id` and `--rootfs-label` are
optional. If both are omitted, disk imaging uses the default root filesystem
label `cloudimg-rootfs`.

A REST API request that creates an image-based Operating System must
provide exactly one of `rootFsId` or `rootFsLabel`. The API rejects requests that
provide both fields or neither field.

Before overwriting the target, NICo checks these UUIDs and labels against the
other physical disks. It cancels the operation if a configured identifier already resolves to
a filesystem on another disk. This prevents an old or duplicate filesystem
identity from silently redirecting the installation away from the selected
target.

After imaging, every configured identifier must resolve to exactly one device,
and that device must belong exclusively to the selected disk. Missing,
duplicate, or cross-disk matches fail the installation.

## EFI Boot Entry Selection

For UEFI systems, NICo selects the architecture-specific shim:

- `shimx64.efi` on x86-64
- `shimaa64.efi` on Arm64

When a distribution name is known, NICo prefers a matching distribution
directory. It then looks for the architecture-specific `BOOTX64.CSV` or
`BOOTAA64.CSV`, followed by `BOOT.CSV`. NICo accepts a CSV entry only when its
first field names the selected shim and its label is nonempty. Without a
distribution match, NICo uses the first valid shim/CSV pair and then a
deterministically sorted shim as the final fallback.

NICo removes existing firmware entries with the selected label before creating
the new entry. If NICo cannot inspect, remove, or create the entry, disk imaging fails.

## Troubleshooting

Inspect the host's disk-imaging journal when provisioning fails:

```bash
journalctl -u disk-imaging.service -e --no-pager
lsblk -o NAME,PATH,SIZE,TYPE,FSTYPE,LABEL,UUID,PARTTYPE
```

Common causes include:

| Failure | Check |
|---|---|
| Explicit target is rejected | Confirm the path resolves to an existing whole-disk block device rather than a partition. |
| `smallest` selects an unexpected disk | Compare whole-disk sizes and check which equal-sized candidates contain an EFI System Partition. |
| Filesystem identity is ambiguous | Look for duplicate root, boot, or EFI UUIDs and labels on other physical disks. |
| Root filesystem cannot be found | For Core/admin entries, verify the supplied root filesystem UUID or label; if both root filesystem flags were omitted, verify that the image uses the label `cloudimg-rootfs`. For REST entries, verify that the image contains the `rootFsId` UUID or `rootFsLabel` supplied during creation. |
| EFI entry creation fails | Confirm the image contains the architecture-appropriate shim and a valid matching CSV label, and inspect firmware-variable access. |

Because the selected disk is overwritten, resolve selection or identifier
ambiguity in a non-production environment before retrying on a machine that
contains data that must be preserved.
