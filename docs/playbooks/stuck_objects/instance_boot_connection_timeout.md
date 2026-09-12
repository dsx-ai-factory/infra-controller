# Troubleshoot an Instance Boot Connection Timeout

Use this playbook when an assigned machine times out while downloading a tenant
operating-system image from a PXE or HTTP server hosted on another instance.

## Symptoms and Data to Collect

The console shows an iPXE timeout similar to:

```text
http://192.0.2.20:8080/boot/tenant-os... Connection timed out
(https://ipxe.org/4c0a6092)
```

Collect the following before changing the network configuration:

- The failing tenant instance ID.
- The complete URL from the console, including IP address and port.
- The tenant instance VPC ID.
- The PXE server instance ID and VPC ID.

If the instances use different VPCs without peering, the boot path is
unreachable. If they use the same VPC or peered VPCs, investigate routing and
tenant-defined security controls.

## Identify Both VPCs

1. Show the failing instance and record its `VPC ID`:

   ```bash
   nico-admin-cli instance show <tenant-instance-id>
   ```

1. Find the instance that owns the IP address in the timed-out URL:

   ```bash
   nico-admin-cli instance show | grep -F '<pxe-server-ip>'
   ```

1. Show that instance and record its `VPC ID`:

   ```bash
   nico-admin-cli instance show <pxe-server-instance-id>
   ```

## Check VPC Peering

If the VPC IDs differ, list the peerings for each VPC:

```bash
nico-admin-cli vpc-peering show --vpc-id <tenant-vpc-id>
nico-admin-cli vpc-peering show --vpc-id <pxe-server-vpc-id>
```

- If no peering connects the two VPCs, place both instances in one VPC or
  configure peering. Refer to
  [VPC Peering](../../manuals/vpc/vpc_peering_management.md).
- If the VPCs are already peered, or the instances use the same VPC, continue
  with path and policy checks.

## Check the Network Path

1. From an instance in the tenant VPC, test the PXE server IP and port.
1. Verify routes in both directions between the tenant and PXE server
   interfaces.
1. Check network security groups and host firewalls for the protocol and port
   in the failed URL. Refer to
   [Network Security Groups](../../manuals/networking/network_security_groups.md).
1. Verify that the HTTP service is listening on the PXE server and bound to the
   expected address.

## Verify Recovery

1. Confirm that the two instances are in the same VPC or connected by an active
   VPC peering.
1. Retry the instance boot.
1. Confirm that the console downloads the boot image without an iPXE timeout
   and that the managed host boots the tenant operating system.

The [iPXE error reference](https://ipxe.org/4c0a6092) describes the timeout
status reported by the boot client.
