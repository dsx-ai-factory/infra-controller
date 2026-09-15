# `nico-admin-cli attestation measured-boot site trusted-profile approve`

_[Hardware commands](../../hardware.md) › [attestation](./attestation.md) › [measured-boot](./attestation-measured-boot.md) › [site](./attestation-measured-boot-site.md) › [trusted-profile](./attestation-measured-boot-site-trusted-profile.md) › **approve**_

## NAME

nico-admin-cli-attestation-measured-boot-site-trusted-profile-approve -
Allow auto-promoting of measurements from machines matching a profile.

## SYNOPSIS

**nico-admin-cli attestation measured-boot site trusted-profile
approve** \[**--pcr-registers**\] \[**--comments**\] \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*PROFILE_ID*\>
\<*APPROVAL_TYPE*\>

## DESCRIPTION

Allow auto-promoting of measurements from machines matching a profile.

## OPTIONS

**--pcr-registers** *\<PCR_REGISTERS\>*  
Specific PCR register selector. All if unset.

**--comments** *\<COMMENTS\>*  
Optional comments about this approval.

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

\<*PROFILE_ID*\>  
The profile-id to approve.

\<*APPROVAL_TYPE*\>  
Whether to set \`oneshot\` or \`persist\`.\

\
*Possible values:*

- oneshot

- persist

## Examples

```sh
nico-admin-cli attestation measured-boot site trusted-profile approve 12345678-1234-5678-90ab-cdef01234567 oneshot
nico-admin-cli attestation measured-boot site trusted-profile approve 12345678-1234-5678-90ab-cdef01234567 persist --pcr-registers 0,7 --comments "trusted SKU"
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
