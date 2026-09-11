# `nico-admin-cli credential add-nmx-m`

*[Hardware commands](../../hardware.md) › [credential](./credential.md) › **add-nmx-m***

## NAME

nico-admin-cli-credential-add-nmx-m - Deprecated compatibility command;
NMX-M is no longer supported

## SYNOPSIS

**nico-admin-cli credential add-nmx-m** \[**--username**\]
\[**--password**\] \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\]

## DESCRIPTION

Deprecated compatibility command. NMX-M is no longer supported. This
command always returns an error and does not modify credentials. Use
NMX-C for NVLink partition management.

## OPTIONS

**--username** *\<USERNAME\>*  
Username

**--password** *\<PASSWORD\>*  
password

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
nico-admin-cli credential add-nmx-m
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
