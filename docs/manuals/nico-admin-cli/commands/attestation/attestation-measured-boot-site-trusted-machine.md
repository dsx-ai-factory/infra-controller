# `nico-admin-cli attestation measured-boot site trusted-machine`

*[Hardware commands](../../hardware.md) › [attestation](./attestation.md) › [measured-boot](./attestation-measured-boot.md) › [site](./attestation-measured-boot-site.md) › **trusted-machine***

## NAME

nico-admin-cli-attestation-measured-boot-site-trusted-machine - Managed
trusted machines.

## SYNOPSIS

**nico-admin-cli attestation measured-boot site trusted-machine**
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]
\<*subcommands*\>

## DESCRIPTION

Managed trusted machines.

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
nico-admin-cli attestation measured-boot site trusted-machine approve 12345678-1234-5678-90ab-cdef01234567 oneshot
nico-admin-cli attestation measured-boot site trusted-machine list
nico-admin-cli attestation measured-boot site trusted-machine remove by-machine-id 12345678-1234-5678-90ab-cdef01234567
```

## Subcommands

| Subcommand | Description |
|---|---|
| [`approve`](./attestation-measured-boot-site-trusted-machine-approve.md) | Approve a trusted machine for auto-promoting its measurements. |
| [`remove`](./attestation-measured-boot-site-trusted-machine-remove.md) | Remove a trusted machine approval. |
| [`list`](./attestation-measured-boot-site-trusted-machine-list.md) | List all active machine approvals. |

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
