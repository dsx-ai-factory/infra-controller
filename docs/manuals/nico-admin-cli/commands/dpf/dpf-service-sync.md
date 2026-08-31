# `nico-admin-cli dpf service-sync`

*[Hardware commands](../../hardware.md) › [dpf](./dpf.md) › **service-sync***

## NAME

nico-admin-cli-dpf-service-sync - Release DPF maintenance holds blocking
a DPUService rollout

## SYNOPSIS

**nico-admin-cli dpf service-sync** \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

Release DPF maintenance holds blocking a DPUService rollout

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
nico-admin-cli dpf service-sync list
nico-admin-cli dpf service-sync list --machine-id fm100psbtmb15tgh6q5duqb8ke5grng7ksd96hetbeie9nc5pvcca6eol80
nico-admin-cli dpf service-sync release --machine-id fm100psbtmb15tgh6q5duqb8ke5grng7ksd96hetbeie9nc5pvcca6eol80
nico-admin-cli dpf service-sync release --instance-id 12345678-1234-5678-90ab-cdef01234567
```

## Subcommands

| Subcommand | Description |
|---|---|
| [`list`](./dpf-service-sync-list.md) | List machines DPF is waiting on before a DPUService rollout |
| [`release`](./dpf-service-sync-release.md) | Release the DPF maintenance hold blocking a DPUService rollout |

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
