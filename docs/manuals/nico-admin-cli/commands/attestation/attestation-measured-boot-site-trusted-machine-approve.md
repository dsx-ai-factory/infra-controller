# `nico-admin-cli attestation measured-boot site trusted-machine approve`

*[Hardware commands](../../hardware.md) › [attestation](./attestation.md) › [measured-boot](./attestation-measured-boot.md) › [site](./attestation-measured-boot-site.md) › [trusted-machine](./attestation-measured-boot-site-trusted-machine.md) › **approve***

## NAME

nico-admin-cli-attestation-measured-boot-site-trusted-machine-approve -
Approve a trusted machine for auto-promoting its measurements.

## SYNOPSIS

**nico-admin-cli attestation measured-boot site trusted-machine
approve** \[**--pcr-registers**\] \[**--comments**\] \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*MACHINE_ID*\>
\<*APPROVAL_TYPE*\>

## DESCRIPTION

Approve a trusted machine for auto-promoting its measurements.

## OPTIONS

**--pcr-registers** *\<PCR_REGISTERS\>*  
Specific PCR register selector. All if unset.

**--comments** *\<COMMENTS\>*  
Optional comments about this approval.

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
The machine-id to approve (or \* for all).

\<*APPROVAL_TYPE*\>  
Whether to set \`oneshot\` or \`persist\`.\

\
*Possible values:*

- oneshot

- persist

## Examples

```sh
nico-admin-cli attestation measured-boot site trusted-machine approve 12345678-1234-5678-90ab-cdef01234567 oneshot
nico-admin-cli attestation measured-boot site trusted-machine approve '*' persist --pcr-registers 0,7 --comments "trusted fleet"
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
