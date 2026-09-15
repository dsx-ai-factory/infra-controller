# `nico-admin-cli set bmc-proxy`

_[Hardware commands](../../hardware.md) › [set](./set.md) › **bmc-proxy**_

## NAME

nico-admin-cli-set-bmc-proxy - Set bmc_proxy

## SYNOPSIS

**nico-admin-cli set bmc-proxy** \<**--enabled**\> \[**--proxy**\]
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Set bmc_proxy

## OPTIONS

**--enabled** *\<ENABLED\>*  
Enable the dynamic site_explorer.bmc_proxy redirect for development and
integration environments. This setting is independent of static
\[bmc_proxy\] routing through nico-bmc-proxy.\

\
*Possible values:*

- true

- false

**--proxy** *\<PROXY\>*  
host:port string use as a proxy for talking to BMCs

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
nico-admin-cli set bmc-proxy --enabled true --proxy 192.0.2.10:8080
nico-admin-cli set bmc-proxy --enabled false
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
