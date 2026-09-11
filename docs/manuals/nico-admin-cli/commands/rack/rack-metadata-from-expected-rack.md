# `nico-admin-cli rack metadata from-expected-rack`

*[Hardware commands](../../hardware.md) › [rack](./rack.md) › [metadata](./rack-metadata.md) › **from-expected-rack***

## NAME

nico-admin-cli-rack-metadata-from-expected-rack - Copy Rack Metadata
from Expected-Rack to Rack

## SYNOPSIS

**nico-admin-cli rack metadata from-expected-rack**
\[**--replace-all**\] \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*RACK*\>

## DESCRIPTION

Copy Rack Metadata from Expected-Rack to Rack

## OPTIONS

**--replace-all**  
Whether to fully replace the Metadata that is currently stored on the
Rack. - If not set, existing Metadata on the Rack will not be touched by
executing the command: - The existing Name will not be changed if the
Name is not equivalent to the Rack ID or Empty. - The existing
Description will not be changed if it is not empty. - Existing Labels
and their values will not be changed. Only labels which do not exist on
the Rack will be added. - If set, the Racks Metadata will be set to the
same values as they would if the Rack would get freshly ingested.
Metadata that is currently set on the Rack will be overridden.

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

\<*RACK*\>  
The rack which should get updated metadata

## Examples

```sh
nico-admin-cli rack metadata from-expected-rack 12345678-1234-5678-90ab-cdef01234567
nico-admin-cli rack metadata from-expected-rack 12345678-1234-5678-90ab-cdef01234567 --replace-all
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
