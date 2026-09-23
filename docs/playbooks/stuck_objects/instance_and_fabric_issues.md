# Instance and Fabric Issues

Use this playbook when an instance is stuck, capacity is unavailable, or GPU,
InfiniBand, or NVLink state blocks assignment or release.

## Start with Instance and Host State

```bash
nico-admin-cli instance show <instance-id>
nico-admin-cli managed-host show <host-machine-id>
nico-admin-cli -f json machine show <host-machine-id>
```

Look for:

- assigned host
- host state and substate
- health alerts
- validation failures
- fabric or network config wait reasons

## GPU Issues

NICo allocates whole bare-metal hosts, not individual GPUs. GPU-related issues
usually appear as allocation blocks, validation failures, health alerts, or
tenant-reported in-life failures.

Check:

```bash
nico-admin-cli compute-allocation show
nico-admin-cli compute-allocation show --id <allocation-id>
nico-admin-cli machine hardware-info show --machine <host-machine-id>
nico-admin-cli machine health-report show <host-machine-id>
nico-admin-cli machine-validation results show --machine <host-machine-id>
```

Useful metrics:

- `carbide_gpus_total_count`
- `carbide_gpus_usable_count`
- `carbide_gpus_in_use_count`

Common causes:

| Symptom | Likely cause |
|---|---|
| No usable GPU hosts | all matching hosts assigned, unhealthy, or failed validation. |
| Validation failed | DCGM, CUDA sample, SKU validation, or inventory mismatch. |
| Tenant reports GPU problem after `Ready` | tenant OS, driver, CUDA, fabric, or workload configuration. |
| Release is blocked | cleanup or fabric detach state has not completed. |

## InfiniBand Issues

InfiniBand problems usually show up as partition programming failures, UFM sync
drift, missing P_Keys, unexpected P_Keys, or cleanup pending alerts.

Check:

```bash
nico-admin-cli ib-partition show
nico-admin-cli ib-partition show <partition-id>
nico-admin-cli machine health-report show <host-machine-id>
```

Look for:

- `IbPortDown`
- `IbCleanupPending`
- missing P_Keys
- unexpected P_Keys
- UFM API errors

## NVLink Partition Issues

NVLink issues usually appear during placement, attach, detach, cleanup, or
domain health checks.

Check:

```bash
nico-admin-cli nvl-logical-partition show
nico-admin-cli nvl-partition show
nico-admin-cli nvl-domain show
nico-admin-cli machine nvlink-info show <host-machine-id>
nico-admin-cli nvlink-nmxc-endpoints show
```

Common causes:

| Symptom | Likely cause |
|---|---|
| no `nvlink_info` | the NVLink partition manager has not yet populated the machine; check its NMX-C connectivity and reconcile metrics. |
| NMX-C connect error | TLS, endpoint, or network issue. |
| partition cleanup pending | stale binding or delayed fabric observation. |
| placement failure | topology, domain health, or requested instance shape mismatch. |

## Release and Cleanup

On termination, cleanup may still need to detach network, InfiniBand, NVLink, or
other fabric state before the host can return to the pool.

Do not force delete first. Confirm what cleanup step is blocked, then fix the
fabric or controller dependency that owns that cleanup.
