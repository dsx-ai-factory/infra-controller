# `nico-admin-cli os-image show`

*[Tenant commands](../../tenant.md) › [os-image](./os-image.md) › **show***

## NAME

nico-admin-cli-os-image-show - Show one or more OS image entries in the
catalog.

## SYNOPSIS

**nico-admin-cli os-image show** \[**-i**\|**--id**\]
\[**-t**\|**--tenant-org-id**\] \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\]

## DESCRIPTION

Show one or more OS image entries in the catalog.

## OPTIONS

**-i**, **--id** *\<ID\>*  
uuid of the OS image to show.

**-t**, **--tenant-org-id** *\<TENANT_ORG_ID\>*  
Tenant organization identifier to filter OS images listing.

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
nico-admin-cli os-image show --id 12345678-1234-5678-90ab-cdef01234567
nico-admin-cli os-image show --tenant-org-id fds34511233a
nico-admin-cli os-image show
```

---

**See also:** [Tenant commands](../../tenant.md) · [CLI reference index](../../index.md)
