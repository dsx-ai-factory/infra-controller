# Collecting Machine Diagnostic Information using nico-admin-cli

This guide describes how to use the `nico-admin-cli` debug bundle command to collect diagnostic information for troubleshooting machines managed by NVIDIA Infra Controller (NICo). The command creates a ZIP file containing logs, health data, and machine state information.

## What the Command Does

The debug bundle command collects data from two sources:

1. **Grafana (Loki)** (optional): Fetches logs using Grafana's Loki datasource
   - Host machine logs
   - NICo API logs
   - DPU agent logs
   - ***Note:*** Log collection is skipped if `--grafana-url` is not provided

2. **NICo API**: Fetches machine information
   - Health alerts for the specified time range
   - Health alert overrides
   - Site controller details (BMC information)
   - Machine state and validation results

## ZIP File Contents

The generated ZIP file contains:

- Host machine logs from Grafana
- NICo API container logs from Grafana
- DPU agent logs from Grafana
- Machine health alerts for the time range
- Health alert overrides (if any are configured)
- Site controller details (BMC IP, port, and other controller information)
- Machine state, SLA status, reboot history, and validation test results
- Summary metadata with Grafana query links

## Prerequisites

Before running the debug bundle command, ensure you have:

### 1. Access to `nico-admin-cli`

You need `nico-admin-cli` installed with valid client certificates to connect to the NICo API. Refer to your NICo installation documentation for setup instructions.

### 2. Grafana Authentication Token (Optional)

***Note:*** This is only required if you want to collect logs. If `--grafana-url` is not provided, log collection is skipped.

Set the `GRAFANA_AUTH_TOKEN` environment variable:

```bash
export GRAFANA_AUTH_TOKEN=<your-grafana-token>
```

This token is used to authenticate with Grafana and fetch logs from the Loki datasource.

### 3. Network Proxy (if needed in your environment)

If you are running from an environment that requires a SOCKS proxy, set the proxy:

```bash
export https_proxy=socks5://127.0.0.1:8888
```

***Note:*** When running from inside the cluster (nico-api pod), the proxy is not required.

### 4. Required Information

- **Machine ID**: The host machine ID you want to collect debug information for
- **Time Range**: Start and end times for log collection
- **Grafana URL** (optional): Your Grafana base URL (e.g., `https://grafana.example.com`)
- **Output Path**: Directory where the ZIP file will be saved

## Running the Debug Bundle Command

### Command Syntax

```bash
nico-admin-cli -a <API_URL> mh debug-bundle <MACHINE_ID> --start-time <TIME> [--grafana-url <URL>] [--end-time <TIME>] [--output-path <PATH>] [--batch-size <SIZE>] [--utc]
```

### Parameters

**Required:**

- `-a <API_URL>`: NICo API endpoint
  - From outside cluster: `https://<your-nico-api-url>/`
  - From inside cluster: `https://127.0.0.1:1079`
- `<MACHINE_ID>`: The machine ID to collect debug information for
- `--start-time <TIME>`: Start time in format `HH:MM:SS` or `YYYY-MM-DD HH:MM:SS`

**Optional:**

- `--grafana-url <URL>`: Grafana base URL (e.g., `https://grafana.example.com`). If not provided, log collection is skipped.
- `--end-time <TIME>`: End time in format `HH:MM:SS` or `YYYY-MM-DD HH:MM:SS` (default: current time)
- `--output-path <PATH>`: Directory where the ZIP file will be saved (default: `/tmp`)
- `--batch-size <SIZE>`: Batch size for log collection (default: `5000`, max: `5000`)
- `--utc`: Interpret start-time and end-time as UTC instead of local timezone

### Examples

**With Grafana configured (collect logs):**

```bash
GRAFANA_AUTH_TOKEN=<your-token> \
https_proxy=socks5://127.0.0.1:8888 \
nico-admin-cli -a https://<your-nico-api-url>/ mh debug-bundle \
  <machine-id> \
  --start-time 06:00:00 \
  --grafana-url https://grafana.example.com
```

**With all options specified:**

```bash
GRAFANA_AUTH_TOKEN=<your-token> \
https_proxy=socks5://127.0.0.1:8888 \
nico-admin-cli -a https://<your-nico-api-url>/ mh debug-bundle \
  <machine-id> \
  --start-time 06:00:00 \
  --end-time 18:00:00 \
  --output-path /custom/path \
  --grafana-url https://grafana.example.com
```

**Without Grafana (metadata only):**

```bash
nico-admin-cli -a https://<your-nico-api-url>/ mh debug-bundle \
  <machine-id> \
  --start-time 06:00:00
```

## Understanding the Output

When you run the debug bundle command, it shows progress through multiple steps:

```
   Creating debug bundle for host: <machine-id>

Step 0: Fetching Loki datasource UID...
   Fetching Loki datasource UID from Grafana: https://grafana.example.com

Step 1: Downloading host-specific logs...
   Processing batch 1/1 (500 records)

Step 2: Downloading nico-api logs...
   Processing batch 1/1 (250 records)

Step 3: Downloading DPU agent logs...
   Processing batch 1/1 (74 records)

Step 4: Fetching health alerts...
   Alerts: 42 records collected

Step 5: Fetching health alert overrides...
   Overrides: 2 overrides collected

Step 6: Fetching site controller details...
   Fetching BMC information for machine...

Step 7: Fetching machine info...
   Fetching machine state and metadata...

Debug Bundle Summary:
   Host Logs: 500 logs collected
   NICo-API Logs: 250 logs collected
   DPU Agent Logs: 74 logs collected
   Health Alerts: 42 records
   Health Alert Overrides: 2 overrides
   Site Controller Details: Collected
   Machine State Information: Collected
   Total Logs: 824

Step 8: Creating ZIP file...

ZIP created: /tmp/20241121060000_<machine-id>.zip
```
