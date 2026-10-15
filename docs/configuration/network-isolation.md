# Network Isolation

NICo enforces separation between tenants across all network planes. This isolation is established automatically during instance provisioning -- operators do not need to configure isolation manually.

## Ethernet (North-South)

BlueField DPUs running HBN (Host-Based Networking with Containerized Cumulus) enforce L3 VXLAN/EVPN boundaries. Each VPC gets its own VRF (Virtual Routing and Forwarding instance) on every DPU that hosts an instance in that VPC. Traffic between VPCs is isolated at the network layer without requiring any leaf switch configuration changes.
Floating blackhole routes for `site_fabric_null_routes` are installed, and authorized route imports, such as compatible VPC peer routes, take precedence only when they are at least as specific as the applicable blackhole.

Key properties:
- Per-VPC VRF with dedicated VNI (VXLAN Network Identifier) from the site's VNI pool
- Null-routes prevent unauthorized upstream-VRF hairpinning
- Route targets control which VRFs can exchange routes
- `deny_prefixes` ACLs block tenant traffic from reaching management networks
- Network Security Groups provide per-subnet firewall rules

For the full networking architecture, see [VPC Network Virtualization](../manuals/vpc/vpc_network_virtualization.md).

## InfiniBand (East-West)

UFM assigns P_Key partitions to each tenant's IB ports. Only hosts sharing a P_Key partition can communicate over InfiniBand, enforcing tenant isolation on the high-performance fabric.

View IB partition assignments:

```
nicocli tui
> infiniband-partition list
> infiniband-partition get
```

## NVLink

NMX-C APIs configure GPU membership in NVLink partitions. A physical NVLink domain can contain multiple isolated partitions belonging to different tenants; GPUs can communicate only with other GPUs in the same partition. For GB200 NVL72 systems, NICo gates instance allocation on NVLink cluster readiness -- if the fabric is not healthy, provisioning is blocked.

View NVLink partition state:

```
nicocli tui
> nvlink-logical-partition list
> nvlink-logical-partition get
```

## What a Tenant Can and Cannot Access

| Resource | Tenant Can Access | Tenant Cannot Access |
|----------|------------------|---------------------|
| Instances | Own instances in own VPCs | Other tenants' instances |
| Network | Traffic within own VPCs and subnets | Management networks, other tenants' VPCs (unless peered) |
| Storage | NVMe on assigned machines | Storage on unassigned machines |
| InfiniBand | P_Key partitions assigned to their instances | Other tenants' IB partitions |
| NVLink | GPUs in NVLink partitions assigned to their instances | GPUs in partitions not assigned to their instances |
| BMC/UEFI | No access (managed by NICo) | All BMC and UEFI interfaces |
