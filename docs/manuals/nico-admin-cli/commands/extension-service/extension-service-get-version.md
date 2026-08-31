# `nico-admin-cli extension-service get-version`

*[Tenant commands](../../tenant.md) › [extension-service](./extension-service.md) › **get-version***

## NAME

nico-admin-cli-extension-service-get-version - Get extension service
version information

## SYNOPSIS

**nico-admin-cli extension-service get-version**
\<**-i**\|**--service-id**\> \[**-v**\|**--versions**\]
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Get extension service version information

## OPTIONS

**-i**, **--service-id** *\<SERVICE_ID\>*  
The extension service ID

**-v**, **--versions** *\<VERSIONS\>*  
Version strings to get (optional, leave empty to get all versions)

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
nico-admin-cli extension-service get-version --service-id 12345678-1234-5678-90ab-cdef01234567
nico-admin-cli extension-service get-version --service-id 12345678-1234-5678-90ab-cdef01234567 --versions 1.0,1.1
```

---

**See also:** [Tenant commands](../../tenant.md) · [CLI reference index](../../index.md)
