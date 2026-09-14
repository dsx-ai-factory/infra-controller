# rms-mock

A mock of the Rack Management Service (RMS) gRPC API for simulated racks.
machine-a-tron mounts it on its bmc-mock listener, so the HTTPS endpoint that
serves the simulated BMCs also answers `RackManager` and `RackManagerV2`
calls. The mock keeps no inventory of its own: it reports node placement from
the same simulated hardware the Redfish chassis are built from, so the two
cannot disagree. RPCs outside its scope return `UNIMPLEMENTED`.

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
