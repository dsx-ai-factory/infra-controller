# nico-dsx-exchange-consumer

Microservice that consumes BMS leak detection events from the BMS MQTT event bus and updates rack-level health overrides in the NICo API.

## Overview

This service bridges the DSX Exchange Event Bus with NICo's health reporting system. When leak detection events are published by the BMS, this consumer:

1. Receives metadata and value messages from MQTT topics
2. Correlates values with their metadata using point paths
3. Detects leak alerts (value = 1) and clears (value = 0)
4. Updates rack health overrides via the NICo API

## Supported Leak Types

| Point Type | Probe ID | Description |
|------------|----------|-------------|
| `LeakDetectRack` | `BmsLeakDetectRack` | Rack-level leak detection |
| `LeakSensorFaultRack` | `BmsLeakSensorFaultRack` | Leak sensor fault |
| `LeakDetectRackTray` | `BmsLeakDetectRackTray` | Rack tray leak detection |

## MQTT Topics

- **Metadata**: `BMS/v1/{pointPath}/Metadata`
- **Value**: `BMS/v1/{pointPath}/Value`

The `pointPath` is vendor-defined and may contain multiple `/` segments.

## Configuration

Configuration is loaded from TOML files and environment variables. See `example/config.example.toml`.

### Environment Variables

All config options can be set via environment variables with prefix `NICO_DSX_CONSUMER__` and `__` as separator:

```bash
NICO_DSX_CONSUMER__MQTT__ENDPOINT=mqtt.nico
NICO_DSX_CONSUMER__MQTT__PORT=1884
NICO_DSX_CONSUMER__CACHE__METADATA_TTL=1h
```

### Key Options

| Option | Default | Description |
|--------|---------|-------------|
| `mqtt.endpoint` | `mqtt.nico` | MQTT broker hostname |
| `mqtt.port` | `1884` | MQTT broker port |
| `mqtt.topic_prefix` | `BMS/v1` | Topic prefix for subscriptions |
| `mqtt.queue_capacity` | `1024` | Internal message queue size |
| `cache.metadata_ttl` | `1h` | TTL for metadata cache |
| `cache.value_state_ttl` | `1h` | TTL for deduplication cache |

## Metrics

Exposed on the configured metrics endpoint (default `:9009`):

| Metric | Type | Description |
|--------|------|-------------|
| `carbide_dsx_exchange_consumer_messages_received_total` | Counter | Number of MQTT messages received |
| `carbide_dsx_exchange_consumer_messages_processed_total` | Counter | Number of messages successfully processed |
| `carbide_dsx_exchange_consumer_messages_dropped_total` | Counter | Number of messages dropped due to queue overflow |
| `carbide_dsx_exchange_consumer_alerts_detected_total` | Counter | Number of leak alerts detected |
| `carbide_dsx_exchange_consumer_dedup_skipped_total` | Counter | Number of messages skipped due to deduplication |
| `carbide_dsx_exchange_consumer_health_report_persist_failures_total` | Counter | Number of rack health report persist failures against the Carbide API |
| `carbide_dsx_exchange_consumer_message_age_seconds` | Histogram | Age of consumed BMS value messages at processing time (consumer lag), in seconds |
| `carbide_dsx_exchange_consumer_metadata_cache_size` | Gauge | Number of entries in the metadata cache |
| `carbide_dsx_exchange_consumer_value_state_cache_size` | Gauge | Number of entries in the value state cache |
| `carbide_dsx_exchange_consumer_queue_pending_messages` | Gauge | Number of messages queued in the DSX exchange consumer's processing channel |

## Running

```bash
# With config file
cargo run -p nico-dsx-exchange-consumer -- --config config.toml

# With environment variables only
NICO_DSX_CONSUMER__MQTT__ENDPOINT=localhost cargo run -p nico-dsx-exchange-consumer
```

## Testing

```bash
cargo test -p nico-dsx-exchange-consumer
```

## Disabling the API Client

For testing without a NICo API connection, set:

```toml
[nico_api]
enabled = false
```

This logs health updates to the console instead of calling the API.
