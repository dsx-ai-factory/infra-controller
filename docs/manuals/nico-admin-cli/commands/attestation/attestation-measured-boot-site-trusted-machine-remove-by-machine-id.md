# `nico-admin-cli attestation measured-boot site trusted-machine remove by-machine-id`

_[Hardware commands](../../hardware.md) › [attestation](./attestation.md) › [measured-boot](./attestation-measured-boot.md) › [site](./attestation-measured-boot-site.md) › [trusted-machine](./attestation-measured-boot-site-trusted-machine.md) › [remove](./attestation-measured-boot-site-trusted-machine-remove.md) › **by-machine-id**_

## NAME

nico-admin-cli-attestation-measured-boot-site-trusted-machine-remove-by-machine-id -
Remove by machine ID.

## SYNOPSIS

**nico-admin-cli attestation measured-boot site trusted-machine remove
by-machine-id** \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*MACHINE_ID*\>

## DESCRIPTION

Remove by machine ID.

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
The machine-id to remove.

## Examples

```sh
nico-admin-cli attestation measured-boot site trusted-machine remove by-machine-id 12345678-1234-5678-90ab-cdef01234567
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
