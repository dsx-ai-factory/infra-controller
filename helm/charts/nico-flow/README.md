# nico-flow Helm Chart

This chart deploys NICo Flow, the task, policy, and automation service, as a
standalone release in the `flow` namespace. `helm-prereqs/setup.sh` installs it
in phase 7h; it is disabled by default in the umbrella chart
(`nico-flow.enabled: false`).

## Runtime configuration (`flowConfig`)

Flow reads runtime settings from `/etc/flow/flowconfig.yaml`. The chart renders
that file from the `flowConfig` values into the `flow-config-files` ConfigMap
and mounts it read-only into the Flow container. The Deployment carries a
checksum of the rendered file, so changing any value rolls the Flow pod.

The defaults below equal the built-in defaults in
`rest-api/flow/internal/config/config.go`, so an unchanged block behaves the
same as running Flow without a config file.

| Value | File key | Type | Default | Description |
|-------|----------|------|---------|-------------|
| `flowConfig.inventoryRunFrequency` | `inventory_run_frequency` | duration | `1m` | Interval between inventory synchronisation runs. Must be greater than zero. |
| `flowConfig.disableInventory` | `disable_inventory` | boolean | `false` | Skip the periodic inventory synchronisation job. |
| `flowConfig.leakDetectionInterval` | `leak_detection_interval` | duration | `1m` | Interval between leak detection runs. Must be greater than zero. |
| `flowConfig.disableLeakDetection` | `disable_leak_detection` | boolean | `false` | Skip the periodic leak detection job. |

The file format also accepts `grpc_timeout`, but Flow does not consume it, so
the chart does not expose it; a future chart version can add it once Flow does.

### Durations

Durations use the Go `time.ParseDuration` format: one or more decimal numbers,
each followed by a unit from `ns`, `us`, `ms`, `s`, `m`, or `h`, for example
`30s`, `1m`, `1.5h`, or `1h30m`. Both intervals must be greater than zero. The
chart checks this at render time, so `helm install`, `helm upgrade`, and
`helm template` fail before anything reaches the cluster. The location names
`deployment.yaml` because the Deployment's checksum annotation renders the
ConfigMap first:

```text
Error: execution error at (nico-flow/templates/deployment.yaml:...): flowConfig.inventoryRunFrequency must be a Go duration string with a unit, such as 30s, 1m, or 1h30m; got "30"
Error: execution error at (nico-flow/templates/deployment.yaml:...): flowConfig.leakDetectionInterval must be greater than zero; got "0s"
```

If a file bypasses the chart, Flow itself rejects a bare number at startup
with `Invalid configuration file /etc/flow/flowconfig.yaml: ...` and a zero
interval with `invalid inventory sync interval: interval must be positive, got
0s` (or `invalid leak detection interval: ...`).

### Booleans

`disableInventory` and `disableLeakDetection` must be YAML booleans (`true` or
`false`). Strings are rejected at render time, including `--set-string` values
and `--set flowConfig.disableInventory=yes` (Helm passes `yes` as a string). In
a values file, Helm's YAML 1.1 parser reads an unquoted `yes` as `true`, so
prefer `true` and `false` there. Omitting a key from a values file keeps the
chart default because Helm merges values, but setting it to `null` (including
`--set flowConfig.disableInventory=null`) removes the key, and the chart rejects
a missing key instead of falling back to the default.

```text
Error: execution error at (nico-flow/templates/deployment.yaml:...): flowConfig.disableInventory must be a boolean (true or false); got "true"
Error: execution error at (nico-flow/templates/deployment.yaml:...): flowConfig.disableInventory must be true or false; the key is missing or null
```

Override values on the existing release, for example:

```bash
helm upgrade flow ./helm/charts/nico-flow \
  --namespace flow \
  --reuse-values \
  --set flowConfig.leakDetectionInterval=5m \
  --set flowConfig.disableInventory=true
```

Confirm the rendered file after the rollout:

```bash
kubectl get configmap flow-config-files -n flow -o jsonpath='{.data.flowconfig\.yaml}'
```

## Upgrading from 0.2.x

In chart 0.2.x, `flowConfig` was an optional raw string holding the whole file,
and the ConfigMap was only created when it was set. From 0.3.0, `flowConfig` is
a structured map with the keys above, and the ConfigMap is always rendered. A
raw-string `flowConfig` fails at render time with `flowConfig must be a map of
settings ...; see the nico-flow README section "Upgrading from 0.2.x"`. Convert
an existing override before upgrading:

```yaml
# 0.2.x
flowConfig: |
  leak_detection_interval: 5m
  disable_inventory: true

# 0.3.0
flowConfig:
  leakDetectionInterval: 5m
  disableInventory: true
```

## Testing

```bash
helm lint helm/charts/nico-flow
helm unittest helm/charts/nico-flow
```
