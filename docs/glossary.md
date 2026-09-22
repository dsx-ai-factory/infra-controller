# Glossary

This glossary consolidates terminology from both halves of NVIDIA Infra Controller (NICo): the on-site Rust control plane, NICo Core, and the cloud-facing Go API layer, NICo REST. It is intended for documentation authors, operators, and anyone reading NICo-related content across the DSX documentation site.

Terms are grouped by domain rather than by repository. A reader looking up "Site Agent" should not need to know whether the term comes from the Core codebase, the REST codebase, or an integration between the two.

This glossary focuses on NICo-specific concepts: terms that only make sense in the context of the NICo platform. Where a term has a general industry definition but carries additional NICo-specific meaning, this glossary explains the NICo-specific part.

## Platform Architecture

### NICo

NVIDIA Infra Controller. NICo is the platform that provides site-local, zero-trust bare-metal lifecycle management with DPU-enforced isolation. It spans NICo Core, the on-site Rust control plane, and NICo REST, the cloud-facing Go API layer.

### NCP

NVIDIA Cloud Partner. In NICo docs, NCP usually refers to an infrastructure provider operating NICo-managed environments for tenant workloads.

### ManagedHost

The fundamental unit of infrastructure that NICo manages. A ManagedHost represents a single physical box in a datacenter and groups one Host Machine with the DPU Machines attached to that host. Most current deployments use one DPU per host, but the Core data model stores DPUs as a list, and supported/tested paths include hosts with multiple DPUs and configuration-gated zero-DPU hosts.

NICo manages the Host and attached DPUs end-to-end: the DPUs provide networking enforcement and management infrastructure, while the Host provides the compute resources that tenants consume.

### Machine

A generic term for either a DPU or a Host. The codebase and APIs use Machine when the distinction between the two does not matter, for example in health reporting, power management, and search queries.

### Host

The compute server as a customer thinks of it, typically an x86-based machine. It is the bare metal that NICo manages. The Host runs whatever operating system the customer provisions onto it. Each Host has its own BMC for out-of-band management.

### Instance

A Host that is currently allocated to and being used by a tenant. Instances are the output of the NICo provisioning pipeline: a ready-to-use bare-metal server with validated hardware, tenant-isolated networking, and DHCP and DNS services available.

Instance creation can be done through the gRPC API, where the caller explicitly selects the machine, or through the REST API, which supports resource allocation pools and random selection.

### Leaf

In NICo architecture, the device that a Host connects to for network access. Currently this is a DPU that makes the overlay network available to the tenant. In future iterations, the Leaf might be a specialized switch instead of a DPU.

### DPU Role in NICo

The DPU is the central enforcement point in NICo architecture. It serves as the VTEP for overlay networking, runs HBN for software-defined networking, and enforces Ethernet tenant isolation in hardware. NICo is responsible for installing the DPU OS and all DPU firmware, including BMC, NIC, and UEFI firmware.

