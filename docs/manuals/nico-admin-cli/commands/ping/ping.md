# `nico-admin-cli ping`

_[Admin commands](../../admin.md) › **ping**_

## NAME

nico-admin-cli-ping - Query the Version gRPC endpoint repeatedly
printing how long it took and any failures.

## SYNOPSIS

**nico-admin-cli ping** \[**-i**\|**--interval**\] \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Query the Version gRPC endpoint repeatedly printing how long it took and
any failures.

## OPTIONS

**-i**, **--interval** *\<INTERVAL\>* \[default: 1.0\]  
Wait interval seconds between sending each request. Real number allowed
with dot as a decimal separator.

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
nico-admin-cli ping
nico-admin-cli ping --interval 0.5
```

---

**See also:** [Admin commands](../../admin.md) · [CLI reference index](../../README.md)
