# `nico-admin-cli machine nvlink-info populate`

_[Hardware commands](../../hardware.md) › [machine](./machine.md) › [nvlink-info](./machine-nvlink-info.md) › **populate**_

## NAME

nico-admin-cli-machine-nvlink-info-populate - Deprecated compatibility
command; NVLink info is populated automatically by NICo

## SYNOPSIS

**nico-admin-cli machine nvlink-info populate** \[**--update-db**\]
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]
\<*MACHINE_ID*\>

## DESCRIPTION

Deprecated compatibility command. The NICo NVLink partition manager
populates and repairs the NVLink info of a managed machine
automatically, so manual population is no longer required. This command
always returns an unsupported-operation error and does not contact
Redfish, NMX-C, or the database. MACHINE_ID, --update-db, --extended,
and --sort-by are all ignored and retained only for command-line
compatibility. Use \`nico-admin-cli machine nvlink-info show\` to
inspect the current NVLink info.

## OPTIONS

**--update-db**  
Ignored; retained for command-line compatibility

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
Machine ID (ignored)

## Examples

```sh
nico-admin-cli machine nvlink-info populate fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