In current deployments, the DPU is a [NVIDIA BlueField-2 or BlueField-3](https://www.nvidia.com/en-us/networking/products/data-processing-unit/) network interface card. It has its own ARM processor, operating system, and BMC. From NICo's point of view, it can act as a network card, a disk controller, and an on-host enforcement point.

### BlueField

The NVIDIA accelerator and SuperNIC product family used by NICo for networking, tenant isolation, and site management. DPU-capable BlueField products have an Arm complex, BMC, NIC firmware, and OS image that NICo can provision and manage. NIC-only SuperNIC products do not expose that switchable DPU execution environment to NICo.

### Host DPU Policy

The operator's desired treatment of DPU hardware on a host. `HostDpuPolicy` has three values: `Manage`, which at the site or resolved level lets NICo provision and attach the DPUs; `Nic`, which asks NICo to run physically present DPU hardware as plain NICs; and `Ignore`, which tells NICo not to configure or attach DPU hardware. For backward compatibility, a per-host `Manage` declaration inherits the site policy rather than overriding it. This policy is intent, not a hardware observation.

### BlueField Operating Mode

The optional operating mode reported by a DPU-capable BlueField product's own BMC. When available, `BlueFieldOperatingMode` is either `Dpu` or `Nic`. NICo compares this observed state with the host DPU policy when deciding whether a DPU-capable card needs reconfiguration. NIC-only SuperNIC products do not need to report a switchable DPU mode. Existing protobuf and Redfish boundaries retain the legacy `NicMode` name for compatibility; model code uses `BlueFieldOperatingMode` so observed state is not confused with desired policy.

### Mellanox/BlueField Device Kind

The factory product classification NICo derives from a card's part number, independently of its current operating mode. Model code calls this `MlxDeviceKind`; some historical variant and protobuf names contain `Mode`, but those names are retained only for compatibility and identify a SKU rather than observed state. A DPU-capable product can therefore remain the same device kind when reconfigured from DPU mode to NIC mode. NIC-only SuperNIC products are classified by product without implying that they can enter DPU mode; future CX8 and CX9 SuperNIC support follows this model.

## REST API Services and Binaries

### API Server

The main NICo REST API server. It handles external HTTP requests, authenticates callers through JWTs, and routes requests to resource handlers. It is the primary entry point for tenant, site, machine, and networking operations exposed through the REST API. Current deployment artifacts in this repo still refer to this component as `nico-rest-api`.

The NICo REST repository's Helm charts and generated SDK now use `nico-rest-api`.

### Workflow Worker

The Temporal workflow worker service for NICo REST. It executes workflow logic for long-running operations such as site setup and hardware lifecycle management. Current deployment artifacts in this repo still refer to this component as `nico-rest-workflow`.

The NICo REST repository's Helm charts now use `nico-rest-workflow`.

### Site Agent

The on-site datacenter agent that bridges Temporal workflows to NICo Core. It polls a site-specific Temporal namespace for workflow tasks, translates them into gRPC calls against the local Core instance, and publishes inventory data back through Temporal.

REST-dispatched operations that need site-local hardware access flow through a Site Agent before reaching Core and the managed hardware. Current deployment artifacts in this repo still refer to this component as `nico-rest-site-agent`; older metrics and code may also use the internal codename Elektra.

The NICo REST repository's Helm charts now use `nico-rest-site-agent`.

### Site Manager

A NICo REST service used for site-level management and Site Agent bootstrap flows. Current deployment artifacts in this repo still refer to this component as `nico-rest-site-manager`.

### Certificate Manager

A NICo REST certificate-management component used by the REST deployment. Current deployment artifacts in this repo still refer to the component and issuer with names such as `nico-rest-cert-manager` and `nico-rest-ca-issuer`. The REST repository also contains a native certificate manager that issues certificates using Go crypto and integrates with cert-manager.io.

### Database Migrations

The NICo REST deployment component that manages PostgreSQL database schema evolution. Current deployment artifacts in this repo still describe the REST stack with `nico-rest-*` names.

### CLI Client (`nicocli`)

The command-line tool for interacting with the REST API. It supports scripted usage and interactive session management for environment switching and resource commands. It was previously named `nicocli`.

## Authentication and Authorization

### Authorization Roles

NICo REST authorizes callers with provider and tenant role families. The REST SDK and current REST OpenAPI text describe authorization in terms of role suffixes such as `PROVIDER_ADMIN` and `TENANT_ADMIN`. Some generated tests, older OpenAPI snapshots, and bundled development Keycloak examples still use prefixed role strings such as `NICO_PROVIDER_ADMIN` and `NICO_TENANT_ADMIN`, so use the role format required by the target issuer and API version.

| Role family | Scope | Capabilities |
| --- | --- | --- |
| Provider admin | Organization, infrastructure provider | Administrative access to manage sites, hardware, tenants, expected machines, racks, and infrastructure operations |
| Provider viewer | Organization, infrastructure provider | Read-only access to infrastructure provider resources such as sites, expected racks, and machines |
| Tenant admin | Organization, tenant | Tenant-scoped administrative access to manage instances, SSH keys, VPC peering, and resources within the assigned tenant organization |
| Tenant viewer | Organization, tenant | Read-only access to tenant-scoped resources |

### JWT Claims Processor Pipeline

The chain of processors that extract authorization context from JWT tokens in the REST API. Processor types include Custom, KAS, Keycloak, and SSA. Each processor handles a different token origin and maps claims to internal authorization context.

### Service Account Authentication (SSA)

Machine-to-machine authentication using service account tokens. In the bundled development Keycloak setup, a service account can obtain a JWT through the client credentials flow and use that token against the REST API.

### NGC KAS

NVIDIA GPU Cloud Key Authentication Service. NICo REST can be configured to accept JWTs issued by NGC KAS and map NGC organization identity into NICo authorization context.

### SPIFFE Identity in NICo

NICo uses SPIFFE-based identities for service-to-service authentication within its microservice architecture. The DPU instance metadata service can issue SPIFFE JWT-SVIDs to tenant processes, providing machine identity signed with per-tenant keys.

## Networking

### BGP

[Border Gateway Protocol](https://en.wikipedia.org/wiki/Border_Gateway_Protocol) is the standardized routing protocol used to exchange routing and reachability information between autonomous systems. In NICo, BGP is used in the Ethernet overlay design so DPUs and top-of-rack or route-server devices can exchange EVPN reachability.

### EVPN

Ethernet VPN. In NICo, EVPN is the control-plane technology used with VXLAN overlays so DPUs and network devices can exchange tenant network reachability information.

### VXLAN Overlay Architecture

[Virtual Extensible LAN](https://en.wikipedia.org/wiki/Virtual_Extensible_LAN) is the primary overlay networking technology NICo uses for Ethernet tenant isolation. In a datacenter, operators often need multiple virtual networks to share one physical cable plant. A tenant expects its machines to be on a private Ethernet network, but operators should not have to re-cable hosts every time tenant assignment changes.

VXLAN solves this by wrapping an Ethernet frame in a VXLAN packet identified by a VNI. NICo uses the DPU as the VTEP, so the DPU wraps tenant Ethernet frames in VXLAN headers before sending them across the IP-routed datacenter network. The receiving VTEP unwraps the packet and delivers the original Ethernet frame. This lets the underlay route ordinary IP packets while the x86 Host behaves as if it received an Ethernet frame from a peer on the same local network.

### Network Segments

A NICo concept for defining IP address pools. Underlay segments are normally
used for management traffic on the underlying physical network, such as DPU OOB
and BMC addresses. A supported zero-DPU exception places the host BMC on
HostInband instead. Overlay segments are used for tenant-facing networks built
on top of VXLAN. NICo assigns IPs from overlay segments to Hosts when creating
instances.

### HostInband Network Segment

A network segment type for the underlay network that a zero-DPU host's NIC
attaches to directly. Unlike overlay (tenant) segments, a HostInband segment
describes a real underlay subnet, exists before any VPC, and may remain
unassociated with a VPC. It is the only segment type a Flat VPC accepts. A
zero-DPU host BMC may share the same physical subnet/VLAN and segment with the
host OS, using a distinct address; this does not extend to DPU BMC or DPU OOB
interfaces. Operators declare HostInband segments in the API server
configuration (or create them through the segment API); tenant instances on
zero-DPU hosts attach to them automatically rather than drawing a link-net from
a VpcPrefix. See
[Shared HostInband for a Host BMC and Host OS](provisioning/ip-and-network-configuration.md#15-shared-hostinband-for-a-host-bmc-and-host-os).

### Flat VPC

A VPC virtualization type for tenant instances on zero-DPU hosts. A Flat VPC is a real tenant VPC — it has an owner, a VNI, and can carry a Network Security Group — but its data plane is operator-managed: NICo allocates the underlay addresses and records the VPC's bookkeeping, while routing and L3/L4 enforcement live on the operator's physical or SDN fabric rather than on a NICo-managed DPU overlay. Tenant instances attach directly to HostInband segments. Contrast with the EthernetVirtualizer (ETV) virtualization type, where NICo programs a per-VPC VRF on each DPU.

### Zero-DPU Host

A managed host that NICo operates without a NICo-managed DPU — either a host whose DPU policy is `ignore`, or a host whose BlueField DPU is run as a plain NIC through `nic`. NICo does not build an overlay for a zero-DPU host; its tenant instances attach directly to HostInband underlay segments and belong to Flat VPCs. DPU policy is set site-wide in the API server configuration or per host on its Expected Machine. Rack-manager deployments must resolve the applicable DPU policy to `nic` to run DPUs as plain NICs.

### HBN in NICo

HBN runs as a container on the DPU and manages network routing using [Cumulus Linux](https://www.nvidia.com/en-us/networking/ethernet-switching/cumulus-linux/) components such as FRR and NVUE. NICo installs and manages HBN as part of DPU provisioning. Ethernet tenancy enforcement is performed within HBN on the DPU; NICo does not need to change Spectrum switches running Cumulus Linux.

DPU health reporting includes HBN status such as whether the container is running, BGP peering state, and configuration version.

General reference: [DOCA HBN Service](https://docs.nvidia.com/doca/sdk/pdf/doca-hbn-service.pdf)

### FNN

The networking subsystem within NICo Core that manages VPC creation, subnet allocation, and VXLAN overlay configuration.

FNN coordinates DPU-side HBN configuration with the NICo data model to deliver tenant-isolated L2 and L3 networks. FNN is itself one of NICo's VPC virtualization types — the L3 DPU-overlay type, alongside `EthernetVirtualizer` (ETV) and `Flat` — and is the only one that uses per-VPC routing profiles, which control route-target import and export policies, access tiers, and underlay leak acceptance.

### DHCP in NICo

[Dynamic Host Configuration Protocol](https://en.wikipedia.org/wiki/Dynamic_Host_Configuration_Protocol) is the network protocol used to automatically assign IP addresses and other communication parameters to devices. NICo runs its own DHCP service. DPUs and Hosts use DHCP to resolve their IP addresses, and NICo responds based on known information about each Machine.

DHCP relay must be configured on switches connected to DPU OOB interfaces, Host BMCs, and DPU BMCs so requests reach the NICo DHCP service.

NICo issues two IP addresses to the DPU RJ45 port: the DPU OOB address, used for SSH access to the ARM OS and NICo management traffic, and the DPU BMC address, used for Redfish and DPU configuration.

### DNS in NICo

[Domain Name System](https://en.wikipedia.org/wiki/Domain_Name_System) resolves domain names to IP addresses. NICo runs DNS services for managed machines and delegated site-controller zones so hosts and control-plane services can resolve NICo-managed names.

### Multi-Tenancy and Isolation

NICo coordinates tenant isolation across four network fabrics, each with its own isolation mechanism.

| Network type | Isolation mechanism | Managed by |
| --- | --- | --- |
| Ethernet north-south | VXLAN with EVPN for VPC creation | DPU through HBN |
| East-west Ethernet | ConnectX-based firmware paths where configured | Outside the current DPU HBN path |
| InfiniBand | Partition key assignment | UFM |
| NVLink | Partition management | NMX-C |

DPUs enforce Ethernet isolation in hardware, UFM enforces InfiniBand isolation, and NMX-C enforces NVLink isolation, all coordinated by NICo.

### VRF

Virtual Routing and Forwarding. In NICo networking, VRFs provide routing-table isolation for virtual networks so tenant or service routes can be kept separate even when they share physical infrastructure.

### VLAN

A VLAN adds a 12-bit identifier to an Ethernet frame to mark which virtual network it belongs to. Switches and routers can use VLAN IDs for isolation, but the 4096-ID limit makes VLANs too small for large multi-tenant environments.

In NICo, VLAN IDs can still appear on the DPU-to-Host link, especially when a Host is running a hypervisor and the VLAN ID identifies which virtual machine should receive the Ethernet frame. VXLAN is used for the larger datacenter overlay.

### VNI

VXLAN Network Identifier, also called a VXLAN ID. It is the 24-bit identifier in the VXLAN header that marks which virtual network an encapsulated Ethernet frame belongs to.

### VTEP

VXLAN Tunnel Endpoint. A VTEP wraps Ethernet frames into VXLAN packets and unwraps VXLAN packets back into Ethernet frames. In NICo, the DPU acts as the VTEP for tenant overlay networking.

### P_Key

InfiniBand partition key. P_Keys are the isolation mechanism used by UFM for InfiniBand tenant separation, analogous to how VXLAN identifies isolated Ethernet overlays.

### NVLink

The high-speed GPU-to-GPU fabric managed outside NICo Core by NMX-C. NICo coordinates with NMX-C so GPU partitioning aligns with tenant isolation.

### FMDS

NICo Metadata Service. FMDS runs on or alongside the DPU path and provides tenant workloads with instance metadata such as machine identity, boot information, and applied instance configuration. Some implementation artifacts still expand the legacy name as NICo Metadata Service.

### LLDP

Link Layer Discovery Protocol. NICo uses LLDP-derived network adjacency information to understand how hosts, DPUs, and switches are connected in the site fabric.

### Allocation and Constraint

REST API concepts for managing resource assignment to tenants. Allocations bind specific machines or capacity to a tenant. Constraints define rules about what resources a tenant can request, such as specific SKUs or rack locations. Together they control which hardware a tenant can see and consume.

## Boot and Provisioning

### BFB

BlueField bootstream. A BFB is an image format used to install or update the operating system and firmware bundle on a BlueField DPU.

### PXE and iPXE Boot

The Preboot Execution Environment, or PXE, is a client-server environment for booting software retrieved from the network. [iPXE](https://en.wikipedia.org/wiki/IPXE) is an open source PXE client and bootloader that can enable network boot on systems without built-in PXE support, or provide additional features beyond built-in PXE.

NICo uses PXE and iPXE for network booting. DPUs and Hosts use PXE after startup to install NICo-specific software images as well as tenant-requested images. NICo runs its own PXE server to serve images shipped as part of the software, such as DPU software and iPXE. This PXE server can coexist with other site PXE servers as long as DHCP is configured correctly and the Host can reach the NICo PXE service.

### Cloud-Init in NICo

[Cloud-init](https://cloudinit.readthedocs.io/en/latest/) is the industry-standard multi-distribution method for initializing cloud instances. During boot, cloud-init identifies the environment it is running in and configures networking, storage, SSH keys, packages, and other operating system settings.

Cloud-init is used in two ways within NICo. DPUs use a NICo-provided cloud-init file to install NICo-related components on top of the base DPU image provided by the NVIDIA networking group. Tenants can provide custom cloud-init configuration to automate installation and configuration of their chosen operating system on the Host.

### BMC

A Baseboard Management Controller manages low-level hardware functions such as BIOS settings and power state. The Host has a BMC, and the DPU has a separate BMC. A Host BMC commonly exposes both a web interface for BIOS and hardware settings and a Redfish API for programmatic management. NICo uses BMC access to discover, power-cycle, and repair machines without relying on the Host operating system.

### BMC Discovery

NICo discovers BMCs through DHCP. When provisioning a NICo site, operators specify which BMC subnets are on the network fabric. Those subnets must have DHCP relay configured to point to the NICo DHCP service. When a BMC requests an IP address, NICo allocates one and cross-references the MAC address against an expected machine table to look up initial credentials.

The Host has its own BMC, and attached DPUs can have their own BMCs. For the common one-host, one-DPU case, that means two BMCs are involved in a ManagedHost.

### Redfish

The HTTP API exposed by BMCs for out-of-band hardware management. NICo uses Redfish to manage power state, credentials, and other BMC-backed operations without relying on the Host operating system.

General reference: [Redfish](https://en.wikipedia.org/wiki/Redfish_(specification))

### OOB

Out of band. OOB management uses an endpoint independent from the Host operating
system, usually through dedicated BMC and DPU management networks, so NICo can
discover, power-cycle, and repair machines even when the tenant OS is
unavailable. A host BMC can remain operationally out of band while sharing a
HostInband L2 network with a zero-DPU host OS, but that topology lacks dedicated
network isolation.

### IPMI

[Intelligent Platform Management Interface](https://en.wikipedia.org/wiki/Intelligent_Platform_Management_Interface) is an older interface for out-of-band computer management and monitoring. Like Redfish, it lets administrators manage a machine over the network even when the Host OS is unavailable. NICo docs may mention IPMI when discussing BMC networks, serial console access, or legacy out-of-band workflows.

### scout

The discovery service that reports newly discovered DPUs to NICo Core during initial site bring-up. After discovery and provisioning, the DPU-side agent takes over ongoing communication with Core.

### dpu-agent

The daemon that runs on each DPU after provisioning. It periodically connects to the NICo Core gRPC API to retrieve configuration instructions and report state.

### Managed Host State Machine

The finite state machine that governs the lifecycle of Hosts managed by NICo. A Host progresses through discovery, DPU initialization, host initialization, BOM validation, machine validation, TPM-based attestation measurement, and finally reaches Ready, at which point it can be assigned to an Instance.

The full set of states defined in the `ManagedHostState` enum includes `DpuDiscoveringState`, `DPUInit`, `HostInit`, `BomValidating`, `Validation`, `Measuring`, `PreAssignedMeasuring`, `StartAssignmentCycle`, `Ready`, `Assigned`, `PostAssignedMeasuring`, `WaitingForCleanup`, `HostReprovision`, `DPUReprovision`, `Failed`, `ForceDeletion`, and `Created`. The typical happy-path progression is `DpuDiscoveringState` to `DPUInit` to `HostInit` to `BomValidating` to `Validation` to `Measuring` to `Ready` to `Assigned`.

### SKU Validation

The process of verifying that a Machine's actual hardware, or Bill of Materials, matches the expected SKU definition. NICo performs BOM validation during the provisioning pipeline to catch hardware mismatches before a Host reaches Ready.

## Workflow and Orchestration

### Temporal in NICo

NICo REST uses Temporal as the workflow orchestration engine for long-running operations. Each NICo site gets a dedicated Temporal namespace, providing workflow isolation between sites. Workflows carry authenticated context and use protobuf-encoded payloads.

### Cloud Workflow and Site Workflow

Two distinct workflow scopes exist. Cloud workflows run in the central management plane and orchestrate cross-site operations. Site workflows run locally at each datacenter and handle site-specific hardware operations. The Site Agent picks up site workflows from its Temporal namespace and translates them into gRPC calls against the local Core instance.

### Core Proto Synchronization

The protobuf interface shared between NICo Core and NICo REST. Proto definitions originate in Core and are synchronized to REST through a snapshot process. This shared contract defines the gRPC API that the Site Agent uses to communicate with Core.

## API Patterns

### OpenAPI Specification

The canonical REST API contract. Endpoint additions or modifications require updating the OpenAPI specification. It is validated in CI and used to generate the Go SDK client and rendered API documentation.

## Technology Stack

### DOCA

NVIDIA Data Center-on-a-Chip Architecture. In NICo, DOCA is the software framework and release train associated with BlueField DPU functionality that NICo installs and validates.

### UFM

Unified Fabric Manager. NICo relies on UFM for InfiniBand partition management, including assigning P_Keys for tenant isolation on the IB fabric.

### IMDS

Instance Metadata Service. In NICo, IMDS can provide tenant workloads with metadata and identity material, including SPIFFE JWT-SVIDs signed with per-tenant keys.

### Connect-RPC

The HTTP-based RPC framework used for the IPAM service's internal communication. Connect-RPC provides protobuf compatibility with HTTP/1.1 and HTTP/2 transports, gRPC health checking, and reflection. The Site Agent communicates with NICo Core over standard gRPC rather than Connect-RPC.

## Health Monitoring

### Health Monitoring

NICo provides hardware health monitoring across both layers. DPUs report health status including HBN configuration correctness and container status, BGP peering state, heartbeat information, configuration version applied, and BMC-side health such as thermal status.

Health information is stored as health report overrides on machine records. The system supports searching for Machines by health alert probe IDs and health alert classifications, allowing API clients to search for health conditions without requiring new API endpoints for each alert category.

## Deployment

### Core Deployment

NICo Core commonly runs on a Kubernetes cluster, with three or five control plane nodes recommended. It runs as a set of microservices including API, DNS, DHCP, hardware monitoring, BMC console, and rack management services. Deployment is done through Kubernetes Kustomize manifests.

### REST Deployment

NICo REST is deployed through Helm charts into a Kubernetes cluster. The deployment includes the API server, workflow worker, site manager, database migration job, and Keycloak integration.

### Disconnected Mode

NICo Core is site-local and continues to manage hardware independently of the REST API process. REST-dispatched workflows depend on the Site Agent and Temporal connectivity configured for that deployment, so behavior during upstream connectivity loss depends on how REST, Temporal, and the Site Agent are deployed.

## Quick Reference: Acronyms

| Acronym | Full name | NICo context |
| --- | --- | --- |
| BGP | Border Gateway Protocol | EVPN route exchange between DPUs and top-of-rack switches |
| BMC | Baseboard Management Controller | Two per ManagedHost, one for the Host and one for the DPU, discovered through DHCP |
| BMM | Bare Metal Manager | Legacy internal name that appears in older source, commands, and docs |
| BOM | Bill of Materials | Hardware inventory validated against expected SKU |
| DHCP | Dynamic Host Configuration Protocol | NICo runs its own DHCP service for BMC and DPU discovery |
| DNS | Domain Name System | NICo runs its own DNS microservice |
| DOCA | Data Center-on-a-Chip Architecture | NVIDIA software framework used by BlueField DPUs |
| DPU | Data Processing Unit | Central enforcement point for tenant isolation |
| EVPN | Ethernet VPN | Control-plane technology used with VXLAN overlays |
| FMDS | NICo Metadata Service | Metadata service for tenant workloads; legacy artifacts may expand it as NICo Metadata Service |
| FNN | Full Next-generation Networking | VPC, subnet, and VXLAN management subsystem in Core |
| gRPC | Google Remote Procedure Call | RPC framework used between the Site Agent and NICo Core |
| HBN | Host Based Networking | Software networking stack on the DPU for VXLAN and EVPN |
| IMDS | Instance Metadata Service | Service that can issue identity and metadata to tenant processes |
| iPXE | iPXE bootloader | Network bootloader used with PXE workflows |
| KAS | Key Authentication Service | NGC token authentication accepted by REST API |
| LLDP | Link Layer Discovery Protocol | Neighbor discovery protocol used for network topology visibility |
| NCP | NVIDIA Cloud Partner | Infrastructure partner operating NICo-managed environments |
| NICo | NVIDIA Infra Controller | This platform, also known historically as NICo, NICo, and BMM |
| NMX-C | NMX Controller | NVLink partition management for GPU-to-GPU isolation |
| OOB | Out of Band | Management network path independent from the Host OS |
| P_Key | Partition Key | InfiniBand isolation identifier assigned by UFM |
| protobuf | Protocol Buffers | Interface definition and serialization format for Core APIs |
| PXE | Preboot Execution Environment | Network boot mechanism used by DPUs and Hosts |
| Redfish | Redfish API | BMC API used for out-of-band hardware management |
| RLA | Rack Level Agent | On-site agent for rack switch management |
| SPIFFE | Secure Production Identity Framework for Everyone | Machine identity for service-to-service authentication |
| SSA | Service Account Authentication | Machine-to-machine authentication through per-tenant tokens |
| TPM | Trusted Platform Module | Hardware attestation during Host provisioning |
| UFM | Unified Fabric Manager | InfiniBand partition management for IB isolation |
| VNI | VXLAN Network Identifier | Numeric identifier for a VXLAN overlay network |
| VPC | Virtual Private Cloud | Tenant network boundary, mapped to overlay networking in FNN |
| VRF | Virtual Routing and Forwarding | Routing-table isolation mechanism used in network virtualization |
| VTEP | VXLAN Tunnel Endpoint | DPU role in overlay networking |
| VXLAN | Virtual Extensible LAN | Primary overlay technology for Ethernet isolation |
