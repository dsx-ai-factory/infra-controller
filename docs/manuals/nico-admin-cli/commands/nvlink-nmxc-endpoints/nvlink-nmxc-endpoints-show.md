# `nico-admin-cli nvlink-nmxc-endpoints show`

_[Hardware commands](../../hardware.md) › [nvlink-nmxc-endpoints](./nvlink-nmxc-endpoints.md) › **show**_

## NAME

nico-admin-cli-nvlink-nmxc-endpoints-show - List chassis serial → NMX-C
endpoint mappings (optionally one serial)

## SYNOPSIS

**nico-admin-cli nvlink-nmxc-endpoints show** \[**--chassis-serial**\]
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

List chassis serial → NMX-C endpoint mappings (optionally one serial)

## OPTIONS

**--chassis-serial** *\<SERIAL\>*  
If set, show only this chassis serial

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

## Examples

```sh
nico-admin-cli nvlink-nmxc-endpoints show
nico-admin-cli nvlink-nmxc-endpoints show --chassis-serial 1234567890123
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
