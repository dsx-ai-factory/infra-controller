# `nico-admin-cli rack maintenance terminate`

*[Hardware commands](../../hardware.md) › [rack](./rack.md) › [maintenance](./rack-maintenance.md) › **terminate***

## NAME

nico-admin-cli-rack-maintenance-terminate - Terminate active rack
maintenance and transition the rack to Error

## SYNOPSIS

**nico-admin-cli rack maintenance terminate** \<**-r**\|**--rack**\>
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Request that the rack controller terminate active maintenance and
transition the rack to Error. The command is rejected unless the rack is
in Maintenance; repeating an accepted request before it is consumed is
safe. Device requests and current phase status are cleaned up. This
terminates NICo rack maintenance orchestration, but does not guarantee
that work already submitted to an external backend is stopped.

## OPTIONS

**-r**, **--rack** *\<RACK\>*  
Rack ID whose active maintenance should be terminated

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
nico-admin-cli rack maintenance terminate --rack rack-42-us-west
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
