# NICo DHCP Server Helm Chart

This chart deploys the NICo Dynamic Host Configuration Protocol (DHCP) server. It can also render the shared IPv4 service-VPC NetworkAttachmentDefinition (NAD) and start its node-local DHCP Container Network Interface (CNI) daemon for data processing unit (DPU) extension-service pods. A NAD is the Kubernetes object that Multus uses to select a CNI network. The Scalable Function (SF) is the BlueField interface allocated to the extension-service pod. The DOCA Platform Framework (DPF) installs this chart into DPU-cluster namespaces.

## IPv4 Service-VPC Network

The IPv4 service-VPC network is disabled by default. Enabling it renders the NAD and a DHCP CNI DaemonSet. A release with the default workload setting also retains the NICo DHCP server DaemonSet and Service. Core's stable platform release suppresses those server resources, so it owns the NAD and CNI daemon without deploying a second NICo DHCP server. The following example enables the network:

```yaml
serviceVpcNetworks:
  ipv4:
    enabled: true
```

When enabled, the chart creates `nico-service-vpc-ipv4` in the Helm release namespace with this CNI configuration:

```json
{
  "bridge": "br-sfc",
  "cniVersion": "0.4.0",
  "interface_type": "dpdk",
  "ipam": {
    "daemonSocketPath": "/run/vpc/cni/dhcp.sock",
    "type": "dhcp"
  },
  "mtu": 9000,
  "name": "nico-service-vpc-ipv4",
  "type": "ovs"
}
```

The NAD carries the `k8s.v1.cni.cncf.io/resourceName: nvidia.com/bf_sf` annotation so Multus allocates the service-side SF. It does not create another static DPF SF reservation.

The consuming DPUService supplies the persisted media access control (MAC) address for its interface in the Multus network-selection annotation. Multus passes that value to the Open vSwitch (OVS) CNI plugin as the `MAC` CNI argument, and OVS applies it before DHCP IPAM runs. The provider image does not need a DHCP client or a NICo-specific client identifier.

The service-VPC DaemonSet runs `/opt/cni/bin/dhcp daemon` from the DPU host. It mounts the host's `/opt/cni/bin` read-only, mounts `/var/run/netns` bidirectionally, and mounts host `/run/vpc/cni` at the daemon's standard `/run/cni` path. The daemon therefore creates `/run/vpc/cni/dhcp.sock` for host-executed CNI requests. DPF's CNI installer must install compatible OVS and DHCP binaries on every eligible DPU node before enablement. This chart starts the DHCP daemon but does not install or inspect either binary.

IPv6 is not rendered by this chart. A future IPv6 NAD must use a distinct name and configuration, preserve a maximum transmission unit (MTU) of 9,000 bytes, and join the same family-neutral platform release so both families share the daemon. It must not change the IPv4 name, CNI version, bridge, interface type, resource annotation, or DHCP socket contract documented here.

## Fixed Contract Values

The chart exposes the following service-VPC and workload switches:

| Value                             | Default                 | Contract                                                                         |
| --------------------------------- | ----------------------- | -------------------------------------------------------------------------------- |
| `serviceDaemonSet.enabled`        | `true`                  | `false` suppresses only the NICo DHCP server DaemonSet and Service.              |
| `serviceVpcNetworks.ipv4.enabled` | `false`                 | Omission or `false` leaves the NAD and CNI daemon absent. `true` renders both.   |
| `serviceVpcNetworks.ipv4.mtu`     | `9000`                  | When enabled, overrides are rejected to preserve the 9,000-byte path MTU.        |
| `serviceVpcNetworks.ipv4.name`    | `nico-service-vpc-ipv4` | When enabled, overrides are rejected because Core uses this shared name.         |

The NAD name is independent of the service universally unique identifier (UUID), slot, DPU deployment, and Helm release revision.

## Stable Platform Ownership

The platform-release behavior in this section applies only when `dpf.enabled=true`. When `dpf.enabled=false`, Core ignores the platform-management value after configuration parsing and performs no platform-release validation or stable DPUService query or write.

