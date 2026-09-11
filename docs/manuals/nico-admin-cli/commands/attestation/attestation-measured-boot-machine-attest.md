# `nico-admin-cli attestation measured-boot machine attest`

*[Hardware commands](../../hardware.md) › [attestation](./attestation.md) › [measured-boot](./attestation-measured-boot.md) › [machine](./attestation-measured-boot-machine.md) › **attest***

## NAME

nico-admin-cli-attestation-measured-boot-machine-attest - Send
measurements for a machine.

## SYNOPSIS

**nico-admin-cli attestation measured-boot machine attest**
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]
\<*MACHINE_ID*\> \<*VALUES*\>

## DESCRIPTION

Send measurements for a machine.

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
The machine ID of the machine to associate this report with.

\<*VALUES*\>  
Comma-separated list of {pcr_register:value,...} to associate with this
report.

## Examples

```sh
nico-admin-cli attestation measured-boot machine attest 12345678-1234-5678-90ab-cdef01234567 0:abc123,7:def456
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
