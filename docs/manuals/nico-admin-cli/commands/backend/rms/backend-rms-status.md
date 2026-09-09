# `nico-admin-cli backend rms status`

_[Hardware commands](../../../hardware.md) › [backend](../backend.md) › [rms](./backend-rms.md) › **status**_

## NAME

nico-admin-cli-backend-rms-status - Probe the RMS backend connectivity and version

## SYNOPSIS

**nico-admin-cli backend rms status** \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\]

## DESCRIPTION

Probe the RMS backend connectivity and version

Sends a `GetRmsVersion` RPC to nico-api, which proxies the request to its
configured RMS backend. Prints a machine-readable status token, a human-readable
message, and (on success) the version string the RMS backend returns.

Exits with status `0` on a fully successful probe (`status: connected`);
exits with status `1` for any error condition.

The `-f json` global flag switches output to a JSON object with the same
three fields.

### Status tokens

| Token | Meaning |
|---|---|
| `connected` | Probe reached RMS and received a version. |
| `not-configured` | RMS endpoint is not set in this nico-api instance. |
| `api-unreachable` | CLI could not connect to nico-api (server down or wrong URL). |
| `rms-unreachable` | nico-api reached but cannot contact the RMS backend. |
| `auth-failed` | A certificate was rejected on the CLI→nico-api or nico-api→RMS path. |
| `timeout` | Connection attempt exceeded the deadline. |
| `error` | Unexpected error; see the `message` field for the gRPC code and detail. Note: `Unimplemented` falls here when the targeted nico-api predates the `GetRmsVersion` RPC. |

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

## Examples

```sh
nico-admin-cli backend rms status
nico-admin-cli -f json backend rms status
```

### Successful output

```text
status:  connected
message: rms status probe successful
version: 1.2.3
```

### RMS not configured output

```text
status:  not-configured
message: rms is not configured on this nico-api instance — set the rms endpoint in the nico-api configuration
version: -
```

---

**See also:** [Hardware commands](../../../hardware.md) · [CLI reference index](../../../README.md)
