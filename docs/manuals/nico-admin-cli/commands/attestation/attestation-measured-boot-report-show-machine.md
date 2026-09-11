# `nico-admin-cli attestation measured-boot report show machine`

*[Hardware commands](../../hardware.md) › [attestation](./attestation.md) › [measured-boot](./attestation-measured-boot.md) › [report](./attestation-measured-boot-report.md) › [show](./attestation-measured-boot-report-show.md) › **machine***

## NAME

nico-admin-cli-attestation-measured-boot-report-show-machine - Show
reports for a machine.

## SYNOPSIS

**nico-admin-cli attestation measured-boot report show machine**
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]
\<*MACHINE_ID*\>

## DESCRIPTION

Show reports for a machine.

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

\<*MACHINE_ID*\>  
The profile name.

## Examples

```sh
nico-admin-cli attestation measured-boot report show machine 12345678-1234-5678-90ab-cdef01234567
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
