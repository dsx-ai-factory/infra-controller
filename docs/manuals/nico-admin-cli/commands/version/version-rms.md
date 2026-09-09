# `nico-admin-cli version rms`

_[Admin commands](../../admin.md) › [version](./version.md) › **rms**_

## NAME

nico-admin-cli-version-rms - Show the version of the configured RMS backend via nico-api

## SYNOPSIS

**nico-admin-cli version rms** \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\]

## DESCRIPTION

Show the version of the configured RMS backend via nico-api

Sends a `GetRmsVersion` RPC to nico-api, which proxies the request to its
configured RMS backend and prints the version string RMS returns. Returns an
error if RMS is not configured on the targeted nico-api instance.

## OPTIONS

**--extended**  
Extended result output.

This used by measured boot, where basic output contains just what you
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
nico-admin-cli version rms
```

---

**See also:** [Admin commands](../../admin.md) · [CLI reference index](../../README.md)
