# `nico-admin-cli machine-validation plugins`

*[Hardware commands](../../hardware.md) › [machine-validation](./machine-validation.md) › **plugins***

## NAME

nico-admin-cli-machine-validation-plugins - Manage OCI Machine
Validation plugins

## SYNOPSIS

**nico-admin-cli machine-validation plugins** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

Manage OCI Machine Validation plugins

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

## Subcommands

| Subcommand | Description |
|---|---|
| [`create`](./machine-validation-plugins-create.md) | Create an OCI Machine Validation plugin |
| [`verify`](./machine-validation-plugins-verify.md) | Verify a plugin revision |
| [`approve-full-host`](./machine-validation-plugins-approve-full-host.md) | Approve full host access for a verified plugin revision |
| [`enable`](./machine-validation-plugins-enable.md) | Enable a plugin revision |
| [`disable`](./machine-validation-plugins-disable.md) | Disable a plugin revision |

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
