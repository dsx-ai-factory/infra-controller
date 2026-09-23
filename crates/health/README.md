# Hardware Health Service

The `carbide-health` package builds the `forge-hw-health` binary. The deployed
component is named `nico-hardware-health`.

The quickest local test runs the service against the repository's HTTPS
Redfish simulator. The supplied configuration uses one static BMC endpoint,
enables tracing and Prometheus output, and disables NICo API discovery and all
health-report sinks. No NICo API server, database, or Kubernetes cluster is
needed for this workflow.

## Prerequisites

- Run every command from the repository root.
- Install `cargo` and the Rust toolchain pinned by
  [`rust-toolchain.toml`](../../rust-toolchain.toml).
- Make sure TCP ports `1266` and `9009` are free.

The first `cargo run` builds its binary and dependencies, so it can take
several minutes.

## Run against `bmc-mock`

Start the mock BMC in one terminal:

```bash
cargo run -p bmc-mock
```

Wait for the mock BMC to report that it is listening on port `1266`.

In a second terminal, start hardware health with the local-test
configuration:

```bash
cargo run -p carbide-health --bin forge-hw-health -- \
  crates/health/example/config.bmc-mock.toml
```

The argument after `--` is the configuration-file path. Always pass this path
for the standalone test: running without it uses defaults that enable NICo API
discovery and health-report sinks.

To include debug logs from the hardware-health crate, set `RUST_LOG` when
starting it:

```bash
RUST_LOG=carbide_health=debug \
  cargo run -p carbide-health --bin forge-hw-health -- \
  crates/health/example/config.bmc-mock.toml
```

## Verify collection

Use a third terminal to query the listener on port 9009:

```bash
curl --fail --silent --show-error http://127.0.0.1:9009/livez
curl --fail --silent --show-error http://127.0.0.1:9009/metrics \
  | rg '^carbide_'
curl --fail --silent --show-error http://127.0.0.1:9009/telemetry \
  | rg '^carbide_hardware_health_'
```

The expected results are:

- `/livez` returns `ok`. This proves that the HTTP listener is running; it
  does not prove that a BMC collection succeeded.
- `/metrics` contains service-level discovery, collector, and process metrics.
- `/telemetry` contains per-sensor gauges after the first discovery and sensor
  collection pass. The default sensor poll interval is 60 seconds.

The tracing sink is also enabled, so the hardware-health terminal shows
endpoint discovery, collector events, and any collection errors.

## Inject an unhealthy sensor

`bmc-mock` supports runtime response injection. The following rule changes the
first temperature sensor in the default mock profile to a critical reading:

```bash
curl --fail --insecure --silent --show-error \
  --request POST \
  --header 'content-type: application/json' \
  --data '{
    "id": "hot-temperature",
    "selector": {
      "OdataId": "/redfish/v1/Chassis/Chassis_0/Sensors/Temperature_1"
    },
    "action": {
      "JsonMerge": {
        "Reading": 100,
        "Status": {"Health": "Critical"}
      }
    }
  }' \
  https://127.0.0.1:1266/injection/rules
```

After the next sensor poll, inspect the affected telemetry and the
hardware-health logs:

```bash
curl --fail --silent --show-error http://127.0.0.1:9009/telemetry \
  | rg 'sensor_name="Temperature 1"'
```

Remove all injection rules to restore normal mock readings:

```bash
curl --fail --insecure --silent --show-error \
  --request DELETE \
  https://127.0.0.1:1266/injection/rules
```

For a faster fault-injection loop, copy
[`config.bmc-mock.toml`](example/config.bmc-mock.toml), add this section, and
run hardware health with the copied file:

```toml
bmc_request_concurrency = 4

[collectors.sensors]
sensor_fetch_interval = "5s"
include_sensor_thresholds = true
```

## Stop the test

Press Ctrl-C in the hardware-health terminal and then in the `bmc-mock`
terminal. Mock injection rules are held in memory and are discarded when
`bmc-mock` exits.

For the complete configuration surface, including real BMC endpoints,
additional collectors, OTLP, and NICo API sinks, see
[`config.example.toml`](example/config.example.toml).
