# `nico-admin-cli managed-host debug-bundle`

*[Hardware commands](../../hardware.md) › [managed-host](./managed-host.md) › **debug-bundle***

## NAME

nico-admin-cli-managed-host-debug-bundle - Download debug bundle with
logs for a specific host

## SYNOPSIS

**nico-admin-cli managed-host debug-bundle** \<**--start-time**\>
\[**--end-time**\] \[**--utc**\] \[**--output-path**\]
\[**--grafana-url**\] \[**--batch-size**\] \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*HOST_ID*\>

## DESCRIPTION

Download debug bundle with logs for a specific host

## OPTIONS

**--start-time** *\<START_TIME\>*  
Start time: YYYY-MM-DD HH:MM:SS or HH:MM:SS (uses todays date). Default:
local timezone, use --utc for UTC

**--end-time** *\<END_TIME\>*  
End time: YYYY-MM-DD HH:MM:SS or HH:MM:SS (uses todays date). Defaults
to current time if not provided. Default: local timezone, use --utc for
UTC

**--utc**  
Interpret start-time and end-time as UTC instead of local timezone

**--output-path** *\<OUTPUT_PATH\>* \[default: /tmp\]  
Output directory path for the debug bundle (default: /tmp)

**--grafana-url** *\<GRAFANA_URL\>*  
Grafana base URL (e.g., [https://grafana.example.com](https://grafana.example.com)). If not provided,
log collection is skipped.

**--batch-size** *\<BATCH_SIZE\>* \[default: 5000\]  
Batch size for log collection (default: 5000, max: 5000)

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

\<*HOST_ID*\>  
The host machine ID to collect logs for

## Examples

```sh
nico-admin-cli managed-host debug-bundle 12345678-1234-5678-90ab-cdef01234567 --start-time "2026-01-02 03:04:05" --end-time "2026-01-02 04:00:00"
nico-admin-cli managed-host debug-bundle 12345678-1234-5678-90ab-cdef01234567 --start-time 03:04:05 --utc
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
