# `nico-admin-cli machine metadata from-expected-machine`

*[Hardware commands](../../hardware.md) › [machine](./machine.md) › [metadata](./machine-metadata.md) › **from-expected-machine***

## NAME

nico-admin-cli-machine-metadata-from-expected-machine - Copy Machine
Metadata from Expected-Machine to Machine

## SYNOPSIS

**nico-admin-cli machine metadata from-expected-machine**
\[**--replace-all**\] \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*MACHINE*\>

## DESCRIPTION

Copy Machine Metadata from Expected-Machine to Machine

## OPTIONS

**--replace-all**  
Whether to fully replace the Metadata that is currently stored on the
Machine. - If not set, existing Metadata on the Machine will not be
touched by executing the command: - The existing Name will not be
changed if the Name is not equivalent to the Machine ID or Empty. - The
existing Description will not be changed if it is not empty. - Existing
Labels and their values will not be changed. Only labels which do not
exist on the Machine will be added. - If set, the Machines Metadata will
be set to the same values as they would if the Machine would get freshly
ingested. Metadata that is currently set on the Machine will be
overridden.

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

\<*MACHINE*\>  
The machine which should get updated metadata

## Examples

```sh
nico-admin-cli machine metadata from-expected-machine 12345678-1234-5678-90ab-cdef01234567
nico-admin-cli machine metadata from-expected-machine 12345678-1234-5678-90ab-cdef01234567 --replace-all
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
