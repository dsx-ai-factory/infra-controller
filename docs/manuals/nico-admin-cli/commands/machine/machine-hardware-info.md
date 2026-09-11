# `nico-admin-cli machine hardware-info`

*[Hardware commands](../../hardware.md) › [machine](./machine.md) › **hardware-info***

## NAME

nico-admin-cli-machine-hardware-info - Update/show machine hardware info

## SYNOPSIS

**nico-admin-cli machine hardware-info** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

Update/show machine hardware info

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
nico-admin-cli machine hardware-info show --machine 12345678-1234-5678-90ab-cdef01234567
nico-admin-cli machine hardware-info update gpus --machine 12345678-1234-5678-90ab-cdef01234567 --gpu-json-file ./gpus.json
```

## Subcommands

| Subcommand | Description |
|---|---|
| [`show`](./machine-hardware-info-show.md) | Show the hardware info of the machine |
| [`update`](./machine-hardware-info-update.md) | Update the hardware info of the machine |

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
