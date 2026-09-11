# `nico-admin-cli dpu-remediation list-applied`

*[Hardware commands](../../hardware.md) › [dpu-remediation](./dpu-remediation.md) › **list-applied***

## NAME

nico-admin-cli-dpu-remediation-list-applied - Display information about
applied remediations

## SYNOPSIS

**nico-admin-cli dpu-remediation list-applied** \[**--remediation-id**\]
\[**--machine-id**\] \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\]

## DESCRIPTION

Display information about applied remediations

## OPTIONS

**--remediation-id** *\<REMEDIATION_ID\>*  
The remediation id to query, in case the user wants to see which
machines have a specific remediation applied. Provide both arguments to
see all the details for a specific remediation and machine.

**--machine-id** *\<MACHINE_ID\>*  
The machine id to query, in case the user wants to see which
remediations have been applied to a specific box. Provide both arguments
to see all the details for a specific remediation and machine.

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
nico-admin-cli dpu-remediation list-applied
nico-admin-cli dpu-remediation list-applied --remediation-id 12345678-1234-5678-90ab-cdef01234567
nico-admin-cli dpu-remediation list-applied --machine-id abcdef01-2345-6789-abcd-ef0123456789
nico-admin-cli dpu-remediation list-applied --remediation-id 12345678-1234-5678-90ab-cdef01234567 --machine-id abcdef01-2345-6789-abcd-ef0123456789
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
