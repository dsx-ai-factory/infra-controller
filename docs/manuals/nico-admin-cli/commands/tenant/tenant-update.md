# `nico-admin-cli tenant update`

*[Tenant commands](../../tenant.md) › [tenant](./tenant.md) › **update***

## NAME

nico-admin-cli-tenant-update - Update an existing tenant

## SYNOPSIS

**nico-admin-cli tenant update** \[**-p**\|**--routing-profile-type**\]
\[**-v**\|**--version**\] \[**-n**\|**--name**\] \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \[*TENANT_ORG*\]

## DESCRIPTION

Update an existing tenant

## OPTIONS

**-p**, **--routing-profile-type** *\<ROUTING_PROFILE_TYPE\>*  
Optional, routing profile name to apply to the tenant

**-v**, **--version** *\<VERSION\>*  
Optional, version to use for comparison when performing the update,
which will be rejected if the actual version of the record does not
match the value of this parameter

**-n**, **--name** *\<NAME\>*  
Organization name of the tenant

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

\[*TENANT_ORG*\]  
Tenant org ID to update

## Examples

```sh
nico-admin-cli tenant update fds34511233a --name "Acme Corp"
nico-admin-cli tenant update fds34511233a --routing-profile-type default
nico-admin-cli tenant update fds34511233a --name "Acme Corp" --version 7
```

---

**See also:** [Tenant commands](../../tenant.md) · [CLI reference index](../../index.md)
