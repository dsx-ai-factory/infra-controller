# `nico-admin-cli power-shelf metadata`

_[Hardware commands](../../hardware.md) › [power-shelf](./power-shelf.md) › **metadata**_

## NAME

nico-admin-cli-power-shelf-metadata - Manage Power Shelf Metadata

## SYNOPSIS

**nico-admin-cli power-shelf metadata** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

Manage Power Shelf Metadata

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
| [`set`](./power-shelf-metadata-set.md) | Set the Name or Description of the Power Shelf |
| [`show`](./power-shelf-metadata-show.md) | Show the Metadata of the Power Shelf |
| [`add-label`](./power-shelf-metadata-add-label.md) | Adds a label to the Metadata of a Power Shelf |
| [`remove-labels`](./power-shelf-metadata-remove-labels.md) | Removes labels from the Metadata of a Power Shelf |
| [`from-expected-power-shelf`](./power-shelf-metadata-from-expected-power-shelf.md) | Copy Power Shelf Metadata from Expected-Power-Shelf to Power Shelf |

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
