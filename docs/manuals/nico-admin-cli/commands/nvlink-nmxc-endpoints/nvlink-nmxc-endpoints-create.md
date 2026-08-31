# `nico-admin-cli nvlink-nmxc-endpoints create`

*[Hardware commands](../../hardware.md) › [nvlink-nmxc-endpoints](./nvlink-nmxc-endpoints.md) › **create***

## NAME

nico-admin-cli-nvlink-nmxc-endpoints-create - Insert a mapping for a
chassis serial

## SYNOPSIS

**nico-admin-cli nvlink-nmxc-endpoints create** \<**--chassis-serial**\>
\<**--endpoint**\> \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\]

## DESCRIPTION

Insert a mapping for a chassis serial

## OPTIONS

**--chassis-serial** *\<SERIAL\>*  
**--endpoint** *\<ENDPOINT\>*  
NMX-C gRPC base URL (e.g. [https://host:50051](https://host:50051))

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
nico-admin-cli nvlink-nmxc-endpoints create --chassis-serial 1234567890123 --endpoint https://192.0.2.10:50051
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
