# `nico-admin-cli attestation spdm profile update`

_[Hardware commands](../../hardware.md) › [attestation](./attestation.md) › [spdm](./attestation-spdm.md) › [profile](./attestation-spdm-profile.md) › **update**_

## NAME

nico-admin-cli-attestation-spdm-profile-update - Replace the selection
of a stored attestation profile

## SYNOPSIS

**nico-admin-cli attestation spdm profile update** \<**--mode**\>
\[**--exact**\] \[**--prefix**\] \[**--if-version-match**\]
\[**--force**\] \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*HARDWARE_CLASS*\>

## DESCRIPTION

Replace the selection of a stored attestation profile

## OPTIONS

**--mode** *\<MODE\>*  
Which attesters the hardware requires\

\
*Possible values:*

- none: Attest nothing. Attestation is off for this hardware

- all: Attest every attester the BMC reports

- allowlist: Attest only the attesters a pattern matches

- denylist: Attest every attester the BMC reports except those a pattern
  matches

**--exact** *\<COMPONENT_ID\>*  
Match one ComponentIntegrity ID in full. Repeatable

**--prefix** *\<PREFIX\>*  
Match every ComponentIntegrity ID starting with this. Repeatable

**--if-version-match** *\<IF_VERSION_MATCH\>*  
Apply only if the stored version still matches this. Omitted applies to
whatever is stored now

**--force**  
Required to switch the any fallback to mode none, which stops attesting
every class without a profile of its own

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

\<*HARDWARE_CLASS*\>  
Hardware class the profile is keyed to

## Examples

```sh
nico-admin-cli attestation spdm profile update Gb200 --mode denylist --exact HGX_BMC_0 --if-version-match V7-T1789080000000000
nico-admin-cli attestation spdm profile update any --mode none --force
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
