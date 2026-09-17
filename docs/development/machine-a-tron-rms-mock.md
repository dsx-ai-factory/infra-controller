# Machine-a-tron RMS Mock

machine-a-tron hosts a mock of the Rack Management Service (RMS) gRPC API, so
the NICo rack workflows that call RMS run against a simulated fleet with no RMS
deployed: scale-up fabric configuration, switch certificates, power control,
rack firmware upgrade, NVLink switch operating system (NVOS) image update,
switch password rotation, and switch factory reset. This page covers what the
mock serves, how to point NICo at it, and what it does not do. Refer to the
[crate README](https://github.com/dsx-ai-factory/infra-controller/blob/main/crates/rms-mock/README.md)
for the job model, and to
[RMS Configuration](../configuration/rms.md)
for the settings that make NICo call RMS in the first place.

## What It Serves

The mock implements `RackManager` and `RackManagerV2` with the `librms`
bindings NICo's client uses. It is mounted on machine-a-tron's bmc-mock HTTPS
listener, on the same port (`service.bmcMock.port`, default `1266`) and with
the same certificate as the Redfish, control, and UFM mock routes. It is
always present, has no enable flag, and is called only when
`nico-api.rms.apiUrl` points at it. Placement, addresses, and power state come
from the same simulated devices that answer Redfish, so RMS and Redfish never
disagree about a device.

| Area | RPCs |
| --- | --- |
| Inventory | `BatchGetNodeDeviceInfo` |
| Power | `BatchGetPowerState`, `BatchSetPowerState` |
| Firmware and NVOS | `ListFirmwareObjects`, `ApplyFirmwareObject`, `GetFirmwareJobStatus`, `ApplySwitchSystemImage`, `GetSwitchSystemImageJobStatus` |
| Scale-up fabric | V2 `ConfigureScaleUpFabricManager`, `GetJobStatus`, `GetScaleUpFabricStatus`, `BatchGetScaleUpFabricServiceStatus` |
| Switch lifecycle | `ConfigureSwitchCertificate`, `GetConfigureSwitchCertificateJobStatus`, `UpdateSwitchSystemPassword`, `BatchResetSwitchFactoryDefault` |

Every other RMS method returns gRPC `UNIMPLEMENTED`, which is what a real RMS
returns for a method it does not serve.

The mock matches the nodes a request names to simulated devices by address,
the Baseboard Management Controller (BMC) MAC first, and echoes each
`node_id` back without interpreting it. NICo sends its own row id, the
machine, switch, or power shelf id, once the device is ingested, and the
device's BMC MAC before that. A node that matches no simulated device is a
per-node failure that names the node, and the rest of the batch proceeds.
Jobs advance each time they are polled rather than with time, and every job
reaches a terminal state. Jobs complete by default; the crate's tests can set
an internal fault table that makes selected jobs fail.

## Pointing NICo at It

Set `nico-api.rms.apiUrl` to the machine-a-tron bmc-mock Service and keep
`nico-api.rms.enabled` on. Use the cross-namespace name: `nico-api` runs in
its own namespace, and a bare Service name resolves against that namespace.

```yaml
rms:
  enabled: true
  apiUrl: https://nico-machine-a-tron-mat-0-bmc-mock.<namespace>.svc.cluster.local:1266
```

The Service is named `<chart name>-<pod key>-bmc-mock`, where the chart name is
`nico-machine-a-tron` unless `nameOverride` is set and the pod key is the pod
that hosts the devices (`mat-0` in the chart defaults).
`kubectl get svc -n <namespace>` lists the Services that exist.
`nico-api.rms.enforceTls` can stay at its default: the listener is TLS only,
and its certificate is issued by the chart's `global.certificate.issuerRef`,
the same issuer that signs the `nico-api` certificate, with the Service name
as its DNS name.

In controller mode with `mat-k8s-controller.gateway.enabled: true`, set
`nico-api.rms.apiUrl` to the protocol gateway Service instead:
`https://<release>-mat-k8s-controller-gateway.<namespace>.svc.cluster.local:8443`,
where `8443` is the default `mat-k8s-controller.gateway.port`. The gateway
forwards each request to the pod that simulates the rack it names, so the racks
of every pod are reachable through one endpoint. Its certificate comes from the
same `global.certificate.issuerRef`, so `nico-api.rms.enforceTls` can stay at
its default here as well. Refer to the
[Protocol gateway](https://github.com/dsx-ai-factory/infra-controller/blob/main/helm/charts/nico-machine-a-tron/README.md#protocol-gateway)
section of the chart README for the gateway settings, and to the
[gateway README](https://github.com/dsx-ai-factory/infra-controller/blob/main/crates/mat-protocol-gateway/README.md#rms-routing)
for the RPCs it routes and how its job ids differ from the mock's.

<Warning>
On a site that also runs a real RMS, pointing `apiUrl` at machine-a-tron
diverts every RMS-backed operation, for real racks as well as simulated ones,
to the mock. Use it only on simulation-only sites.
</Warning>

## Limitations

- Job state is in memory and lost on restart. A poll for a job the new
  process never issued reads completed on `GetJobStatus` and
  `GetConfigureSwitchCertificateJobStatus`, and answers `RETURN_CODE_FAILURE`
  on `GetFirmwareJobStatus` and `GetSwitchSystemImageJobStatus`, as the RMS
  API specifies.
- Job retention differs from RMS, which keeps completed and failed jobs for
  24 hours by default and caps its job tracker at 10,000 records. The mock
  forgets a job once it has reported completed, keeps the most recent 4,096
  failed node jobs, and reports a failed job it has forgotten as completed.
- Firmware, NVOS images, passwords, and factory resets change nothing on the
  simulated devices: a device's Redfish firmware inventory reads the same
  after an apply, and a factory reset only clears the switch's fabric primary
  role in the mock, once the switch's job completes.
- `ListFirmwareObjects` reports object ids only, one by default. The
  `[rms_mock]` table of the machine-a-tron configuration can name others, and
  the chart exposes no setting for it.
- `GetNodeFirmwareInventory` is not served, so NICo's RMS power-shelf backend
  reports an error per shelf when it lists shelf firmware. The rack firmware
  upgrade does not use it.
- Placement is reported for compute and switch trays only, and power shelves
  report none.
- A single pod's mock serves only its own racks. In controller mode, the
  protocol gateway fronts every pod behind one Service. Refer to the
  [Protocol gateway](https://github.com/dsx-ai-factory/infra-controller/blob/main/helm/charts/nico-machine-a-tron/README.md#protocol-gateway)
  section of the chart README.
- Jobs carry no timestamps.
