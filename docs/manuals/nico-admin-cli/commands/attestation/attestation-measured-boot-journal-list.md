# `nico-admin-cli attestation measured-boot journal list`

_[Hardware commands](../../hardware.md) › [attestation](./attestation.md) › [measured-boot](./attestation-measured-boot.md) › [journal](./attestation-measured-boot-journal.md) › **list**_

## NAME

nico-admin-cli-attestation-measured-boot-journal-list - List all journal
IDs and machines.

## SYNOPSIS

**nico-admin-cli attestation measured-boot journal list**
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]
\[*MACHINE_ID*\]

## DESCRIPTION

List all journal IDs and machines.

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
List journal entries for a machine ID.

## Examples

```sh
nico-admin-cli attestation measured-boot journal list
nico-admin-cli attestation measured-boot journal list 12345678-1234-5678-90ab-cdef01234567
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
