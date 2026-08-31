# `nico-admin-cli attestation spdm list`

*[Hardware commands](../../hardware.md) › [attestation](./attestation.md) › [spdm](./attestation-spdm.md) › **list***

## NAME

nico-admin-cli-attestation-spdm-list - List SPDM attestation machine
statuses

## SYNOPSIS

**nico-admin-cli attestation spdm list** \[**--machine-id**\]
\[**--selector**\] \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\]

## DESCRIPTION

List SPDM attestation machine statuses

## OPTIONS

**--machine-id** *\<MACHINE_ID\>*  
Machine ID

**--selector** *\<SELECTOR\>*  
Filter attestation machines by selector\

\
*Possible values:*

- in-progress

- unsuccessful

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
nico-admin-cli attestation spdm list 12345678-1234-5678-90ab-cdef01234567
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
