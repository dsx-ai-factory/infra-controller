# `nico-admin-cli dpa ensure`

*[Hardware commands](../../hardware.md) › [dpa](./dpa.md) › **ensure***

## NAME

nico-admin-cli-dpa-ensure - Create/ensure a DPA interface

## SYNOPSIS

**nico-admin-cli dpa ensure** \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*MACHINE_ID*\> \<*MAC_ADDR*\> \<*DEVICE_TYPE*\>
\<*PCI_NAME*\> \<*INTERFACE_TYPE*\> \[*DEVICE_DESCRIPTION*\]

## DESCRIPTION

Create/ensure a DPA interface

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

\<*MACHINE_ID*\>  
Machine ID

\<*MAC_ADDR*\>  
MAC address (e.g. 00:11:22:33:44:55)

\<*DEVICE_TYPE*\>  
Device type (e.g. BlueField3)

\<*PCI_NAME*\>  
PCI name (e.g. 5e:00.0)

\<*INTERFACE_TYPE*\>  
Interface type (e.g. SVPC or ASTRA)\

\
*Possible values:*

- svpc

- astra

\[*DEVICE_DESCRIPTION*\]  
Device description (e.g. NVIDIA BlueField-3 B3140L E-Series FHHL
SuperNIC)

## Examples

```sh
nico-admin-cli dpa ensure 12345678-1234-5678-90ab-cdef01234567 00:11:22:33:44:55 BlueField3 5e:00.0
nico-admin-cli dpa ensure 12345678-1234-5678-90ab-cdef01234567 00:11:22:33:44:55 BlueField3 5e:00.0 "NVIDIA BlueField-3 B3140L E-Series FHHL SuperNIC"
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
