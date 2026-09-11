# `nico-admin-cli attestation measured-boot journal show`

*[Hardware commands](../../hardware.md) › [attestation](./attestation.md) › [measured-boot](./attestation-measured-boot.md) › [journal](./attestation-measured-boot-journal.md) › **show***

## NAME

nico-admin-cli-attestation-measured-boot-journal-show - Show a journal
entry by ID, or all.

## SYNOPSIS

**nico-admin-cli attestation measured-boot journal show**
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]
\[*JOURNAL_ID*\]

## DESCRIPTION

Show a journal entry by ID, or all.

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

\[*JOURNAL_ID*\]  
The optional journal entry ID.

## Examples

```sh
nico-admin-cli attestation measured-boot journal show
nico-admin-cli attestation measured-boot journal show 12345678-1234-5678-90ab-cdef01234567
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
