# `nico-admin-cli attestation spdm trigger`

_[Hardware commands](../../hardware.md) › [attestation](./attestation.md) › [spdm](./attestation-spdm.md) › **trigger**_

## NAME

nico-admin-cli-attestation-spdm-trigger - Trigger attestation for a
given machine with id

## SYNOPSIS

**nico-admin-cli attestation spdm trigger**
\<**--redfish-timeout-secs**\> \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*MACHINE_ID*\>

## DESCRIPTION

Trigger attestation for a given machine with id

## OPTIONS

**--redfish-timeout-secs** *\<REDFISH_TIMEOUT_SECS\>*  
Timeout in seconds for Redfish operations while triggering attestation
(passed to carbide-api)

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
Machine ID

## Examples

```sh
nico-admin-cli attestation spdm trigger 12345678-1234-5678-90ab-cdef01234567 --redfish-timeout-secs 60
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
