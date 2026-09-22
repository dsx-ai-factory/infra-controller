# `nico-admin-cli backend rms status`

*[Hardware commands](../../hardware.md) › [backend](./backend.md) › [rms](./backend-rms.md) › **status***

## NAME

nico-admin-cli-backend-rms-status - Probe the RMS backend connectivity
and version

## SYNOPSIS

```text
nico-admin-cli backend rms status [--extended]
[--sort-by] [-h|--help]
```

## DESCRIPTION

Probe the RMS backend connectivity and version.

Sends a `GetRmsVersion` rpc to nico-api, which proxies the request to
its configured RMS backend. Text output prints three lines — `status`
(a machine-readable token), `message` (a human-readable sentence), and
`version` (the string the RMS backend returned, or `-` when the
probe did not get one). The global `-f json` flag emits the same three
fields as a JSON object.

Exits `0` only when the status is `connected`; every other status
exits `1`.

Status tokens:

`connected` — the probe reached RMS and received a version.

`not-configured` — no RMS endpoint is set on this nico-api instance.

`api-unreachable` — the cli could not connect to nico-api, because the
server is down or the url is wrong.

`rms-unreachable` — nico-api was reached but cannot contact the RMS
backend.

`cli-config-error` — a local cli configuration problem, such as a
missing CA file, prevented connecting to nico-api.

`auth-failed` — a certificate was rejected on the cli→nico-api or
nico-api→RMS path.

`auth-or-version-mismatch` — permission denied: either the cli
certificate lacks the required role, or this nico-api server predates
`GetRmsVersion` and its rbac rules reject the call before dispatch.

`api-version-mismatch` — nico-api returned `Unimplemented`; the
server predates this rpc and has no rbac layer to intercept it.

`timeout` — the connection attempt exceeded the deadline.

`error` — an unexpected error; the `message` field carries the grpc
code and detail.

## OPTIONS

`--extended`

Extended result output.

This is used by measured boot, where basic output contains just what you
probably care about, and "extended" output also dumps out all the
internal UUIDs that are used to associate instances.

`--sort-by <SORT_BY> [default: primary-id]`

Sort output by specified field

*Possible values:*

> - primary-id: Sort by the primary ID
>
> - state: Sort by state

`-h, --help`

Print help (see a summary with -h)

## Examples

```sh
nico-admin-cli backend rms status
nico-admin-cli -f json backend rms status
```

---

**Related:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
