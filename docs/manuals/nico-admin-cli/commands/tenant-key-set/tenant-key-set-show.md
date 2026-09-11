# `nico-admin-cli tenant-key-set show`

*[Tenant commands](../../tenant.md) › [tenant-key-set](./tenant-key-set.md) › **show***

## NAME

nico-admin-cli-tenant-key-set-show - Display Tenant KeySet information

## SYNOPSIS

**nico-admin-cli tenant-key-set show** \[**-t**\|**--tenant-org-id**\]
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\] \[*ID*\]

## DESCRIPTION

Display Tenant KeySet information

## OPTIONS

**-t**, **--tenant-org-id** *\<TENANT_ORG_ID\>*  
The Tenant Org ID to query

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

\[*ID*\] \[default: \]  
The Tenant KeySet ID in the format of \<tenant_org_id\>/\<keyset_id\> to
query, leave empty for all (default)

## Examples

```sh
nico-admin-cli tenant-key-set show
nico-admin-cli tenant-key-set show fds34511233a/87654321-4321-8765-cdef-0123456789ab
nico-admin-cli tenant-key-set show --tenant-org-id fds34511233a
```

---

**See also:** [Tenant commands](../../tenant.md) · [CLI reference index](../../index.md)
