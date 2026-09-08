# Example API Workflows

This section provides example REST API workflows for common NICo tasks. All examples use `curl` for API calls and assume a bearer token authentication system is in place.

## Viewing Site Inventory

<AccordionGroup>
  <Accordion title="View Your Sites">
    Use the value of `id` from the output of the preceding example as the value for the infrastructureProviderId URL parameter:
    <Code src="snippets/input/view_sites.sh" title="Example Call" />
    The Site ID in the response is a required input for many configuration requests.
    <Code src="snippets/output/view_sites.json" title="Example Response" />
  </Accordion>
  <Accordion title="View Your Machines">
    Use the `id` value from the output of the preceding examples as the values for the `infrastructureProviderId` and `siteId` URL parameters. The following sample command uses URL parameters to filter for machines that are in a `Ready` state and are not assigned an instance type.
    <Code src="snippets/input/view_machines.sh" title="Example Call" />
    <Code src="snippets/output/view_machines.json" title="Example Response" />
  </Accordion>
  <Accordion title="View Existing IP Blocks">
    Use the value of `id` from the output of the preceding example as the value for the `infrastructureProviderId` and `siteId` URL parameters.
    <Code src="snippets/input/view_ip_blocks.sh" title="Example Call" />
    <Code src="snippets/output/view_ip_blocks.json" title="Example Response" />
  </Accordion>
</AccordionGroup>

## Managing Virtual Private Clouds

<Note>
`networkVirtualizationType` supports two VPC networking mechanisms: **FNN** is recommended for all deployments that include DPUs (instances in FNN VPCs reference a `vpcPrefixId` in their interface configuration.); **Legacy** VPCs use subnets instead of VPC prefixes. New deployments with DPUs should use FNN exclusively.

`tenantId` is the ID of the Tenant organization generated during setup. This value is distinct from the organization name used in the API URL path.
</Note>

<AccordionGroup>
  <Accordion title="Create a VPC">
    Create the VPC and specify a name.
    <Code src="snippets/input/create_vpc.sh" title="Example Call" />
    <Code src="snippets/output/create_vpc.json" title="Example Response" />
  </Accordion>
  <Accordion title="(Optional) Confirm the VPC Status">
    Poll the VPC endpoint to confirm the status changes to `Ready`:
    <Code src="snippets/input/poll_vpc_status.sh" title="Example Call" />
    <Code src="snippets/output/poll_vpc_status.json" title="Example Response" />
  </Accordion>
  <Accordion title="Add an Instance with a Single Interface">
    Add one or more compute instances. The `interfaces` array configures how each DPU port is assigned a network address. For FNN VPCs, specify a `vpcPrefixId`; for Legacy VPCs, specify a `subnetId`.

    The `isPhysical` flag determines whether a physical function (PF) or a virtual function (VF) is configured on the DPU port. Set `isPhysical: true` for standard bare-metal configurations. VFs (`isPhysical: false`) are used when running VMs on the host that require direct hardware passthrough of a DPU port.
    <Code src="snippets/input/create_instance_single_interface.sh" title="Example Call" />
    <Code src="snippets/output/create_instance_single_interface.json" title="Example Response" />
  </Accordion>
  <Accordion title="Add an Instance with Multiple Interfaces">
    <Code src="snippets/input/create_instance_multiple_interfaces.sh" title="Example Call" />
    <Code src="snippets/output/create_instance_multiple_interfaces.json" title="Example Response" />
  </Accordion>
  <Accordion title="(Optional) Confirm the Instance Status">
    Poll the Instance to confirm the status changes to `Ready`:
    <Code src="snippets/input/poll_instance_status.sh" title="Example Call" />
    <Code src="snippets/output/poll_instance_status.json" title="Example Response" />
  </Accordion>
</AccordionGroup>

## Allocating Machines

Before allocating Machines, you should have the ID of the Instance Type. You can get the ID by making a `GET` request to the `/v2/org/{org-name}/nico/instance/type` endpoint and specifying the `infrastructureProviderId=<provider-id>` and `siteId=<site-id>` parameters.

<AccordionGroup>
  <Accordion title="Allocate Compute Instances">
    <Code src="snippets/input/allocate_machines.sh" title="Example Call" />
    <Code src="snippets/output/allocate_machines.json" title="Example Response" />
  </Accordion>
</AccordionGroup>

## Assigning Instance Types to Machines

Before assigning Instance Types, you should have the ID of the Instance Type. You can get the ID by making a `GET` request to the `/v2/org/{org-name}/nico/instance/type` endpoint and specifying the `siteId=<site-id>` parameter.

<AccordionGroup>
  <Accordion title="Get Machines Without an Instance Type">
    Get the machines that do not have an instance type assigned and that report a status of `Ready`:
    <Code src="snippets/input/get_machines_without_instance_type.sh" title="Example Call" />
    <Code src="snippets/output/get_machines_without_instance_type.json" title="Example Response" />
  </Accordion>
  <Accordion title="Associate Machines with an Instance Type">
    You can specify one or more machine IDs in the `machineIds` parameter.
    <Code src="snippets/input/associate_machines_with_instance_type.sh" title="Example Call" />
    <Code src="snippets/output/associate_machines_with_instance_type.json" title="Example Response" />
  </Accordion>
</AccordionGroup>

## Managing Operating Systems

Before adding an operating system image, ensure you have:

- An iPXE script as a one-line string.
- **Optional**: A cloud-init script as a one-line string.
- For the iPXE string and cloud-init string, replace newline characters with `\n` and escape quotation marks with `\"`.
- Your Tenant ID.

