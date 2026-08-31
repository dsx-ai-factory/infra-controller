# `nico-admin-cli network-device show`

*[Network commands](../../network.md) › [network-device](./network-device.md) › **show***

## NAME

nico-admin-cli-network-device-show - Display network device information

## SYNOPSIS

**nico-admin-cli network-device show** \[**-a**\|**--all**\]
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\] \[*ID*\]

## DESCRIPTION

Display network device information

## OPTIONS

**-a**, **--all**  
Show all network devices (DEPRECATED)

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

\[*ID*\] \[default: \]  
Show data for the given network device (e.g. \`mac=\<mac\>\`), leave
empty for all (default)

## Examples

```sh
nico-admin-cli network-device show
nico-admin-cli network-device show mac=00:11:22:33:44:55
```

---

**See also:** [Network commands](../../network.md) · [CLI reference index](../../index.md)
