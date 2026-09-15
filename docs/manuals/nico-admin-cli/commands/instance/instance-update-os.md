# `nico-admin-cli instance update-os`

_[Tenant commands](../../tenant.md) › [instance](./instance.md) › **update-os**_

## NAME

nico-admin-cli-instance-update-os - Update instance OS

## SYNOPSIS

**nico-admin-cli instance update-os** \<**-i**\|**--instance**\>
\<**--os**\> \[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Update instance OS

## OPTIONS

**-i**, **--instance** *\<INSTANCE\>*  
**--os** *\<OS_JSON\>*  
OS definition in JSON format

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
nico-admin-cli instance update-os --instance 12345678-1234-5678-90ab-cdef01234567 --os '{"os_image_id":"abcdef01-2345-6789-abcd-ef0123456789"}'
```

---

**See also:** [Tenant commands](../../tenant.md) · [CLI reference index](../../README.md)
