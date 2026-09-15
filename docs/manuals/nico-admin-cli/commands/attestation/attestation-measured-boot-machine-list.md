# `nico-admin-cli attestation measured-boot machine list`

_[Hardware commands](../../hardware.md) › [attestation](./attestation.md) › [measured-boot](./attestation-measured-boot.md) › [machine](./attestation-measured-boot-machine.md) › **list**_

## NAME

nico-admin-cli-attestation-measured-boot-machine-list - List all
machines + their info.

## SYNOPSIS

**nico-admin-cli attestation measured-boot machine list**
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

List all machines + their info.

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

## Examples

```sh
nico-admin-cli attestation measured-boot machine list
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
