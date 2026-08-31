# `nico-admin-cli extension-service delete`

*[Tenant commands](../../tenant.md) › [extension-service](./extension-service.md) › **delete***

## NAME

nico-admin-cli-extension-service-delete - Delete an extension service

## SYNOPSIS

**nico-admin-cli extension-service delete** \<**-i**\|**--id**\>
\[**-v**\|**--versions**\] \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\]

## DESCRIPTION

Delete an extension service

## OPTIONS

**-i**, **--id** *\<SERVICE_ID\>*  
The extension service ID to delete

**-v**, **--versions** *\<VERSIONS\>*  
Version strings to delete (optional, leave empty to keep all versions)

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
nico-admin-cli extension-service delete --id 12345678-1234-5678-90ab-cdef01234567
nico-admin-cli extension-service delete --id 12345678-1234-5678-90ab-cdef01234567 --versions 1.0,1.1
```

---

**See also:** [Tenant commands](../../tenant.md) · [CLI reference index](../../index.md)
