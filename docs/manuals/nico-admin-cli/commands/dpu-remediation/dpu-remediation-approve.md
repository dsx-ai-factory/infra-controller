# `nico-admin-cli dpu-remediation approve`

*[Hardware commands](../../hardware.md) › [dpu-remediation](./dpu-remediation.md) › **approve***

## NAME

nico-admin-cli-dpu-remediation-approve - Approve a remediation

## SYNOPSIS

**nico-admin-cli dpu-remediation approve** \<**--id**\>
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Approve a remediation

## OPTIONS

**--id** *\<ID\>*  
The id of the remediation to approve

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
nico-admin-cli dpu-remediation approve --id 12345678-1234-5678-90ab-cdef01234567
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
