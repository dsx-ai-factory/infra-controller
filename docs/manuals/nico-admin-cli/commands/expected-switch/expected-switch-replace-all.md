# `nico-admin-cli expected-switch replace-all`

_[Tenant commands](../../tenant.md) › [expected-switch](./expected-switch.md) › **replace-all**_

## NAME

nico-admin-cli-expected-switch-replace-all - Replace all expected
switches

## SYNOPSIS

**nico-admin-cli expected-switch replace-all**
\<**-f**\|**--filename**\> \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\]

## DESCRIPTION

Replace all expected switches

## OPTIONS

**-f**, **--filename** *\<FILENAME\>*  
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
nico-admin-cli expected-switch replace-all --filename ./switches.json
```

---

**See also:** [Tenant commands](../../tenant.md) · [CLI reference index](../../README.md)
