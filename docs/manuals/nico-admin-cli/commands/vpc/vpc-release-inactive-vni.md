# `nico-admin-cli vpc release-inactive-vni`

*[Network commands](../../network.md) › [vpc](./vpc.md) › **release-inactive-vni***

## NAME

nico-admin-cli-vpc-release-inactive-vni - Release the inactive VNI after
independently verifying convergence

## SYNOPSIS

```text
nico-admin-cli vpc release-inactive-vni [--if-version-match]
<--expected-inactive-vni> <--confirm-convergence>
[--extended] [--sort-by] [-h|--help] <ID>
```

## DESCRIPTION

Release the exact inactive VNI retained after a routing-profile change.
The active VNI and VPC configuration remain unchanged; the VPC version
advances.

Requires --cloud-unsafe-op USERNAME before vpc and
--confirm-convergence. Before release, independently verify that DPUs
attached to the target VPC and its direct peers, plus fabric routes, no
longer use the inactive VNI. Keep the allocation if any consumer is
unreachable unless that consumer has been isolated. Hold concurrent
attachment, peering, and routing changes through cleanup. Core validates
allocation ownership, not dataplane convergence; a Core commit is not
proof of convergence.

Omit --if-version-match for interactive use: the CLI shows the current
state and asks for fresh confirmation that the displayed inactive
allocation is safe to release. Both stdin and stderr must be terminals.
Scripts must supply --if-version-match. Each invocation without a
version approves a new action; it does not resume an earlier attempt.

Before mutation, the observed routing state is printed as JSON to
stderr. Each RPC attempt uses the client request timeout (300 seconds by
default, configurable with FORGE_CLIENT_REQUEST_TIMEOUT_SECS), including
connection setup and response reads. This command does not retry
mutations. After an ambiguous error, inspect vpc routing-state; the
release may have committed. If rerunning the same request, keep the
original --if-version-match and --expected-inactive-vni. A stale-version
error does not prove whether release ran; do not substitute a fresh
version automatically.

Use --format ascii-table (default), json, or yaml and --output PATH
before vpc. CSV output is unsupported.

## OPTIONS

`--if-version-match <VERSION>`

Original version observed with the inactive VNI; required for scripts,
omitted for interactive confirmation; keep it unchanged when rerunning
this request

`--expected-inactive-vni <EXPECTED_INACTIVE_VNI>`

Exact inactive VNI observed with this version (1..=16777215); keep it
unchanged when rerunning this request

`--confirm-convergence`

Acknowledge that target and direct-peer DPUs and fabric routes no longer
use this VNI and concurrent attachment, peering, and routing changes are
held

`--extended`

Extended result output.

This is used by measured boot, where basic output contains just what you
probably care about, and "extended" output also dumps out all the
internal UUIDs that are used to associate instances.

`--sort-by <SORT_BY> [default: primary-id]`

Sort output by specified field

*Possible values:*

- primary-id: Sort by the primary ID

- state: Sort by state

`-h, --help`

Print help (see a summary with -h)

`<ID>`

VPC ID whose inactive allocation will be released

## Examples

```sh
nico-admin-cli --cloud-unsafe-op admin vpc release-inactive-vni 12345678-1234-5678-90ab-cdef01234567 --expected-inactive-vni 7000 --confirm-convergence
nico-admin-cli --cloud-unsafe-op admin vpc release-inactive-vni 12345678-1234-5678-90ab-cdef01234567 --if-version-match V1-T1789080000000000 --expected-inactive-vni 7000 --confirm-convergence
```

---

**See also:** [Network commands](../../network.md) · [CLI reference index](../../README.md)
