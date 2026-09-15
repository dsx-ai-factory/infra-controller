# `nico-admin-cli os-image delete`

_[Tenant commands](../../tenant.md) › [os-image](./os-image.md) › **delete**_

## NAME

nico-admin-cli-os-image-delete - Delete an OS image entry that is not
used on any instances.

## SYNOPSIS

**nico-admin-cli os-image delete** \<**-i**\|**--id**\>
\<**-t**\|**--tenant-org-id**\> \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\]

## DESCRIPTION

Delete an OS image entry that is not used on any instances.

## OPTIONS

**-i**, **--id** *\<ID\>*  
uuid of the OS image to delete.

**-t**, **--tenant-org-id** *\<TENANT_ORG_ID\>*  
Tenant organization identifier of OS image to delete.

**--extended**  
Extended result output.

This is used by measured boot, where basic output contains just what you
probably care about, and "extended" output also dumps out all the
internal UUIDs that are used to associate instances.

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
nico-admin-cli os-image delete --id 12345678-1234-5678-90ab-cdef01234567 --tenant-org-id fds34511233a
```

---

**See also:** [Tenant commands](../../tenant.md) · [CLI reference index](../../README.md)
