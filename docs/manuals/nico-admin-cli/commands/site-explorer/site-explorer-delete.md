# `nico-admin-cli site-explorer delete`

_[Tenant commands](../../tenant.md) › [site-explorer](./site-explorer.md) › **delete**_

## NAME

nico-admin-cli-site-explorer-delete - Delete an explored endpoint from
the database.

## SYNOPSIS

**nico-admin-cli site-explorer delete** \<**--address**\>
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Delete an explored endpoint from the database.

## OPTIONS

**--address** *\<ADDRESS\>*  
BMC IP address of the endpoint to delete

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
nico-admin-cli site-explorer delete --address 192.0.2.10
```

---

**See also:** [Tenant commands](../../tenant.md) · [CLI reference index](../../README.md)
