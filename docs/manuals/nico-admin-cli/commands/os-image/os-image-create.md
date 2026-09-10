# `nico-admin-cli os-image create`

_[Tenant commands](../../tenant.md) › [os-image](./os-image.md) › **create**_

## NAME

nico-admin-cli-os-image-create - Create an OS image entry in the OS
catalog for a tenant.

## SYNOPSIS

**nico-admin-cli os-image create** \<**-i**\|**--id**\>
\<**-u**\|**--url**\> \<**-m**\|**--digest**\>
\<**-t**\|**--tenant-org-id**\> \[**-v**\|**--create-volume**\]
\[**-s**\|**--capacity**\] \[**-n**\|**--name**\]
\[**-d**\|**--description**\] \[**-y**\|**--auth-type**\]
\[**-p**\|**--auth-token**\] \[**-f**\|**--rootfs-id**\]
\[**-l**\|**--rootfs-label**\] \[**-b**\|**--boot-disk**\]
\[**--bootfs-id**\] \[**--extended**\] \[**--efifs-id**\]
\[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Create an OS image entry in the OS catalog for a tenant.

## OPTIONS

**-i**, **--id** *\<ID\>*  
uuid of the OS image to create.

**-u**, **--url** *\<URL\>*  
url of the OS image qcow file.

**-m**, **--digest** *\<DIGEST\>*  
Digest of the OS image file, typically a SHA-256.

**-t**, **--tenant-org-id** *\<TENANT_ORG_ID\>*  
Tenant organization identifier for the OS catalog to create this in.

**-v**, **--create-volume** *\<CREATE_VOLUME\>*  
Create a source volume for block storage use.\

\
*Possible values:*

- true

- false

**-s**, **--capacity** *\<CAPACITY\>*  
Size of the OS image source volume to create.

**-n**, **--name** *\<NAME\>*  
Name of the OS image entry.

**-d**, **--description** *\<DESCRIPTION\>*  
Description of the OS image entry.

**-y**, **--auth-type** *\<AUTH_TYPE\>*  
Authentication type, usually Bearer.

**-p**, **--auth-token** *\<AUTH_TOKEN\>*  
Authentication token, usually in base64.

**-f**, **--rootfs-id** *\<ROOTFS_ID\>*  
uuid of the root filesystem of the OS image.

**-l**, **--rootfs-label** *\<ROOTFS_LABEL\>*  
Label of the root filesystem of the OS image.

**-b**, **--boot-disk** *\<BOOT_DISK\>*  
Whole-disk target that the image overwrites. Accepts smallest,
/dev/nvme\<controller\>n\<namespace\>, /dev/sd\<letters\>, or
/dev/disk/by-id/\<identifier\>. If omitted or empty, selection prefers a
disk with an EFI partition, then /dev/nvme0n1 or /dev/sda.

**--bootfs-id** *\<BOOTFS_ID\>*  
UUID of the image boot filesystem (/boot)

**--extended**  
Extended result output.

This is used by measured boot, where basic output contains just what you
probably care about, and "extended" output also dumps out all the
internal UUIDs that are used to associate instances.

**--efifs-id** *\<EFIFS_ID\>*  
UUID of the image EFI filesystem (/boot/efi)

**--sort-by** *\<SORT_BY\>* \[default: primary-id\]  
Sort output by specified field\

\
*Possible values:*

- primary-id: Sort by the primary ID

- state: Sort by state

**-h**, **--help**  
Print help (see a summary with -h)

## Examples

```sh
nico-admin-cli os-image create --id 12345678-1234-5678-90ab-cdef01234567 --url https://images.example.com/ubuntu.qcow2 --digest sha256:2c26b46b68ffc68ff99b453c1d30413413422d706483bfa0f98a5e886266e7ae --tenant-org-id fds34511233a
nico-admin-cli os-image create --id 12345678-1234-5678-90ab-cdef01234567 --url https://images.example.com/ubuntu.qcow2 --digest sha256:2c26b46b68ffc68ff99b453c1d30413413422d706483bfa0f98a5e886266e7ae --tenant-org-id fds34511233a --name ubuntu-22.04 --description "Ubuntu 22.04 base" --auth-type Bearer --auth-token ZXhhbXBsZS1pbWFnZS10b2tlbg==
nico-admin-cli os-image create --id 12345678-1234-5678-90ab-cdef01234567 --url https://images.example.com/ubuntu.qcow2 --digest sha256:2c26b46b68ffc68ff99b453c1d30413413422d706483bfa0f98a5e886266e7ae --tenant-org-id fds34511233a --boot-disk smallest
nico-admin-cli os-image create --id 12345678-1234-5678-90ab-cdef01234567 --url https://images.example.com/ubuntu.qcow2 --digest sha256:2c26b46b68ffc68ff99b453c1d30413413422d706483bfa0f98a5e886266e7ae --tenant-org-id fds34511233a --boot-disk /dev/disk/by-id/nvme-Dell_BOSS-N1_VNOWW56VFCV0055601UT
```

---

**See also:** [Tenant commands](../../tenant.md) · [CLI reference index](../../README.md)
