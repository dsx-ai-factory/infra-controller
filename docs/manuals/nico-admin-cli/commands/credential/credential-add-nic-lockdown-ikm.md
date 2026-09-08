# `nico-admin-cli credential add-nic-lockdown-ikm`

_[Hardware commands](../../hardware.md) › [credential](./credential.md) › **add-nic-lockdown-ikm**_

## NAME

nico-admin-cli-credential-add-nic-lockdown-ikm - Set the site-wide
SuperNIC lockdown IKM (input key material)

## SYNOPSIS

**nico-admin-cli credential add-nic-lockdown-ikm** \<**--password**\>
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Set the site-wide SuperNIC lockdown IKM (input key material)

## OPTIONS

**--password** *\<PASSWORD\>*  
The site-wide NIC lockdown IKM value

**--extended**  
Extended result output.

This used by measured boot, where basic output contains just what you
probably care about, and "extended" output also dumps out all the
internal UUIDs that are used to associate instances.

**--sort-by** *\<SORT_BY\>* \[default: primary-id\]  
Sort output by specified field  

  
*Possible values:*

- primary-id: Sort by the primary ID

- state: Sort by state

**-h**, **--help**  
Print help (see a summary with -h)

## Examples

```sh
nico-admin-cli credential add-nic-lockdown-ikm --password mypassword
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
