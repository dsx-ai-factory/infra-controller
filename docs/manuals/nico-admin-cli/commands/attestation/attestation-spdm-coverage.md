# `nico-admin-cli attestation spdm coverage`

_[Hardware commands](../../hardware.md) › [attestation](./attestation.md) › [spdm](./attestation-spdm.md) › **coverage**_

## NAME

nico-admin-cli-attestation-spdm-coverage - Show which hardware classes
the site has and what would attest each

## SYNOPSIS

**nico-admin-cli attestation spdm coverage** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Show which hardware classes the site has and what would attest each

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

## Examples

```sh
nico-admin-cli attestation spdm coverage
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
