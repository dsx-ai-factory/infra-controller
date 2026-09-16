# `nico-admin-cli attestation spdm profile create`

_[Hardware commands](../../hardware.md) › [attestation](./attestation.md) › [spdm](./attestation-spdm.md) › [profile](./attestation-spdm-profile.md) › **create**_

## NAME

nico-admin-cli-attestation-spdm-profile-create - Store an attestation
profile for a hardware class

## SYNOPSIS

**nico-admin-cli attestation spdm profile create** \<**--mode**\>
\[**--exact**\] \[**--prefix**\] \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*HARDWARE_CLASS*\>

## DESCRIPTION

Store an attestation profile for a hardware class

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
Hardware class the profile is keyed to. One of the classes exploration
records, or any for the fallback. Run attestation spdm coverage to see
the classes this site has

## Examples

```sh
nico-admin-cli attestation spdm profile create Gb200 --mode allowlist --prefix HGX_IRoT_GPU_
nico-admin-cli attestation spdm profile create Gb200 --mode allowlist --prefix HGX_IRoT_GPU_ --exact VERA_CPU_0
nico-admin-cli attestation spdm profile create any --mode all
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
