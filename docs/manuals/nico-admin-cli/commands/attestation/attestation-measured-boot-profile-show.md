# `nico-admin-cli attestation measured-boot profile show`

*[Hardware commands](../../hardware.md) › [attestation](./attestation.md) › [measured-boot](./attestation-measured-boot.md) › [profile](./attestation-measured-boot-profile.md) › **show***

## NAME

nico-admin-cli-attestation-measured-boot-profile-show - Show profiles in
different ways.

## SYNOPSIS

**nico-admin-cli attestation measured-boot profile show**
\[**--is-id**\] \[**--is-name**\] \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \[*IDENTIFIER*\]

## DESCRIPTION

Show profiles in different ways.

## OPTIONS

**--is-id**  
Explicitly say the identifier is profile ID.

**--is-name**  
Explicitly say the identifier is a profile name.

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

\[*IDENTIFIER*\]  
The optional profile ID or name.

## Examples

```sh
nico-admin-cli attestation measured-boot profile show
nico-admin-cli attestation measured-boot profile show 12345678-1234-5678-90ab-cdef01234567 --is-id
nico-admin-cli attestation measured-boot profile show my-profile --is-name
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
