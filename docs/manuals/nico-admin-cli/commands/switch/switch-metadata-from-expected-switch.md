# `nico-admin-cli switch metadata from-expected-switch`

*[Hardware commands](../../hardware.md) › [switch](./switch.md) › [metadata](./switch-metadata.md) › **from-expected-switch***

## NAME

nico-admin-cli-switch-metadata-from-expected-switch - Copy Switch
Metadata from Expected-Switch to Switch

## SYNOPSIS

**nico-admin-cli switch metadata from-expected-switch**
\[**--replace-all**\] \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*SWITCH*\>

## DESCRIPTION

Copy Switch Metadata from Expected-Switch to Switch

## OPTIONS

**--replace-all**  
Whether to fully replace the Metadata that is currently stored on the
Switch. - If not set, existing Metadata on the Switch will not be
touched by executing the command: - The existing Name will not be
changed if the Name is not equivalent to the Switch ID or Empty. - The
existing Description will not be changed if it is not empty. - Existing
Labels and their values will not be changed. Only labels which do not
exist on the Switch will be added. - If set, the Switches Metadata will
be set to the same values as they would if the Switch would get freshly
ingested. Metadata that is currently set on the Switch will be
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

\<*SWITCH*\>  
The switch which should get updated metadata

## Examples

```sh
nico-admin-cli switch metadata from-expected-switch 12345678-1234-5678-90ab-cdef01234567
nico-admin-cli switch metadata from-expected-switch 12345678-1234-5678-90ab-cdef01234567 --replace-all
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
