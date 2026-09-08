# `nico-admin-cli vpc change-routing-profile`

*[Network commands](../../network.md) › [vpc](./vpc.md) › **change-routing-profile***

## NAME

nico-admin-cli-vpc-change-routing-profile - Change the routing profile
while retaining the previous VNI

## SYNOPSIS

```text
nico-admin-cli vpc change-routing-profile [--if-version-match]
[--vni] [--extended] [--sort-by]
[-h|--help] <ID> <ROUTING_PROFILE_TYPE>
```

## DESCRIPTION

Change a supported FNN VPC between configured profiles with opposite
internal settings. Core validates the destination against the tenants
access tier and retains the previous VNI until an explicit release. A
Core commit does not prove DPU/fabric convergence or guarantee that a
later change back will be accepted.

Requires --cloud-unsafe-op USERNAME before vpc. Hold attachment,
peering, deletion, and routing/profile-definition changes until the
target and directly peered DPUs and fabric routes have been
independently checked and the inactive allocation released. Core does
not enforce this hold or verify convergence.

Omit --if-version-match for interactive use: the CLI reads the current
state, shows the proposed action, and asks for confirmation before
submitting that observed version. Both stdin and stderr must be
terminals. Scripts must supply --if-version-match. Each invocation
without a version approves a new action; it does not resume an earlier
attempt.

Before mutation, the observed routing state is printed as JSON to
stderr. Each RPC attempt uses the client request timeout (300 seconds by
default, configurable with FORGE_CLIENT_REQUEST_TIMEOUT_SECS), including
connection setup and response reads. This command does not retry
mutations. After an ambiguous error, inspect vpc routing-state; the
change may have committed. If rerunning the same request, keep the
original --if-version-match and --vni selection, including omission. Do
not substitute a fresh version automatically.

Use --vni only when every serving Core supports the field: older Core
may ignore it and choose another VNI. A mismatched or lost response does
not undo a commit.

Use --format ascii-table (default), json, or yaml and --output PATH
before vpc. CSV output is unsupported. EXTERNAL and INTERNAL below are
example configured profile names, not an exhaustive list.

## OPTIONS

`--if-version-match <VERSION>`

Original observed version, for example V1-T1789080000000000; required
for scripts, omitted for interactive confirmation; keep it unchanged
when rerunning this request

`--vni <VNI>`

Exact destination VNI (1..=16777215). It must match any retained
destination allocation; otherwise it must be a free materialized entry
in the destination pool. Core rejects unavailable or mismatched values
without falling back. Omission reuses a retained destination VNI or
allocates automatically. Use only when every serving Core supports this
field: older Core may ignore it and choose another VNI

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

VPC ID whose routing profile will change

`<ROUTING_PROFILE_TYPE>`

Nonempty destination profile name from the sites Core configuration

## Examples

```sh
nico-admin-cli --cloud-unsafe-op admin vpc change-routing-profile 12345678-1234-5678-90ab-cdef01234567 EXTERNAL
nico-admin-cli --cloud-unsafe-op admin vpc change-routing-profile 12345678-1234-5678-90ab-cdef01234567 EXTERNAL --if-version-match V1-T1789080000000000
nico-admin-cli --cloud-unsafe-op admin vpc change-routing-profile 12345678-1234-5678-90ab-cdef01234567 EXTERNAL --if-version-match V1-T1789080000000000 --vni 7000
nico-admin-cli --cloud-unsafe-op admin vpc change-routing-profile 12345678-1234-5678-90ab-cdef01234567 INTERNAL --if-version-match V1-T1789080000000000
```

---

**See also:** [Network commands](../../network.md) · [CLI reference index](../../README.md)
