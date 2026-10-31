# Getting Started

This section provides a quick overview of the API and how to get started.

## Authentication and Authorization

The first step is to authenticate using a JWT bearer token. Organization structures and roles depend on the authentication and authorization configuration used. For details on authentication and authorization, check the [Authentication and Authorization](/infra-controller/rest-api-reference/authentication-and-authorization) section.

## API Version

The next step is to be aware of the API version being used. The API version can be retrieved by calling the [Retrieve Metadata endpoint](/infra-controller/rest-api-reference/api-reference/metadata/get-metadata). In general, the API maintains backward
compatibility with the previous versions. Any breaking changes are announced using a deprecation notice. Click on each API resource to see the deprecation notices.

## Service Account Mode

Depending on the auth configuration used, the NICo REST API may be configured in Service Account mode. In this mode, API users can act as both Provider and Tenant as part of the same organization.
If this is the case, the user must first retrieve the Service Account by making a call to the [Retrieve Service Account endpoint](/infra-controller/rest-api-reference/api-reference/service-account/get-current-service-account).
For service accounts, the Tenant entity is initialized as a privileged Tenant when a Ready Tenant Account has `siteCapabilities` with `targetedInstanceCreation` enabled.

## Provider or Tenant Mode

If NICo REST API is not configured in Service Account mode, the user should retrieve the Infrastructure Provider by making a call to the [Retrieve Infrastructure Provider endpoint](/infra-controller/rest-api-reference/api-reference/infrastructure-provider/get-current-infrastructure-provider)
or the Tenant by making a call to the [Retrieve Tenant endpoint](/infra-controller/rest-api-reference/api-reference/tenant/get-current-tenant).

In both cases, these calls initialize Provider and Tenant entities for the organization. All resources created are anchored to either the Provider or Tenant entity.

Once the Provider and the Tenant are initialized, the user can create resources by making calls to the appropriate endpoints.

## Creating Site IP Blocks

NICo publishes Core SitePrefix inventory on the Site Agent's inventory schedule. Each operator-managed prefix creates a managed Site IP Block, or adopts an existing compatible, unlinked block with the same CIDR. A prefix wholly inside an RFC 1918 or RFC 4193 private range gets a `DatacenterOnly` IP Block; other prefixes get `Public`. REST owns the block's name and description after creation or adoption. When no REST IP Block, including a retired one, already carries that Core ID, a `Deleting` report can adopt and retire a compatible unlinked root, even if it was renamed or created manually. A REST name is not an exemption from Core-managed lifecycle.

When Core reports an operator prefix as `Deleting`, its Site IP Block becomes `Deleting`, rejects new Allocations, and is removed together with its IPAM root once existing Allocations drain. Repeated `Deleting` reports do not recreate it. A later `Ready` report for the same Core ID restores the original REST identity and IPAM root. An overlapping replacement prefix, such as a `/23` replacing a `/24`, is created after the old root is removed. If the replacement is processed first, it is retried on a later inventory.

The final successful inventory page carries the complete set of Core prefix IDs. Missing linked IP Blocks are reconciled from that list, including an empty inventory; blocks created within the Site's inventory interval are protected. A missing operator root is removed when free. If Allocations remain, it becomes `Deleting`, continues to reject new Allocations and resizing, and its Core link is cleared so a new Core ID for the same CIDR can adopt it. Once unlinked, it is not swept again; after its Allocations drain, a Provider can delete it manually, or a new Core ID can adopt it. Missing tenant-created unmanaged blocks are removed if already `Deleting`; otherwise they become `Error`. Allocated tenant blocks and unlinked roots are not swept. Failed inventory messages do not change IP Blocks.

