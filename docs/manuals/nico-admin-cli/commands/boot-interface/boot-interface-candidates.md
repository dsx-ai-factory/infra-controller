# `nico-admin-cli boot-interface candidates`

*[Hardware commands](../../hardware.md) › [boot-interface](./boot-interface.md) › **candidates***

## NAME

nico-admin-cli-boot-interface-candidates - List boot-interface
candidates for a machine and the picks among them

## SYNOPSIS

**nico-admin-cli boot-interface candidates** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*MACHINE*\>

## DESCRIPTION

List every NIC that could be the boot interface for a machine -- the
managed \`machine_interfaces\` rows and the pre-first-lease predictions
-- and mark the picks among them: \`current\` (the effective managed
pick, or the predicted pick before the first lease), \`default\` (the
lowest-MAC non-underlay managed interface if no primary interface were
set), and \`explored\` (the default site-explorer recorded for the BMC
endpoint of the machine). The predicted pick is a declared primary, or
the sole non-underlay prediction; with several eligible undeclared
predictions the system refuses to guess. For selection, an
already-declared primary is eligible regardless of segment; otherwise
underlay rows are excluded from the automatic fallback. The
selection/default picks are computed server-side; use \`boot-interface
show\` to compare them with the persisted desired target and controller
progress. Read-only.

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

\<*MACHINE*\>  
The machine ID for which to list boot-interface candidates

## Examples

```sh
nico-admin-cli boot-interface candidates 12345678-1234-5678-90ab-cdef01234567
nico-admin-cli --format json boot-interface candidates 12345678-1234-5678-90ab-cdef01234567
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
