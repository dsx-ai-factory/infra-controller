# `nico-admin-cli attestation measured-boot site trusted-machine remove`

*[Hardware commands](../../hardware.md) › [attestation](./attestation.md) › [measured-boot](./attestation-measured-boot.md) › [site](./attestation-measured-boot-site.md) › [trusted-machine](./attestation-measured-boot-site-trusted-machine.md) › **remove***

## NAME

nico-admin-cli-attestation-measured-boot-site-trusted-machine-remove -
Remove a trusted machine approval.

## SYNOPSIS

**nico-admin-cli attestation measured-boot site trusted-machine remove**
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]
\<*subcommands*\>

## DESCRIPTION

Remove a trusted machine approval.

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
nico-admin-cli attestation measured-boot site trusted-machine remove by-approval-id 12345678-1234-5678-90ab-cdef01234567
nico-admin-cli attestation measured-boot site trusted-machine remove by-machine-id 12345678-1234-5678-90ab-cdef01234567
```

## Subcommands

| Subcommand | Description |
|---|---|
| [`by-approval-id`](./attestation-measured-boot-site-trusted-machine-remove-by-approval-id.md) | Remove by approval ID. |
| [`by-machine-id`](./attestation-measured-boot-site-trusted-machine-remove-by-machine-id.md) | Remove by machine ID. |

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
