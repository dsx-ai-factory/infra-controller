# `nico-admin-cli extension-service show`

_[Tenant commands](../../tenant.md) › [extension-service](./extension-service.md) › **show**_

## NAME

nico-admin-cli-extension-service-show - Show extension service
information

## SYNOPSIS

**nico-admin-cli extension-service show** \[**-i**\|**--id**\]
\[**-t**\|**--type**\] \[**-n**\|**--name**\]
\[**-o**\|**--tenant-organization-id**\] \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Show extension service information

## OPTIONS

**-i**, **--id** *\<ID\>*  
The extension service ID to show (leave empty to show all)

**-t**, **--type** *\<SERVICE_TYPE\>*  
Filter by service type (optional)\

\
*Possible values:*

- kubernetes-pod

**-n**, **--name** *\<SERVICE_NAME\>*  
Filter by service name (optional)

**-o**, **--tenant-organization-id** *\<TENANT_ORGANIZATION_ID\>*  
Filter by tenant organization ID (optional)

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
nico-admin-cli extension-service show
nico-admin-cli extension-service show --id 12345678-1234-5678-90ab-cdef01234567
nico-admin-cli extension-service show --type kubernetes-pod
nico-admin-cli extension-service show --name my-service
nico-admin-cli extension-service show --tenant-organization-id fds34511233a
```

---

**See also:** [Tenant commands](../../tenant.md) · [CLI reference index](../../README.md)
