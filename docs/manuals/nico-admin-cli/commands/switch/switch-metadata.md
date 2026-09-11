# `nico-admin-cli switch metadata`

_[Hardware commands](../../hardware.md) › [switch](./switch.md) › **metadata**_

## NAME

nico-admin-cli-switch-metadata - Manage Switch Metadata

## SYNOPSIS

**nico-admin-cli switch metadata** \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

Manage Switch Metadata

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
| [`set`](./switch-metadata-set.md) | Set the Name or Description of the Switch |
| [`show`](./switch-metadata-show.md) | Show the Metadata of the Switch |
| [`add-label`](./switch-metadata-add-label.md) | Adds a label to the Metadata of a Switch |
| [`remove-labels`](./switch-metadata-remove-labels.md) | Removes labels from the Metadata of a Switch |
| [`from-expected-switch`](./switch-metadata-from-expected-switch.md) | Copy Switch Metadata from Expected-Switch to Switch |

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
