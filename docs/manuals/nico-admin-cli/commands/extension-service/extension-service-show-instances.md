# `nico-admin-cli extension-service show-instances`

*[Tenant commands](../../tenant.md) › [extension-service](./extension-service.md) › **show-instances***

## NAME

nico-admin-cli-extension-service-show-instances - Show instances using
an extension service

## SYNOPSIS

**nico-admin-cli extension-service show-instances**
\<**-i**\|**--service-id**\> \[**-v**\|**--version**\]
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Show instances using an extension service

## OPTIONS

**-i**, **--service-id** *\<SERVICE_ID\>*  
The extension service ID

**-v**, **--version** *\<VERSION\>*  
Version string to filter by (optional)

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
nico-admin-cli extension-service show-instances --service-id 12345678-1234-5678-90ab-cdef01234567
nico-admin-cli extension-service show-instances --service-id 12345678-1234-5678-90ab-cdef01234567 --version 1.0
```

---

**See also:** [Tenant commands](../../tenant.md) · [CLI reference index](../../index.md)
