# rms-mock

A mock of the Rack Management Service (RMS) gRPC API for simulated racks.
machine-a-tron mounts it on its bmc-mock listener, so the HTTPS endpoint that
serves the simulated BMCs also answers `RackManager` and `RackManagerV2`
calls. The mock keeps no inventory of its own: it reports node placement from
the same simulated hardware the Redfish chassis are built from, so the two
cannot disagree. RPCs outside its scope return `UNIMPLEMENTED`.

## Served RPCs

Besides `GetVersion` and `BatchGetNodeDeviceInfo`, the mock serves the RPCs a
rack passes on its way to ready: `ConfigureSwitchCertificate` with
`GetConfigureSwitchCertificateJobStatus`, the V2
`ConfigureScaleUpFabricManager` with `GetJobStatus`, `GetScaleUpFabricStatus`
and `BatchGetScaleUpFabricServiceStatus`. Nothing is installed on a simulated
switch. The mock elects one fabric-manager primary per rack, which is the one
switch that reads back enabled: the requested primary when it is one of the
rack's simulated switches, otherwise the one lowest in the rack, with node id
breaking ties. A node the request names but no simulated device answers for is
a per-node failure: the batch fails, the node's result says why, and no job is
issued for it.
`ConfigureScaleUpFabricManager` has no per-node results, so when none of its
switches match, the job it returns fails and names them. Jobs advance each
time they are polled rather than with time, and a poll for a job id this
process never issued reports it completed.

## Pointing NICo at the mock

NICo reaches RMS through the `nico-api` chart's `rms` values. Set
`nico-api.rms.apiUrl` to machine-a-tron's bmc-mock Service, which the
`nico-machine-a-tron` chart exposes as
`https://<release>-bmc-mock.<namespace>.svc.cluster.local:<service.bmcMock.port>`,
and keep `nico-api.rms.enabled` on. `nico-api.rms.enforceTls` and the
certificate values apply exactly as they do for a real RMS, since the mock is
served with the listener's own TLS material. Nothing has to be enabled on the
machine-a-tron side: the services are always mounted. The optional
`[rms_mock]` table in the machine-a-tron configuration sets `version_string`,
which `GetVersion` reports.
