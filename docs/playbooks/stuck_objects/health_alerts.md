# Health Alerts and Overrides

Use this playbook when a host or DPU is blocked by health, or when an operator
needs to understand whether an override is safe.

## Inspect Health

Aggregate managed-host health:

```bash
nico-admin-cli managed-host show <host-machine-id>
```

Per-source health reports:

```bash
nico-admin-cli machine health-report show <machine-id>
```

JSON output for scripting:

```bash
nico-admin-cli -f json machine health-report show <machine-id>
```

## Health Sources

Health is built from multiple report sources.

| Source | Examples |
|---|---|
| Hardware health | BMC sensors, chassis status, leak signals, Redfish state. |
| DPU agent | DPU heartbeat, HBN/BGP state, DHCP relay, network config application. |
| Validation | machine validation, SKU validation, discovery checks. |
| Rack or infrastructure health | rack-level inputs when configured. |
| Overrides | operator or workflow-created health reports. |

## Classifications

Classifications determine operational impact.

| Classification | Impact |
|---|---|
| `PreventAllocations` | Host should not receive new work. |
| `PreventHostStateChanges` | Host should not move through some lifecycle states while the condition is unresolved. |
| `SuppressExternalAlerting` | Host should be excluded from fleet-health alerting calculations. |
| `ExcludeFromStateMachineSla` | Host should not count against SLA while intentionally held. |
| `StopRebootForAutomaticRecoveryFromStateMachine` | NICo should not automatically reboot the host during state-machine recovery. |

## Common Probe Areas

| Area | Example probes or symptoms |
|---|---|
| Machine validation | failed DCGM, CUDA sample, SKU validation, inventory mismatch. |
| Site Explorer and BMC | endpoint exploration failure, Redfish timeout, missing credentials. |
| Hardware sensors | fan, power, temperature, voltage, leak detection. |
| DPU agent | `HeartbeatTimeout`, `BgpStats`, `ServiceRunning`, DHCP relay/server. |
| InfiniBand | port down, missing or unexpected P_Keys, cleanup pending. |
| Rack and power | rack health input, power shelf state, switch state. |

## Overrides

Use overrides sparingly and always include an incident or maintenance reason.

Mark a false positive healthy:

```bash
nico-admin-cli machine health-override add <machine-id> \
  --template mark-healthy \
  --message "false positive INC-123"
```

Hold a host out of allocation:

```bash
nico-admin-cli machine health-override add <machine-id> \
  --template out-for-repair \
  --message "INC-123 replacing hardware"
```

Remove an override:

```bash
nico-admin-cli machine health-override remove <machine-id> <source-name>
```

## Guidance

- Do not override a probe until the owner and impact are understood.
- Do not use `mark-healthy` to bypass unknown hardware or DPU failures.
- Always remove temporary overrides during incident closeout.
- Prefer maintenance mode when the goal is to suppress SLA noise during
  investigation.

## Related References

- [Health Checks and Health Aggregation](../../architecture/health_aggregation.md)
- [Health Probe IDs](../../architecture/health/health_probe_ids.md)
- [Health Alert Classifications](../../architecture/health/health_alert_classifications.md)
