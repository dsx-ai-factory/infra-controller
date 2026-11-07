# Diagnostic Tools

Use this page as a command reference while investigating a stuck object or site
operation incident.

## CLI Setup

`nico-admin-cli` is the primary operator CLI for NICo site state.

```bash
cargo build -p nico-admin-cli
```

Common connection options:

| Option | Meaning |
|---|---|
| `-a <url>` | NICo API gRPC endpoint. |
| `-f json` | JSON output for scripting. |
| `API_URL` | Environment variable for the API URL. |
| `https_proxy=socks5://...` | SOCKS5 proxy when reaching the site from off-site. |

## Common Commands

| Need | Command |
|---|---|
| API version or reachability | `nico-admin-cli version`, `nico-admin-cli ping` |
| All managed hosts | `nico-admin-cli managed-host show --all` |
| One managed host | `nico-admin-cli managed-host show <host-machine-id>` |
| Machine event history | `nico-admin-cli -f json machine show <machine-id>` |
| Debug bundle | `nico-admin-cli managed-host debug-bundle <machine-id> --start-time <time>` |
| Maintenance mode | `nico-admin-cli managed-host maintenance on --host <host-machine-id> --reference "INC-123"` |
| Health reports | `nico-admin-cli machine health-report show <machine-id>` |
| Site Explorer reports | `nico-admin-cli site-explorer get-report all` |
| Redfish browse | `nico-admin-cli redfish browse --address <bmc-ip> <uri>` |
| Network segments | `nico-admin-cli network-segment show` |
| InfiniBand partitions | `nico-admin-cli ib-partition show` |
| NVLink partitions | `nico-admin-cli nvl-partition show` |
| Compute allocation | `nico-admin-cli compute-allocation show` |

## Query State History

```bash
nico-admin-cli -a <api-url> -f json machine show <machine-id>
```

Use this to inspect state transitions, timestamps, and handler outcomes.

## Query Health

Aggregate state:

```bash
nico-admin-cli managed-host show <host-machine-id>
```

Per-source health reports:

```bash
nico-admin-cli machine health-report show <machine-id>
```

JSON output:

```bash
nico-admin-cli -f json machine health-report show <machine-id>
```

## Add or Remove Health Overrides

Mark a false positive healthy for allocation:

```bash
nico-admin-cli machine health-override add <machine-id> \
  --template mark-healthy \
  --message "false positive INC-123"
```

Hold a host out of allocation:

```bash
nico-admin-cli machine health-override add <machine-id> \
  --template out-for-repair \
  --message "INC-123"
```

Remove an override:

```bash
nico-admin-cli machine health-override remove <machine-id> <source-name>
```

## Kubernetes Logs

Namespace names vary by site and deployment generation. Confirm the namespace
before copying commands.

```bash
kubectl get ns
kubectl -n <nico-namespace> get pods
kubectl -n <nico-namespace> logs deploy/nico-api --tail=500 | grep <machine-id>
```

Common log sources:

| Component | What to look for |
|---|---|
| `nico-api` | State transitions, Redfish errors, Vault failures, health reports, gRPC errors. |
| `nico-dhcp` | DHCP lease and discovery issues. |
| `nico-pxe` | PXE and HTTP boot artifact requests. |
| Site Explorer | BMC endpoint discovery and scrape failures. |
| DPF operator | DPU provisioning custom resources and operator status. |
| `nico-dpu-agent` | DPU heartbeat, BGP, HBN, DHCP relay, and applied network config. |

## Loki and Grafana

Use a debug bundle when possible:

```bash
GRAFANA_AUTH_TOKEN=<token> \
nico-admin-cli managed-host debug-bundle <machine-id> \
  --start-time <time> \
  --grafana-url https://<grafana-host>
```

Use `logcli` directly when a bundle is not enough:

```bash
logcli --addr=http://localhost:3100 \
  --org-id=<org-id> \
  query \
  --timezone=UTC \
  --from="<YYYY-MM-DDTHH:MM:SSZ>" \
  --to="<YYYY-MM-DDTHH:MM:SSZ>" \
  --limit 0 \
  --forward \
  '{k8s_container_name="<container-name>"}'
```

## On-Metal Host and DPU Logs

| Location | Use |
|---|---|
| `/var/log/nico/nico-scout.log` | Host discovery scout during ingestion. |
| `journalctl -u nico-dpu-agent` | DPU agent heartbeat, network config, BGP, HBN, and service health. |
| DPU BMC or rshim console | Use when SSH to the DPU fails. |

## Metrics

Metric names may retain the historical `carbide_*` prefix even when the service
name is now NICo.

| Metric | Use |
|---|---|
| `carbide_machines_per_state` | Count hosts by state. |
| `carbide_machines_time_in_state_seconds` | Average time in each state. |
| `carbide_machines_per_state_above_sla` | Hosts past state SLA. |
| `carbide_hosts_health_status_count` | Healthy vs alerting hosts. |
| `carbide_hosts_health_overrides_count` | Active overrides. |
| `carbide_dpus_up_count` / `carbide_dpus_healthy_count` | DPU agent presence and health. |
| `carbide_endpoint_exploration_*` | BMC discovery health. |
| `carbide_available_ips_count` | DHCP or IP pool pressure. |
| `carbide_gpus_usable_count` | GPU capacity for allocation. |
| `carbide_api_vault_requests_failed_total` | Credential pipeline failures. |
