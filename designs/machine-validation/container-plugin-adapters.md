# Machine Validation Container Plugin Adapters

**Status:** Agreed design and implementation reference, updated 2026-11-17. Extends the
[pluggable framework design](pluggable-machine-validation-framework.md).

## Purpose and selection

NICo already supports container plugins through a standard input/output contract.
Dedicated adapters let NICo integrate containers such as NVIDIA HealthValidation
without putting their invocation rules or output formats into the core framework.

Each immutable plugin revision carries an optional `container_plugin_adapter`.
`DSXHealthValidation` selects the HealthValidation adapter; omission selects the
standard NICo container contract. Unknown adapter names are rejected at
registration. Multiple plugins with different adapters can coexist at one site.
The adapter choice participates in qualification and verification alongside the
image digest, entrypoint, arguments, and parameters.

`type` remains `container`, both explicitly and by default. Adapter selection
belongs to each plugin revision; `[machine_validation_config]` retains site-wide
policy. Adapter selection does not introduce another execution type.

## Responsibilities and execution

| Component | Responsibility |
| --- | --- |
| Framework | Select the adapter; own scheduling, qualification, lifecycle, persistence, and reporting. |
| Adapter | Prepare the invocation and input files, identify dependencies, invoke through the shared runtime, and return a normalized result. |
| Shared runtime | Enforce site-approved access; own image pulls, container execution, timeouts, cleanup, and log capture. |

The framework passes a generic request containing machine identity, revision
settings, parameters, and execution limits. The adapter prepares the container
invocation and interprets its output. Results and logs return through existing
framework interfaces; adapters do not update controller state or persistence
directly. One initial interface and dispatch integration enables subsequent
adapters to be added through their implementations and registry entries.

## Admin CLI

```sh
nico-admin-cli machine-validation plugins create \
  --name dsx-health \
  --type container \
  --container-plugin-adapter DSXHealthValidation \
  --image 'nvcr.io/0837451325059433/components-dev/health-validation@sha256:abd01453dccba79370e6ac9e88552669b4b5ec5b3bedeebe604792fe779889d1' \
  --entrypoint /health-validation \
  --arg run \
  --arg=--execution-context \
  --arg OPERATOR_INFRA \
  --parameters '{"gpu_count":4}' \
  --context OnDemand \
  --platform GB200
```

`--type` is optional and defaults to `container`. Repeated `--entrypoint` values
form the command vector; repeatable `--arg` values append ordered arguments.
The standard contract preserves the image ENTRYPOINT and supplies the vector
after the image reference. DSX explicitly overrides the executable with the
first vector element. Preserve this distinction for existing revisions and
execute without a shell. Command changes require a new revision and qualification.

`--parameters` carries structured input, separate from command arguments. The
standard adapter puts it into NICo's `input.json`; the DSX adapter maps optional
`gpu_count` to `checks.gpu_count.expected_gpu_count` in generated operator
configuration. The adapter appends managed arguments such as `--node-id` and
`--config` and rejects conflicting user arguments.

## HealthValidation result contract

Only `overall_severity` determines health: `PASS`, `INFO`, and `WARNING` pass;
`CRITICAL` and `UNKNOWN` fail. Preserve warning severity. Other report fields,
including schema version and individual checks, are ignored for interpretation;
the raw report remains available for diagnostics. Exits `0` and `1` permit
interpretation. Missing or unrecognized severity, malformed JSON, other process
exits, timeouts, and truncated output are execution errors.

## Runtime policy and acceptance

Registration and enablement require site approval for named adapters through
`allowed_container_plugin_adapters`, which defaults to an empty list.
`DSXHealthValidation` declares all-GPU access, container root with capabilities
dropped, host network/PID namespaces, and host service sockets. The additional
site approvals `allow_plugin_host_network`, `allow_plugin_host_pid`, and
`allow_plugin_host_sockets` default to false; all three are required for this
adapter's static profile. Missing approvals make an approved-adapter site
configuration invalid and block registration/enablement.

The adapter discovers host `journalctl` and DCGM 4 (`libdcgm.so.4` through
`ldconfig -p`), mounts their non-glibc dependencies read-only, and sets
`LD_LIBRARY_PATH`. Host networking reaches `nv-hostengine`; host PID access
provides node-wide process visibility. A configured GPU-count override also
requires the host journald socket for native audit entries. Socket mounts are
explicit service access: a read-only bind still permits sending requests. The
runtime handles generic declarations, while DCGM discovery and audit behavior
remain inside the DSX adapter. No privileged or writable host-root access is
required.

The qualification image is the private NGC image built from HealthValidation
PR #269's merge commit `412b7a6283c1`, pinned in the example above. It accepts
`--execution-context` and defaults to `TENANT_GUEST`; NICo qualification selects
`OPERATOR_INFRA` explicitly. Context remains a configurable command argument.
This profile targets DCGM 4 images; it does not auto-detect or downgrade access
for the earlier `0.2.0-rc1` image.

Validate on Scout using the tested GPU and journal setup. A valid `CRITICAL`
verdict must produce a completed, failed validation rather than an execution
error. Standard container plugins must retain their existing contract and
qualification, verification, access-approval, and enablement workflow.

Full Scout qualification remains pending. Upgrade API instances and Scouts
before introducing adapter revisions. Record the image digest, software and
hardware versions, actual-profile GPU/journal access, persisted verdicts, logs,
and timeout cleanup as described in the
[operator qualification guide](../../docs/provisioning/machine-validation.md#qualification-evidence).