When the top-level IPv4 control is explicitly configured, NICo Core maintains one direct, detached DPUService named `nico-service-vpc` in the DPF operator namespace. Its Helm release has the same family-neutral name, and the DPUService carries `carbide.nvidia.com/platform-service: service-vpc`. Core disables the NICo DHCP server DaemonSet and Service on this release while allowing its privileged DHCP CNI daemon. The enabled release therefore owns both the shared NAD and its node-local daemon. Deployment-owned DHCP releases preserve an omitted IPv4 switch, whose chart default is `false`, and Core overwrites any explicitly supplied `serviceVpcNetworks.ipv4.enabled` value with `false`. This prevents BF3, its derived BF3 GB200 deployment, and other DPU deployments from becoming additional owners. A deployment-owned release’s effective IPv4 switch remains unchanged when omitted or already `false`; Core replaces explicit `true` with `false`. Future address-family NADs belong in this same release so the families share one daemon without a configurable runtime owner.

With an explicit `enabled` value, Core reads only that stable DPUService during startup reconciliation. It creates the object when absent, patches mutable chart fields in place when owned, and refuses to adopt or overwrite a same-name object without the expected label and immutable identity. Core does not query the live NAD, Argo Application, DPU nodes, daemon socket, CNI binaries, or extension-service pods as part of this reconciliation. Omitting the value performs no platform DPUService query or write.

Core synchronously validates the platform-management object structure, the Boolean `enabled` value, and the packaged chart version. Errors from those checks fail startup, as do DPUService ownership, identity, and Kubernetes API reconciliation errors. Helm validates other chart-native values, including unknown IPv4 fields and the fixed NAD name and MTU, during asynchronous DPF and Argo CD reconciliation. Schema or template failures do not fail Core startup. If later DPUService reconciliation fails, DPF initialization can already have written the shared BMC Secret and deployment resources, and Core does not roll those writes back. A successful DPUService create or patch returns before the Argo Applications, NAD, DHCP CNI DaemonSet, and daemon socket converge. Core readiness and Helm `--wait` can succeed before that convergence.

DPF and Argo CD do not provide a repository-enforced guarantee against an independently configured, differently named DPUService or Application rendering the same NAD or starting another daemon on the same host socket. Operators must ensure no external release renders `nico-service-vpc-ipv4` or owns `/run/vpc/cni/dhcp.sock`, and must verify every Argo Application generated for the stable release is Synced and Healthy after rollout. A disabled platform release intentionally renders no resources. Those Applications should remain Synced and Healthy while pruning a previously enabled NAD and daemon.

Changing the top-level switch from `true` to `false` updates the same stable release and lets Argo prune its NAD and DHCP CNI daemon. Do not disable it while any active or terminating DPUService pod still references the NAD. Removing the switch is not disablement: omission stops platform reconciliation and leaves any existing stable DPUService unchanged.

## Values Precedence

Chart defaults apply first. For deployment-owned DHCP releases, NICo merges the selected service's `extra_helm_values`. If those effective values explicitly contain `serviceVpcNetworks.ipv4.enabled`, Core replaces only that leaf with `false`; omission and sibling values remain unchanged. DPF deployment-specific DPUServiceConfiguration values apply afterward and do not enable this chart field. For the stable platform release, NICo uses the top-level `[dpf.services.dhcp_server]` repository URL, image settings, pull secret, and `extra_helm_values`, but pins the chart name and version to the `nico-dhcp-server` package built with the running Core release. Core then forces only `serviceDaemonSet.enabled: false`, preserving sibling DaemonSet settings for the DHCP CNI daemon.

A deployment-local mandatory `services` block replaces the top-level mandatory-service set for that deployment, but it does not change the stable platform release. Enable the IPv4 platform network only through the top-level DHCP service configuration:

```toml
[dpf.services.dhcp_server.extra_helm_values.serviceVpcNetworks.ipv4]
enabled = true
```

