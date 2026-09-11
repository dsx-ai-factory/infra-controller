# `nico-admin-cli expected-rack delete`

*[Tenant commands](../../tenant.md) › [expected-rack](./expected-rack.md) › **delete***

## NAME

nico-admin-cli-expected-rack-delete - Delete expected rack

## SYNOPSIS

**nico-admin-cli expected-rack delete** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*RACK_ID*\>

## DESCRIPTION

Delete expected rack

## OPTIONS

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

\<*RACK_ID*\>  
Rack ID of expected rack to delete.

## Examples

```sh
nico-admin-cli expected-rack delete 12345678-1234-5678-90ab-cdef01234567
```

---

**See also:** [Tenant commands](../../tenant.md) · [CLI reference index](../../index.md)