Site configuration inventory no longer creates or removes IP Blocks from `site_fabric_prefixes`. Upgrade the Site Agent as part of the rollout: until it runs the SitePrefix publisher, new Cloud workflows do not reconcile fabric-prefix changes for that Site. Existing Temporal histories retain the legacy importer for replay. The SitePrefix receiver and publisher replace that importer as described in [receiver issue #5762](https://github.com/dsx-ai-factory/infra-controller/issues/5762), [receiver implementation #7008](https://github.com/dsx-ai-factory/infra-controller/pull/7008), and [publication and retirement issue #5763](https://github.com/dsx-ai-factory/infra-controller/issues/5763).

To create any other IP Block, the user must make a call to the [Create IP Block endpoint](/infra-controller/rest-api-reference/api-reference/ip-block/create-ipblock). It rejects a range that overlaps an existing Site IP Block.

> **Note:** From this point onwards, a brief outline is provided for the typical API call flows for various use cases.

## Typical API Call Flow for Service Account

- Retrieve available Sites using the [Retrieve All Sites endpoint](/infra-controller/rest-api-reference/api-reference/site/get-all-site) and choose a Site to create resources in. For _Disconnected_ NICo installations where NICo
REST is deployed alongside NICo Core, typically there will be a single Site available.
- Retrieve the Site IP Blocks using the [Retrieve All IP Blocks endpoint](/infra-controller/rest-api-reference/api-reference/ip-block/get-all-ipblock).
- For each Site IP Block, create a Network Allocation for the Tenant entity using the [Create Allocation endpoint](/infra-controller/rest-api-reference/api-reference/allocation/create-allocation) using the full prefix length.
This will create a Tenant IP Block for each Site IP Block.
- Creating an Allocation will create the Tenant in NICo Core.
- Create a VPC using the [Create VPC endpoint](/infra-controller/rest-api-reference/api-reference/vpc/create-vpc).
- After the VPC reaches `Ready`, create the network resource that matches its
  `networkVirtualizationType`, referencing the VPC and a Tenant IP Block.
  - For an `FNN` VPC, create a VPC Prefix using the
    [Create VPC Prefix endpoint](/infra-controller/rest-api-reference/api-reference/vpc-prefix/create-vpc-prefix).
  - For a VPC whose `networkVirtualizationType` is
    `ETHERNET_VIRTUALIZER`, create an IPv4 Subnet from a `Ready`,
    tenant-allocated IPv4 IP Block at the VPC's Site using the
    [Create Subnet endpoint](/infra-controller/rest-api-reference/api-reference/subnet/create-subnet).
  - For a `FLAT` VPC, do not create a VPC Prefix or Subnet. When creating
    the Instance, set `autoNetwork` to `true` and omit `interfaces`.
- Create an Operating System using the [Create Operating System endpoint](/infra-controller/rest-api-reference/api-reference/operating-system/create-operating-system) specifying iPXE script and user data.
- Retrieve available Machines on Site using the [Retrieve All Machines endpoint](/infra-controller/rest-api-reference/api-reference/machine/get-all-machine)
- Create an Instance specifying the VPC, Operating System, and Machine. For an
  `FNN` or `ETHERNET_VIRTUALIZER` VPC, include the matching VPC Prefix or
  Subnet in `interfaces`.

## Typical API Call Flow for Provider

- Create a Tenant Account using the [Create Tenant Account endpoint](/infra-controller/rest-api-reference/api-reference/tenant-account/create-tenant-account). Provider must know the Tenant org name.
- Once the Tenant has accepted the Tenant Account, the Provider can allocate resources to the Tenant.
- Retrieve available Sites using the [Retrieve All Sites endpoint](/infra-controller/rest-api-reference/api-reference/site/get-all-site) and choose a Site to create resources in.
- Create an Instance Type using the [Create Instance Type endpoint](/infra-controller/rest-api-reference/api-reference/instance-type/create-instance-type)
- Retrieve available Machines on Site using the [Retrieve All Machines endpoint](/infra-controller/rest-api-reference/api-reference/machine/get-all-machine)
- Update a Machine's Instance Type using the [Update Machine endpoint](/infra-controller/rest-api-reference/api-reference/machine/update-machine) or assign multiple Machines to an Instance Type using the
[Create Instance Type/Machine Association endpoint](/infra-controller/rest-api-reference/api-reference/instance-type/create-instance-type-machine-association)
- Create a Compute Allocation for Tenant using the [Create Compute Allocation endpoint](/infra-controller/rest-api-reference/api-reference/allocation/create-allocation) referencing the Instance Type
- Creating any type of Allocation for a Tenant will create the Tenant in NICo Core.
- Retrieve the Site IP Blocks using the [Retrieve All IP Blocks endpoint](/infra-controller/rest-api-reference/api-reference/ip-block/get-all-ipblock).
- Create a Network Allocation for Tenant using the [Create Allocation endpoint](/infra-controller/rest-api-reference/api-reference/allocation/create-allocation) referencing a Site IP Block
- Creating a Network Allocation will create a Tenant IP Block

## Typical API Call Flow for Tenant

- Accept the Tenant Account using the [Update Tenant Account endpoint](/infra-controller/rest-api-reference/api-reference/tenant-account/update-tenant-account).
- Retrieve available Sites using the [Retrieve All Sites endpoint](/infra-controller/rest-api-reference/api-reference/site/get-all-site) and choose a Site to create resources in. Any Site where the Tenant
has an Allocation will be returned.
- Create a VPC using the [Create VPC endpoint](/infra-controller/rest-api-reference/api-reference/vpc/create-vpc).
- Retrieve available Site IP Blocks using the [Retrieve All IP Blocks endpoint](/infra-controller/rest-api-reference/api-reference/ip-block/get-all-ipblock). Any IP Block for which the Tenant has received
a Network Allocation from the Provider will be returned.
- After the VPC reaches `Ready`, create the network resource that matches its
  `networkVirtualizationType`, referencing the VPC and a Tenant IP Block.
  - For an `FNN` VPC, create a VPC Prefix using the
    [Create VPC Prefix endpoint](/infra-controller/rest-api-reference/api-reference/vpc-prefix/create-vpc-prefix).
  - For a VPC whose `networkVirtualizationType` is
    `ETHERNET_VIRTUALIZER`, create an IPv4 Subnet from a `Ready`,
    tenant-allocated IPv4 IP Block at the VPC's Site using the
    [Create Subnet endpoint](/infra-controller/rest-api-reference/api-reference/subnet/create-subnet).
  - For a `FLAT` VPC, do not create a VPC Prefix or Subnet. When creating
    the Instance, set `autoNetwork` to `true` and omit `interfaces`.
- Create an Operating System using the [Create Operating System endpoint](/infra-controller/rest-api-reference/api-reference/operating-system/create-operating-system) specifying iPXE script and user data.
- Retrieve available Instance Types using the [Retrieve All Instance Types endpoint](/infra-controller/rest-api-reference/api-reference/instance-type/get-all-instance-type). Any Instance Type for which
the Tenant has received a Compute Allocation from the Provider will be returned.
- Create an Instance specifying the VPC, Operating System, and Instance Type.
  For an `FNN` or `ETHERNET_VIRTUALIZER` VPC, include the matching VPC Prefix
  or Subnet in `interfaces`.
