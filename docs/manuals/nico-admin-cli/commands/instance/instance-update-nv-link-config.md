# `nico-admin-cli instance update-nv-link-config`

_[Tenant commands](../../tenant.md) › [instance](./instance.md) › **update-nv-link-config**_

## NAME

nico-admin-cli-instance-update-nv-link-config - Update instance NVLink
configuration

## SYNOPSIS

**nico-admin-cli instance update-nv-link-config**
\<**-i**\|**--instance**\> \<**--config**\> \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Update instance NVLink configuration

## OPTIONS

**-i**, **--instance** *\<INSTANCE\>*  
**--config** *\<NVLINK_JSON\>*  
NVLink configuration in JSON format

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
nico-admin-cli instance update-nv-link-config --instance 12345678-1234-5678-90ab-cdef01234567 --config '{"partition_id":"abcdef01-2345-6789-abcd-ef0123456789"}'
```

---

**See also:** [Tenant commands](../../tenant.md) · [CLI reference index](../../README.md)
