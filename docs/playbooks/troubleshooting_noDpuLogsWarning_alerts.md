# Troubleshooting noDpuLogsWarning Alerts

The noDpuLogsWarning alert fires under the following conditions:
1. NICo has been receiving logs from the DPU ARM OS within the last 30 days
2. It has not received any nico-dpu-agent.service log events within the last 10 minutes
3. And opentelemetry-collector-prom end point running on the DPU ARM OS has been down for more than 5 minutes

The format of the alert name is "\<NICo site ID\>-noDpuLogsWarning (\<NICo site ID\> \<DPU ARM OS hostname\> nico-monitoring/nico-monitoring-(\<NICo site ID\>-prometheus warning)

## Common Causes of these alerts

1. The machine is currently being re-provisioned and taking longer than expected to complete provisioning

2. The machine is being worked on by another SRE team member. The machine might be powered off, undergoing maintenance or might have been force-deleted.

3. Issues with systemd services on the DPU ARM OS. <br/>
On the DPU ARM OS, check that node-exporter, otelcol-contrib and nico-dpu-otel-agent services are running and not reporting errors: <br/>
```bash
systemctl status node-exporter otelcol-contrib nico-dpu-otel-agent
```

4. Hostname is not picked up by the OpenTelemetry Collector service <br/>
Connect to the OpenTelemetry collector port and check that metrics are being generated and check for any other errors:
```bash
curl 127.0.0.1:9999/metrics | grep telemetry_stats
...
telemetry_stats_log_records_total{component="telemetry_stats",grouping="logs_by_component",host_name="localhost",http_scheme="http",instance="127.0.0.1:8890",job="log-stats",log_component="journald",machine_id="fm100dsekkqjprbu96gq67vd6p24rc1uqnct6dv15opjka9he3qlbk3doc0",net_host_port="8890",service_instance_id="127.0.0.1:8890",service_name="log-stats",source="telemetrystatsprocessor:0.0.1",systemd_unit="kernel"} 272
...
```
In the example above, the hostname being used by the otelcol-contrib service (host_name="localhost") is set to localhost. The host_name should be set to the hostname of the DPU ARM OS. To resolve this issue, restart the OpenTelemetry Collector service:
```bash
systemctl restart otelcol-contrib
```
Wait for 5 minutes after restarting the service and check the metrics again:
```bash
curl http://127.0.0.1:9999/metrics | grep telemetry_stats
...
telemetry_stats_log_records_total{component="telemetry_stats",grouping="logs_by_component",host_name="192-168-134-165.nico.example.org",http_scheme="http",instance="127.0.0.1:8890",job="log-stats",log_component="journald",machine_id="fm100ds5eue9nh4kmhb2mkdh1jrthqso8r3lve4jvn51biitt509s86e8gg",net_host_port="8890",service_instance_id="127.0.0.1:8890",service_name="log-stats",source="telemetrystatsprocessor:0.0.1",systemd_unit="kernel"} 20
...
```
In this example the host_name is now set to 192-168-134-165.nico.example.org.

5. Check nico-hardware-health pod for errors scraping information from the IP address for the DPU:
```bash
kubectl logs nico-hardware-health-67c95c7775-bd4mw -n nico-system --timestamps
```
If errors are being sent against the endpoint, but it is available on the network (You can ping it, ssh to the DPU ARM OS and all services appear to be running with no errors), you can attempt to restart the nico-hardware-health pod to see if this resolves the issues:
```bash
kubectl delete pod nico-hardware-health-67c95c7775-bd4mw -n nico-system
```
