# `nico-admin-cli generate-shell-complete fish`

*[Admin commands](../../admin.md) › [generate-shell-complete](./generate-shell-complete.md) › **fish***

## NAME

nico-admin-cli-generate-shell-complete-fish

## SYNOPSIS

**nico-admin-cli generate-shell-complete fish** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

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

---

**See also:** [Admin commands](../../admin.md) · [CLI reference index](../../index.md)
