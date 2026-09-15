# `nico-admin-cli mlx profile show`

_[Hardware commands](../../hardware.md) › [mlx](./mlx.md) › [profile](./mlx-profile.md) › **show**_

## NAME

nico-admin-cli-mlx-profile-show - Show profile details

## SYNOPSIS

**nico-admin-cli mlx profile show** \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*PROFILE_NAME*\>

## DESCRIPTION

Show profile details

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

\<*PROFILE_NAME*\>  
Profile name to show

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
