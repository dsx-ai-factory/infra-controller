# `nico-admin-cli machine metadata`

*[Hardware commands](../../hardware.md) › [machine](./machine.md) › **metadata***

## NAME

nico-admin-cli-machine-metadata - Edit Metadata associated with a
Machine

## SYNOPSIS

**nico-admin-cli machine metadata** \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

Edit Metadata associated with a Machine

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
| [`set`](./machine-metadata-set.md) | Set the Name or Description of the Machine |
| [`show`](./machine-metadata-show.md) | Show the Metadata of the Machine |
| [`add-label`](./machine-metadata-add-label.md) | Adds a label to the Metadata of a Machine |
| [`remove-labels`](./machine-metadata-remove-labels.md) | Removes labels from the Metadata of a Machine |
| [`from-expected-machine`](./machine-metadata-from-expected-machine.md) | Copy Machine Metadata from Expected-Machine to Machine |

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
