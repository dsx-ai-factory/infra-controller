# `nico-admin-cli os-image update`

_[Tenant commands](../../tenant.md) › [os-image](./os-image.md) › **update**_

## NAME

nico-admin-cli-os-image-update - Update the authentication details or
name and description for an OS image.

## SYNOPSIS

**nico-admin-cli os-image update** \<**-i**\|**--id**\>
\[**-n**\|**--name**\] \[**-d**\|**--description**\]
\[**-y**\|**--auth-type**\] \[**-p**\|**--auth-token**\]
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Update the authentication details or name and description for an OS
image.

## OPTIONS

**-i**, **--id** *\<ID\>*  
uuid of the OS image to update.

**-n**, **--name** *\<NAME\>*  
Optional, name of the OS image entry.

**-d**, **--description** *\<DESCRIPTION\>*  
Optional, description of the OS image entry.

**-y**, **--auth-type** *\<AUTH_TYPE\>*  
Optional, Authentication type, usually Bearer.

**-p**, **--auth-token** *\<AUTH_TOKEN\>*  
Optional, Authentication token, usually in base64.

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
nico-admin-cli os-image update --id 12345678-1234-5678-90ab-cdef01234567 --name ubuntu-22.04 --description "Ubuntu 22.04 base"
nico-admin-cli os-image update --id 12345678-1234-5678-90ab-cdef01234567 --auth-type Bearer --auth-token <token>
```

---

**See also:** [Tenant commands](../../tenant.md) · [CLI reference index](../../README.md)
