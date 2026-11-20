# Known Issues

Match the symptom and deployed revision before applying a recovery. Linked
issues and fixes provide the implementation history; a fix on `main` does
not establish which released binaries include it.

## Retired SitePrefix Blocks VPC Prefix Creation

**Affected behavior:** after splitting an operator-managed fabric prefix,
such as a `/24` into two `/25` roots, the old SitePrefix can remain in
`Deleting`. A REST request to create a VPC Prefix in a replacement range
then matches both the retired and replacement roots and fails with HTTP 412.
Allocations from the replacement IP Blocks can still succeed.
[Issue #8538](https://github.com/dsx-ai-factory/infra-controller/issues/8538)
records this failure on a build without retired-root cleanup.

**Fix boundary:** [PR #8878](https://github.com/dsx-ai-factory/infra-controller/pull/8878),
merged as [commit `6d2fa6a76846`](https://github.com/dsx-ai-factory/infra-controller/commit/6d2fa6a76846d49c76314f49ae28c8f5c4c1479f),
adds cleanup of unused retired operator roots. Deploy binaries containing
that change on **all Core replicas before changing fabric-prefix
configuration**. Reconcile added roots on authoritative Core before serving
requests for them on other replicas. Do not infer that an older release
contains the fix from the issue's open or closed state.

With the fix, `Deleting` can still be expected while a child retains the
root: VPC Prefix references, including soft-deleted records, and direct
network prefixes without Ready replacement coverage must drain before
cleanup. Investigate retained children if the root persists.

On an affected older build, capture the parent SitePrefix IDs, lifecycle
states, replacement ranges, and failed request. Coordinate an upgrade or
recovery with the site owner. The issue contains a database workaround;
verify references and take a backup with the owner before considering it.
Deleting a parent row is not a general remedy for a prefix that still has
children.
