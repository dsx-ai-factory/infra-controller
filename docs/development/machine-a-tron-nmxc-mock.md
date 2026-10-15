# Machine-a-tron NMX-C Mock

machine-a-tron hosts a mock of the NMX-C (NVLink controller) gRPC API, so the
NICo NVLink workflows that call NMX-C run against a simulated fleet with no
NVLink switches deployed: the partition monitor's domain discovery, factory
partition removal, tray holding partitions, and tenant partition membership.
This page covers what the mock serves, how NICo reaches it, and what it does
not do. Refer to the
[crate README](https://github.com/dsx-ai-factory/infra-controller/blob/main/crates/nmxc-mock/README.md)
for how the mock picks the rack that answers a request and for the
`[nmxc_mock]` keys that set its version string and factory partition, and to
[NVLink Partitioning](../manuals/nvlink_partitioning.md) for the settings that
make NICo call NMX-C in the first place.

## What It Serves

The mock implements `NMX_Controller` with the `libnmxc` bindings NICo's client
uses. It is mounted on machine-a-tron's bmc-mock HTTPS listener, on the same
port (`service.bmcMock.port`, default `1266`) and with the same certificate as
the Redfish, control, UFM, and RMS routes. It is always present and has no
enable flag. Each simulated rack is one NVLink domain with a UUID derived from
the rack id and an independent, in-memory partition table; the GPUs it reports
are the ones the rack's compute trays report through discovery, identified by
the same fabric GUID, so NICo joins partition membership to its machines.

| Area | RPCs |
| --- | --- |
| Session | `Hello` |
| Domain | `GetDomainProperties`, `GetComputeNodeCount`, `GetComputeNodeInfoList`, `GetGpuInfoList`, `GetSwitchNodeCount`, `GetSwitchNodeInfoList` |
| Partitions | `GetPartitionCount`, `GetPartitionIdList`, `GetPartitionInfoList`, `CreatePartition`, `DeletePartition`, `AddGpusToPartition`, `RemoveGpusFromPartition` |

Every other method, including `Subscribe`, returns gRPC `UNIMPLEMENTED`. A
rejected partition operation is reported the way a real controller reports it,
in `server_header.return_code`, not as a gRPC status.

Each domain boots with the factory partition a real controller has, id `32766`
named `Default`, holding every GPU; NICo deletes it before it provisions
partitions of its own. A GPU belongs to at most one partition, and partition
ids are allocated from 1.

## How NICo Reaches It

A real deployment has one NMX-C per rack, reached at a switch NVOS address on
port 9370. NICo resolves that address from a switch of the rack that is
`Ready` and whose Fabric Manager status reports
`CONTROL_PLANE_STATE_CONFIGURED`, so nothing names the mock directly. In
controller mode, `mat-k8s-controller` publishes every simulated switch's NVOS
lease as the `externalIPs` of a `mat-nvos-*` Service that forwards port 9370
to the bmc-mock listener, and the mock selects the rack from the address each
request was sent to. The NVOS network therefore has the same
[requirements](https://github.com/dsx-ai-factory/infra-controller/blob/main/helm/charts/nico-machine-a-tron/README.md#requirements)
as the BMC network. Override mode has no per-switch routing, so NICo cannot
reach the mock there.

Because NICo dials by address, it verifies the mock's certificate against one
name. The `nico-machine-a-tron` chart adds `mat-mock.nvidia.com` to every pod
certificate as an extra SAN (`certificate.extraDnsNames`), and the chart's
issuer is the same `global.certificate.issuerRef` that signs the `nico-api`
certificate, so NICo needs only the CA and that name. The simulation profile
`helm-prereqs/values/nico-core-simulation.yaml` and the Tilt values carry the
block:

```toml
[nvlink_config]
enabled = true
allow_insecure = false
nmx_c_tls_ca_cert_path = "/var/run/secrets/nico-roots/ca.crt"
nmx_c_tls_authority = "mat-mock.nvidia.com"
```

`allow_insecure` must be set explicitly: the field has no default, and it must
stay `false`, because `true` makes NICo dial plaintext, which the TLS-only
listener does not serve. No client certificate is configured; the mock does
not request one.

<Warning>
On a site that also has real NVLink switches, this configuration applies to
every rack NICo manages. Use it only on simulation-only sites.
</Warning>

## Limitations

- Partition state is in memory and lost when machine-a-tron restarts. Each
  domain then boots with its factory partition again, and NICo's partition
  monitor deletes it and re-creates the tray holding partitions on its next
  iterations.
- `Subscribe` is not served, so the `health` collectors that stream NMX-C
  telemetry see an endpoint without telemetry.
- Partition health is always healthy, and `NMX_ST_RESOURCE_EXHAUSTED` is never
  reported, so NICo's multicast-limit retry is not exercised.
- Only Wiwynn GB200 and Lenovo GB300 trays report NVLink GPUs; other GB300
  platforms simulate no GPUs and form no domain.
- A rack is reconciled only once a switch of the rack reports
  `CONTROL_PLANE_STATE_CONFIGURED`, which the rack controller records from the
  RMS mock's scale-up fabric status. Until then the partition monitor reports
  the rack as `NoEndpoint`.
- The two standalone GB200 hosts of a simulation profile belong to no rack and
  are reached, if at all, through the `nvlink_nmxc_endpoints` chassis mapping
  (`nico-admin-cli nvlink-nmxc-endpoints`).