<AccordionGroup>
  <Accordion title="Add an Operating System Image">
    <Code src="snippets/input/add_operating_system.sh" title="Example Call" />
    <Code src="snippets/output/add_operating_system.json" title="Example Response" />
  </Accordion>
  <Accordion title="Add an Image-Based Operating System">
    The `imageDisk` value identifies the whole disk that NICo overwrites. A `/dev/disk/by-id/` selector is stable across enumeration changes but must exist on every eligible machine. Use `smallest` only when the intended boot disk is consistently the smallest whole disk. See [Image-Based Operating Systems](../../docs/configuration/image-based-operating-systems.md) for selection, update, and filesystem-identity behavior.
    <Code src="snippets/input/add_image_operating_system.sh" title="Example Call" />
    <Code src="snippets/output/add_image_operating_system.json" title="Example Response" />
  </Accordion>
</AccordionGroup>

## Managing Subnets and VPC Prefixes

Before managing Subnets, ensure you have at least one IP Block allocated so that you can add a Subnet of the IP Block address space.

<AccordionGroup>
  <Accordion title="Add a Subnet">
    Add one or more subnets. The following command sample shows how to add one subnet.
    <Code src="snippets/input/create_subnet.sh" title="Example Call" />
    <Code src="snippets/output/create_subnet.json" title="Example Response" />
  </Accordion>
  <Accordion title="(Optional) Confirm the Subnet Status">
    Poll the subnet endpoint to confirm that the status changes to `Ready`:
    <Code src="snippets/input/poll_subnet_status.sh" title="Example Call" />
    <Code src="snippets/output/poll_subnet_status.json" title="Example Response" />
  </Accordion>
  <Accordion title="Add a VPC Prefix">
    The following command sample shows how to add one VPC prefix. You can also add multiple VPC prefixes at once.
    <Code src="snippets/input/create_vpc_prefix.sh" title="Example Call" />
    <Code src="snippets/output/create_vpc_prefix.json" title="Example Response" />
  </Accordion>
</AccordionGroup>

## Managing IP Blocks

<AccordionGroup>
  <Accordion title="Add an IP Block">
    <Code src="snippets/input/add_ip_block.sh" title="Example Call" />
    <Code src="snippets/output/add_ip_block.json" title="Example Response" />
  </Accordion>
  <Accordion title="Allocate an IP Block">
    <Code src="snippets/input/allocate_ip_block.sh" title="Example Call" />
    <Code src="snippets/output/allocate_ip_block.json" title="Example Response" />
  </Accordion>
</AccordionGroup>

## Managing Network Security Groups

<AccordionGroup>
  <Accordion title="Retrieve All Network Security Groups">
    <Code src="snippets/input/get_nsgs.sh" title="Example Call" />
    <Code src="snippets/output/get_nsgs.json" title="Example Response" />
  </Accordion>
  <Accordion title="Create a Network Security Group that Limits Traffic">
    <Code src="snippets/input/create_nsg_limit_traffic.sh" title="Example Call" />
    <Code src="snippets/output/create_nsg_limit_traffic.json" title="Example Response" />
  </Accordion>
  <Accordion title="Create a Network Security Group that Permits All Traffic">
    <Code src="snippets/input/create_nsg_permit_all.sh" title="Example Call" />
    <Code src="snippets/output/create_nsg_permit_all.json" title="Example Response" />
  </Accordion>
  <Accordion title="Modify the Rules for a Network Security Group">
    <Code src="snippets/input/update_nsg_rules.sh" title="Example Call" />
    <Code src="snippets/output/update_nsg_rules.json" title="Example Response" />
  </Accordion>
</AccordionGroup>

## Accessing the Serial Console

<AccordionGroup>
  <Accordion title="Enable the Serial Console on a Compute Instance">
    <Code src="snippets/input/enable_serial_console.sh" title="Example Call" />
    <Code src="snippets/output/enable_serial_console.json" title="Example Response" />
  </Accordion>
  <Accordion title="Add an SSH Key Group">
    The SSH key should be in RSA, ECDSA, or ED25519 format. Add the SSH Key Group:
    <Code src="snippets/input/add_ssh_key_group.sh" title="Example Call" />
    Note the value of the `version` field — you will need it to update the SSH Key Group.
    <Code src="snippets/output/add_ssh_key_group.json" title="Example Response" />
  </Accordion>
  <Accordion title="Add a Public SSH Key">
    <Code src="snippets/input/add_ssh_key.sh" title="Example Call" />
    <Code src="snippets/output/add_ssh_key.json" title="Example Response" />
  </Accordion>
  <Accordion title="Add the Public SSH Key to the Key Group">
    Specify the new and existing key IDs to keep in the `sshKeyIds` field.
    <Code src="snippets/input/add_key_to_group.sh" title="Example Call" />
    <Code src="snippets/output/add_key_to_group.json" title="Example Response" />
  </Accordion>
  <Accordion title="Add Sites to the Key Group">
    Specify the new and existing Site IDs to keep in the `siteIds` field. You can combine this step and the preceding step by specifying both the SSH Key IDs and Site IDs in the same request.
    <Code src="snippets/input/add_sites_to_key_group.sh" title="Example Call" />
    <Code src="snippets/output/add_sites_to_key_group.json" title="Example Response" />
  </Accordion>
  <Accordion title="View Instances That Belong to the VPC">
    <Code src="snippets/input/get_vpc_instances.sh" title="Example Call" />
    <Code src="snippets/output/get_vpc_instances.json" title="Example Response" />
  </Accordion>
  <Accordion title="Parse IP Addresses from the Response">
    Use a command like the following to retrieve the IP Addresses from the response, then access the host or application deployed on the Instance.
    <Code src="snippets/input/parse_instance_ip_addresses.sh" title="Example Call" />
    <Code src="snippets/output/parse_instance_ip_addresses.json" title="Example Response" />
  </Accordion>
</AccordionGroup>
