# `nico-admin-cli attestation measured-boot machine show`

_[Hardware commands](../../hardware.md) › [attestation](./attestation.md) › [measured-boot](./attestation-measured-boot.md) › [machine](./attestation-measured-boot-machine.md) › **show**_

## NAME

nico-admin-cli-attestation-measured-boot-machine-show - Get all info
about a machine.

## SYNOPSIS

**nico-admin-cli attestation measured-boot machine show**
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]
\[*MACHINE_ID*\]

## DESCRIPTION

Get all info about a machine.

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

\[*MACHINE_ID*\]  
The machine ID to show.

## Examples

```sh
nico-admin-cli attestation measured-boot machine show
nico-admin-cli attestation measured-boot machine show 12345678-1234-5678-90ab-cdef01234567
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
