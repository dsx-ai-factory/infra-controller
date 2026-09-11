# `nico-admin-cli attestation measured-boot profile create`

_[Hardware commands](../../hardware.md) › [attestation](./attestation.md) › [measured-boot](./attestation-measured-boot.md) › [profile](./attestation-measured-boot-profile.md) › **create**_

## NAME

nico-admin-cli-attestation-measured-boot-profile-create - Create a new
profile with a given config.

## SYNOPSIS

**nico-admin-cli attestation measured-boot profile create**
\[**--extra-attrs**\] \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*NAME*\> \<*VENDOR*\> \<*PRODUCT*\>

## DESCRIPTION

Create a new profile with a given config.

## OPTIONS

**--extra-attrs** *\<EXTRA_ATTRS\>*  
A comma-separated list of additional k:v,k:v,... attributes to set.

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

\<*NAME*\>  
Every profile gets a name.

\<*VENDOR*\>  
The hardware vendor (e.g. dell).

\<*PRODUCT*\>  
The hardware product (e.g. poweredge_r750).

## Examples

```sh
nico-admin-cli attestation measured-boot profile create my-profile dell poweredge_r750
nico-admin-cli attestation measured-boot profile create my-profile dell poweredge_r750 --extra-attrs region:us-west,rack:r1
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