The Core control is tri-state. Omission leaves platform ownership unmanaged and performs no stable DPUService reconciliation. Explicit `true` creates or updates the stable release with the NAD and daemon enabled. Explicit `false` creates or updates the same release with both resources disabled, allowing Argo to prune them while retaining stable ownership.

## Rollout

These steps apply to initial enablement on a site with no existing service-VPC slots. Keep extension-service registration creates and updates quiesced through the slot and admission changes.

1. Configure a qualified positive `dpu_config.service_vpc_slot_count` and the required SF pool and endpoint capacity while leaving every `service_vpc_enabled_address_families` list empty.
1. Restart every Core replica with the same slot configuration, wait for the generated DPF deployment and service resources to converge, reprovision the affected non-Astra DPUs, and verify the fixed slot NADs and service-chain switches use MTU 9,000.
1. Ensure the top-level DHCP `helm_repo_url` publishes or mirrors the `nico-dhcp-server` version packaged with the running Core build. The stable release uses this version even when deployment-owned DHCP releases remain pinned to an older chart.
1. Verify DPF's CNI installer has installed compatible OVS and DHCP binaries in `/opt/cni/bin` on every eligible DPU node.
1. Configure the top-level DHCP service to enable the IPv4 platform network and restart every Core replica with the same configuration.
1. Verify `nico-service-vpc` has the fixed Helm release identity, `carbide.nvidia.com/platform-service: service-vpc`, and `spec.security.privileged: true`. Verify every Argo Application generated for it is Synced and Healthy.
1. Verify no competing DPUService, Application, or Helm release renders `nico-service-vpc-ipv4` or owns `/run/vpc/cni/dhcp.sock`.
1. Verify `nico-service-vpc-dhcp-cni` is ready on every eligible DPU node, carries `app.kubernetes.io/instance: nico-service-vpc`, and creates `/run/vpc/cni/dhcp.sock` on each node.
1. Verify the installed `nico-service-vpc-ipv4` NAD carries `app.kubernetes.io/instance: nico-service-vpc`, the documented CNI configuration and MTU, and the `nvidia.com/bf_sf` resource annotation.
1. Set `service_vpc_enabled_address_families = ["ipv4"]` only on qualified non-Astra deployments and restart every Core replica. Keep registration writes quiesced until every replica uses the same configuration and registration reconciliation converges. This admits creation of dormant registration parents; workload activation is not implemented.

The stable platform DPUService is not part of a DPUDeployment service revision. Its reconciliation does not release DPU maintenance holds, does not depend on `dpu_service_sync_enabled`, and does not wait for tenant maintenance. NodeEffect and automatic maintenance-release controls apply only to deployment-owned revisions whose `DPUServiceConfiguration` has `upgradePolicy.applyNodeEffect: true`. NICo sets that field to `false` for the DPU agent, DHCP server, FMDS, and OTel Collector; HBN, DTS, and extra services use the gated default. Keep registration writes quiesced while changing address-family admission or fixed slot inventory as described in the [Core configuration reference](../../../crates/api-core/src/cfg/README.md#dpfdeploymentconfig).

## Disablement and Downgrade

Prevent new service-VPC attachments, disable Core admission, and drain every active or terminating consumer. Delete every networked DPF Helm chart extension-service registration through Core and wait until each reaches `Deleted`. This lets reconciliation remove its owned DPUService, DPUServiceInterfaces, and DPUServiceChains instead of repairing them after family disablement. Only then set the platform-network control explicitly to `false` and wait for every stable Argo Application to remain Synced and Healthy with both the NAD and DHCP CNI DaemonSet absent before downgrading Core. Do not repoint the retained `nico-service-vpc` release to a chart that predates `serviceDaemonSet.enabled`; such a chart ignores workload suppression and renders a NICo DHCP server DaemonSet and Service. Do not remove or downgrade the OVS or DHCP CNI binaries while any NAD consumer remains. Runtime compatibility is part of the same drain boundary. After the NAD, daemon, and all consumers are gone, the stable DPUService can remain pinned to the suppression-capable chart without exposing connectivity to the tenant OS.
