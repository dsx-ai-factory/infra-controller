# Scale FAQ

Use these references when planning a large site and deciding where to place
the services used during installation, ingestion, and recovery.

## What Scale Has Been Tested?

The [Sizing and Scale Guidance](sizing-and-scale.md) distinguishes simulated
fleets from field measurements and lists the tested software and settings.
Use those boundaries when choosing controller resources and ingestion
concurrency. A simulated host count is not a measured tenant-workload,
firewall, registry, or boot-image throughput limit.

## How Do I Size Address Pools and Control Route Imports?

Allocate address pools, ASNs, VNIs, and route targets with the network team
using [Network Prerequisites](prerequisites/network.md) and the
[Control-Plane Network Requirements and Site Worksheet](https://github.com/dsx-ai-factory/infra-controller/blob/main/scripts/control-plane-dpu/control-plane-network.md).
For tenant VIPs, the [BGP Peering](../manuals/networking/bgp_peering.md)
manual describes route announcements. Plan external access through
[Tenant Connectivity and Shared Services](../manuals/vpc/tenant_connectivity.md).

## Where Should the Container Registry and OS Repositories Live?

Place the image registry on a network reachable from every Kubernetes node
that pulls controller, DPF, or DPU workload images. Verify DNS, TLS trust,
authentication, and outbound access separately for the controller cluster
and DPU cluster. A successful pull on a controller node does not establish
that a DPU node can pull the same image.

Keep the [Boot Image Hosting](../bootable_artifacts.md#boot-image-hosting)
path reachable from the provisioning networks. The reference PXE deployment
copies artifacts from container images and serves files to devices; those
are separate traffic paths with separate capacity requirements.

For tenant OS package repositories and container registries, use an endpoint
reachable from the tenant VPC through its configured routing. An address on
the controller's Kubernetes Service network is not automatically reachable
from a tenant. When a repository serves several VPCs, plan its placement
using [Tenant Connectivity and Shared Services](../manuals/vpc/tenant_connectivity.md).
Validate an authenticated fetch from the actual tenant path.

Use local mirrors where your deployment requires them, and plan their
storage, synchronization, and concurrent-download capacity. Include image
pulls after a node replacement and simultaneous OS updates or host recovery
in the load test. Size rack uplinks and VIP ingress for the combined traffic
that crosses them, rather than the average download rate of one host.

## How Should I Validate the Download Capacity?

Use the [Concurrent Download Model](../bootable_artifacts.md#plan-for-concurrent-downloads)
to estimate useful throughput, then measure a representative batch from
each consumer network. Check the registry, PXE server, package repository,
service VIP, and network links individually. Repeat with cold caches and
the failover path you intend to use. Keep these measurements separate from
the controller sizing figures.
