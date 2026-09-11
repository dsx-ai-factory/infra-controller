# `nico-admin-cli browse ufm`

*[Hardware commands](../../hardware.md) › [browse](./browse.md) › **ufm***

## NAME

nico-admin-cli-browse-ufm - Browse a UFM fabric via the API server

## SYNOPSIS

**nico-admin-cli browse ufm** \<**--fabric-id**\> \<**--path**\>
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Browse a UFM fabric via the API server

## OPTIONS

**--fabric-id** *\<FABRIC_ID\>*  
UFM fabric ID

**--path** *\<PATH\>*  
Path to browse within the fabric

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
nico-admin-cli browse ufm --fabric-id default --path /ufmRest/resources/systems
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
