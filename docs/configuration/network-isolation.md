# Network Isolation

NICo reconciles tenant-isolation policy across the Ethernet, InfiniBand, and NVLink data planes after operators configure the required site policy and fabric prerequisites. Ethernet isolation is not guaranteed by a virtual routing and forwarding (VRF) instance alone: shared upstream routing can otherwise hairpin traffic between tenant VRFs.

## Ethernet (North-South)

BlueField DPUs running HBN (Host-Based Networking with Containerized Cumulus) reconcile the selected VPC virtualization mode. Full Next-generation Networking (FNN) creates a per-VPC VRF and, under `mutual_isolation`, installs floating blackhole routes for `site_fabric_null_routes`. Authorized route imports, such as compatible VPC peer routes, take precedence only when they are at least as specific as the applicable blackhole. EthernetVirtualizer (ETV) uses an L2 overlay in the default VRF. ETV applies its site-prefix isolation access control list (ACL) only when no Network Security Group replaces that policy. Flat VPC isolation is owned by the operator-managed network fabric. The `open` isolation mode does not install the FNN blackholes or ETV site-prefix isolation ACL.

Key properties:

- FNN per-VPC VRF with a dedicated VNI (VXLAN Network Identifier) from the site's VNI pool
- FNN site-fabric blackholes prevent unauthorized upstream-VRF hairpinning
- Route targets control which VRFs can exchange routes
- `deny_prefixes` ACLs block tenant traffic from reaching management networks
- Network Security Groups provide per-subnet firewall rules

For the full networking architecture, see [VPC Network Virtualization](../manuals/vpc/vpc_network_virtualization.md).

## InfiniBand (East-West)

UFM assigns P_Key partitions to each tenant's IB ports. Only hosts sharing a P_Key partition can communicate over InfiniBand, enforcing tenant isolation on the high-performance fabric.

View IB partition assignments:

```text
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
| Network | Traffic within own VPCs and subnets | Management networks and, when mutual isolation is configured, other VPCs unless authorized by peering or routing policy |
| Storage | NVMe on assigned machines | Storage on unassigned machines |
| InfiniBand | P_Key partitions assigned to their instances | Other tenants' IB partitions |
| NVLink | GPUs in NVLink partitions assigned to their instances | GPUs in partitions not assigned to their instances |
| BMC/UEFI | No access (managed by NICo) | All BMC and UEFI interfaces |
