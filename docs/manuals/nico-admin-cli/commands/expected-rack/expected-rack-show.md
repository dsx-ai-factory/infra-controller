# `nico-admin-cli expected-rack show`

*[Tenant commands](../../tenant.md) › [expected-rack](./expected-rack.md) › **show***

## NAME

nico-admin-cli-expected-rack-show - Show expected rack

## SYNOPSIS

**nico-admin-cli expected-rack show** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \[*RACK_ID*\]

## DESCRIPTION

Show expected rack

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

\[*RACK_ID*\]  
Rack ID of the expected rack to show. Leave unset for all.

## Examples

```sh
nico-admin-cli expected-rack show
nico-admin-cli expected-rack show 12345678-1234-5678-90ab-cdef01234567
```

---

**See also:** [Tenant commands](../../tenant.md) · [CLI reference index](../../index.md)
