# nmxc-mock

A mock of the NMX-C (NVLink controller) gRPC API for simulated racks.
machine-a-tron mounts it on its bmc-mock listener, so the HTTPS endpoint that
serves the simulated BMCs also answers `NMX_Controller` calls. The mock keeps
no hardware inventory of its own: each simulated rack is one NVLink domain,
and the GPUs it reports are the ones the rack's machines report through
discovery, identified by the same fabric GUID, so NICo's partition monitor can
join the two. Partition state is kept per domain, in memory, for the life of
the process. RPCs outside its scope, including `Subscribe`, return
`UNIMPLEMENTED`.

## Which rack answers

A real deployment has one NMX-C per rack, reached at a switch's NVOS address
on port 9370. Here every such address is routed to one listener, so the mock
selects the domain from the request's authority (the HTTP/2 `:authority`, or
`Host`): an authority whose host is one of a rack's switch NVOS addresses
selects that rack. When only one rack is simulated it answers on any name, so
a port-forward to the listener works without a per-address route.

In Kubernetes, `mat-k8s-controller` creates a Service per simulated switch
that publishes the switch's leased NVOS address as `externalIPs` and forwards
port 9370 to the bmc-mock listener, exactly as it already does for BMC
addresses. The underlay (NVOS) DHCP segment must therefore stay outside the
cluster's ServiceCIDR and pod CIDR, as the BMC segment already must.

## Pointing NICo at the mock

NICo resolves a rack's NMX-C from the NVOS address of a switch in that rack,
so nothing names the mock directly. The mock is served with the bmc-mock
listener's own TLS material and does not require client authentication, so
`nico-api`'s `[nvlink_config]` needs only the CA and the name to verify:

```toml
[nvlink_config]
enabled = true
allow_insecure = false
nmx_c_tls_ca_cert_path = "/var/run/secrets/nico-roots/ca.crt"
nmx_c_tls_authority = "mat-mock.nvidia.com"
```

NICo dials the mock by IP address, so hostname verification relies on
`nmx_c_tls_authority`, which is one value for every rack. The
`nico-machine-a-tron` chart therefore adds `mat-mock.nvidia.com` to every pod's
certificate as an extra SAN (`certificate.extraDnsNames`); the CA is the one
that issues those certificates, which in the standard deployment is the
`nico-roots` CA nico-api already mounts. `dev/deployment/tilt/values.yaml`
carries this configuration for the local cluster.

Each new domain boots with a factory partition (id 32766, named `Default`)
holding all of its GPUs, as a real controller does; NICo deletes it before it
provisions partitions of its own. The optional `[nmxc_mock]` table in the
machine-a-tron configuration sets `version_string`, and
`boot_with_default_partition`, `default_partition_id` and
`default_partition_name` for that factory partition.
