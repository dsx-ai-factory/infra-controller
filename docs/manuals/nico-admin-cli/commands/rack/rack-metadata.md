# `nico-admin-cli rack metadata`

*[Hardware commands](../../hardware.md) › [rack](./rack.md) › **metadata***

## NAME

nico-admin-cli-rack-metadata - Edit Metadata associated with a Rack

## SYNOPSIS

**nico-admin-cli rack metadata** \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

Edit Metadata associated with a Rack

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
| [`set`](./rack-metadata-set.md) | Set the Name or Description of the Rack |
| [`show`](./rack-metadata-show.md) | Show the Metadata of the Rack |
| [`add-label`](./rack-metadata-add-label.md) | Adds a label to the Metadata of a Rack |
| [`remove-labels`](./rack-metadata-remove-labels.md) | Removes labels from the Metadata of a Rack |
| [`from-expected-rack`](./rack-metadata-from-expected-rack.md) | Copy Rack Metadata from Expected-Rack to Rack |

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
