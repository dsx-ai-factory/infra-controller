# Infiniband NIC and Port Selection

NVIDIA Infra Controller (NICo) supports multiple Infiniband enabled Network Interface Cards (NICs).
Each of those NICs might feature 1-2 physical ports, where each port allows
to connect the NIC to an Infiniband switch that is part of a certain Infiniband fabric.

This document describes how NICo enumerates available NICs and how it
makes them available for selection by a tenant during instance creation.

## Requirements

1. Hosts with the identical hardware configuration should be reported by NICo as having the exact same machine capabilities. E.g. a Machine having 2 Infiniband NICs that each have 2 ports that are connected to different Infiniband fabrics (4 fabrics in total), should be exactly reported as such.
2. If NICo tenants configure multiple hosts of the same instance type with the same infiniband configuration and run the same operating system, they should find exactly the exact same device names on the host. This allows them to e.g. statically use certain Infiniband devices in applications and containers without a need for complex run-time enumeration on the tenant side. E.g. a tenant should be able to rely on the devices `ibp202s0f0` and `ibp202s0f1` always being available and connected to their desired configuration.

## Recommendation

Each port of all supported Infiniband NICs is reported as a separate PCI device.
This makes those ports individually controllable and thereby mostly indistinguishable from a different physical NIC. E.g. an
infiniband capable ConnectX-6 NIC shows up on a Linux host as the following 2 devices:

```text
ubuntu@alpha:~$ lspci -v | grep Mellanox
ca:00.0 Infiniband controller: Mellanox Technologies MT28908 Family [ConnectX-6]
        Subsystem: Mellanox Technologies MT28908 Family [ConnectX-6]
ca:00.1 Infiniband controller: Mellanox Technologies MT28908 Family [ConnectX-6]
        Subsystem: Mellanox Technologies MT28908 Family [ConnectX-6]
```

Both show up as 2 independent infiniband devices:

```text
ls /sys/class/infiniband
ibp202s0f0  ibp202s0f1
```

This setup is mostly equivalent to a setup with 2 single-port Infiniband NICs.
Therefore we seem to have 2 options for presenting multi-port NICs to NICo users:

1. **Preferred:** Present each physical port of a NIC as a separate Infiniband NIC. The combination of a NIC & port is referred to as `device`.
2. Present a multi-port NIC as single NIC with multiple ports.

**Option 1) is preferred** because it simplifies the NICo data model and user experience: Users don't have to worry about 2 dimensions (NIC and port) when selecting an interface they want to configure - they only have to select a device. The fact that this interface is really a part of a hardware component that features 2 interfaces does not matter for the user workflows, where they want to use the infiniband device to send or receive data.

Various NICo user APIs can therefore by simplified to a point where no port information is required to be entered or shown. E.g. during Instance creation, the infiniband interface network configuration object only requires to pass a network device ID and no longer a port. In a similar fashion, the NICo internal data models for storing hardware information about infiniband devices can be simplified by dropping port data.

### How are the devices still related?

While the devices for the 2 ports seem mostly independent, there are still a few areas where they behave different than 2 independent cards:

1. Both devices report the same serial number.
2. The Mellanox firmware tools (`mlxconfig`, `mst`) show only a single device. E.g.

    ```text
    MST devices:
    ------------
    /dev/mst/mt4123_pciconf0         - PCI configuration cycles access.
                                       domain:bus:dev.fn=0000:ca:00.0 addr.reg=88 data.reg=92 cr_bar.gw_offset=-1
                                       Chip revision is: 00
    ```

   This breaks the illusion of 2 independent devices. Since the tenant can install and use those tools without the availability of a NIC firmware lockdown, they are able to inspect these properties. There however doesn't seem to be an obvious problem with it.
3. Due to 2), the port configurations for both ports are performed by manipulating a single device object in the Mellanox Firmware tools. E.g. both of the following commands

    ```text
    mlxconfig -d /dev/mst/mt4123_pciconf0 set LINK_TYPE_P1=1 LINK_TYPE_P2=1
    mlxconfig -d /dev/mst/mt4123_pciconf0.1 set LINK_TYPE_P1=1 LINK_TYPE_P2=1
    ```

   reconfigure both ports of a physical card from ethernet to infiniband (`LINK_TYPE=2` configures
   ethernet, `LINK_TYPE=1` configures infiniband), independent of whether the target
   device is the first port (`/dev/mst/mt4123_pciconf0` or 2nd port `/dev/mst/mt4123_pciconf0.1`).

   The same applies also for settings like `NUM_OF_VFS` and `SRIOV_EN`.

None of those reasons seem blockers for representing the ports as separate devices for NICo users:
Since NICo configures the device for tenants, they do not need to worry about the physical properties and can just
use the independent devices.

## Required changes

### NICo machine hardware enumeration

When NICo discovers a machine that is intended to be managed by the NICo site controller,
it enumerates its hardware details using the [nico-scout](https://github.com/dsx-ai-factory/infra-controller/tree/main/crates/scout) tool.

The tool reports all discovered hardware information (e.g. the number and type
of CPUs, GPUs, and network interfaces), and this information gets persisted
in the NICo database.

The reported information includes the list of Infiniband network interfaces. The
site controller needs the information to decide whether a certain Infiniband
configuration is valid for a Machine.

The NICo DiscoveryData model for Infiniband that is defined as follows almost
supports the preferred model:

```protobuf
message InfinibandInterface {
  PciDeviceProperties pci_properties = 1;
  string guid = 2;
}

message PciDeviceProperties{
  string vendor = 1;
  string device = 2;
  string path = 3;
  sint32 numa_node = 4;
  optional string description = 5;
}
```

In this model, every port of an Infiniband NIC already shows up as a separate
network device. E.g. a dual port ConnectX-6 NIC gets reported as:

```json
[
    {
        "guid": "1234",
        "pci_properties": {
            "path": "/devices/pci0000:c9/0000:c9:02.0/0000:ca:00.0/net/ibp202s0f0",
            "device": "0x101b",
            "vendor": "0x15b3",
            "numa_node": 1,
            "description": "MT28908 Family [ConnectX-6]"
        }
    },
    {
        "guid": "5678",
        "pci_properties": {
            "path": "/devices/pci0000:c9/0000:c9:02.0/0000:ca:00.1/net/ibp202s0f1",
            "device": "0x101b",
            "vendor": "0x15b3",
            "numa_node": 1,
            "description": "MT28908 Family [ConnectX-6]"
        }
    }
]
```

There however seem to be aspects that we can improve on:

1. The device and vendor names are passed as identifiers. If Tenants would want to
  use the same information to configure infiniband on an instance, the API calls
  to do that would contain the same non-descriptive data: Configure the first
  Infiniband interface of type `vendor: 0x15b3` and `device: 0x101b`. If we would
  use those fields to directly report the stringified versions, both the hardware
  report and the interface selection become more obvious to the user. We could
  also transmit both the IDs and the names. But as long as the IDs are not referenced
  in any other NICo APIs they do not seem too useful.
2. The device path is very OS and driver specific. A different path is reported
  depending on which of the various Mellanox drivers the NICo discovery image uses.
  We are be able to have more stable information by just persisting the PCI slot - either
  in the existing `path` field or a new `slot` field.
3. For multi-fabric support, we would include the identifier of the fabric that the
  device is connected to. This field can be empty in the MVP which supports only a single fabric.
  An empty field would always reference the default Infiniband fabric.
4. The `device` is referred to as `interface` in the discovery data API, which is
  inconsistent with the remaining terminology. We can rename `InfinibandInterface`
  to `InfinibandDevice`, and `infiniband_interfaces` to `infiniband_devices`.

With these changes, the submitted discovery information for the dual port NIC is:

```json
[
    {
        "guid": "1234",
        "fabric": "IbFabric1",
        "pci_properties": {
            "slot": "0000:ca:00.0",
            "vendor": "Mellanox Technologies",
            "device": "MT28908 Family [ConnectX-6]",
            "numa_node": 1,
            "description": "TBD (not strictly required)"
        }
    },
    {
        "guid": "5678",
        "fabric": "IbFabric2",
        "pci_properties": {
            "slot": "0000:ca:00.1",
            "vendor": "Mellanox Technologies",
            "device": "MT28908 Family [ConnectX-6]",
            "numa_node": 1,
            "description": "TBD (not strictly required)"
        }
    }
]
```

### Instance Type hardware capabilities

The NICo cloud backend currently displays Machine hardware details with slightly
less granularity than the site APIs. It uses a "Machine Capability" model that
tries to model how many components of a particular type a Machine includes. This
model reduces the amount of data that needs to be transferred between the Rest API
backend and NICo users since it doesn't need to explain every individual component
in detail. It also has the advantage that "machine capabilities" can describe
groups of similar machines ("instance types") instead of just a single machine.
Each machine that adheres to an instance type shares the same capabilities.

To support Infiniband, we can extend the existing capabilities model of the
NICo REST API backend to cover infiniband:

- Each Infiniband `device` will be represented by a capability that describes
  the device.
- The `type` field that is used for Infiniband devices would be `Infiniband`.
- The `name` field is the device name.
  The vendor can optionally be stored a separate `vendor` field.
  Alternatively the `name` field could store the concatenation of `vendor` and
  the device `name`. However since some APIs might just require the name, keeping
  the information separate seems clearer.
- Every physical port of an Infiniband NIC would be shown as one separate
  device (`count: 1`).
- For multi-fabric support, each entry would also be annotated with the `fabric`
  that the port is connected to.
- Virtual Functions (VF)s are not presented in this list of hardware capabilities,
  since their existence can be controlled by configuring the associated
  Physical Function (PF).
- Hardware details like PCI slots and hardware GUIDs are not shown in this
  model. Since they could be different from Machine to Machine, they cannot
  be used in the data model that is shared across a range of Machines.

```json
[
    {
        "type": "Infiniband",
        "name": "MT28908 Family [ConnectX-6]",
        "vendor": "Mellanox Technologies",
        "count": 1,
        "fabric": "IbFabric1",
    },
    {
        "type": "Infiniband",
        "name": "MT28908 Family [ConnectX-6]",
        "vendor": "Mellanox Technologies",
        "count": 1,
        "fabric": "IbFabric2",
    }
]
```

If both ports of the dual port NIC would be connected to the same fabric,
the NIC would be represented as a single entry:

```json
[
    {
        "type": "Infiniband",
        "name": "MT28908 Family [ConnectX-6]",
        "vendor": "Mellanox Technologies",
        "count": 2,
        "fabric": "IbFabric1",
    }
]
```

**Alternative**: If we would merge the device vendor and name fields, the entry would become:

```json
[
    {
        "type": "Infiniband",
        "name": "Mellanox Technologies MT28908 Family [ConnectX-6]",
        "count": 2,
        "fabric": "IbFabric1",
    }
]
```

### Instance creation APIs

When tenants create instances, they need to pass configuration that describes
how Infiniband interfaces on the new instance get configured.

For instance types that feature multiple devices, the tenant needs to select
which device to utilize.
This is especially important in cases where the ports of NICs are connected to
different fabrics.

An important aspect of instance configuration APIs is that they are decoupled
from the actual hardware. This allows configurations to be shared between all
instances of the same instance type. And it allows hardware (like an actual NIC)
to be replaced at runtime without changing the configuration objects. Therefore
the tenant facing configurations do not contain machine-specific identifiers
like a serial-number, MAC address or GUID on it. The tenant instead selects
the device via attributes that are common between all machines of the same instance
type.

Due to these constraints, we allow the tenant to select a device via
the following configuration object of type `InstanceInfinibandConfig`:

```json
{
    "ib_interfaces": [{
        // The first 3 parameters select the physical PCI device
        "device": "MT28908 Family [ConnectX-6]",
        "fabric": "IbFabric1",
        // Specifies that the n-th instance of the device will be used by this interface.
        // In this example the first ConnectX-6 NIC&port that utilizes
        // fabric "IbFabric1" will be configured.
        "device_instance": 0,

        // Select the PF or a specific VF. If a VF is required, the parameter
        // `virtual_function_id` also needs to be supplied
        "function_type": "PhysicalFunction",

        // Configures the partition this interface gets attached to
        "ib_partition_id": "some_partition_identifier",
    }, {
        "device": "MT28908 Family [ConnectX-6]",
        "fabric": "IbFabric1",
        "device_instance": 1,

        "function_type": "VirtualFunction",
        "virtual_function_id": 0,

        "ib_partition_id": "some_other_partition_identifier",
    }]
}
```

In this model, the `device` field references a particular Infiniband PCI device that
is reported in the `name` field of the `Infiniband` capability. It is used along with the `fabric`
attribute to select a device combination that is suitable for the purpose of
the tenant.

A capability that describes that a host supports multiple Infiniband devices
of the same model, attached to the same fabric (e.g. via `count: 2`) requires the
tenant needs to select via `device_instance` which particular instance of the device needs
to be configured.

The parameters `device`, `fabric` and `device_instance` always select the
physical PCI device (PhysicalFunction). A tenant uses the 2 additional parameters
`function_type` and `virtual_function_id` to configure a `device` that makes use of
a VirtualFunction on top of the selected PhysicalFunction.

#### Device vendor

The API described above fully omits the device vendor as a selection criteria.
This would make selection ambiguous in case a Machine would feature devices with the
same name but produced by different vendors.
Given all known devices that NICo will support initially are produced by Mellanox/NVIDIA,
this is however not an issue in the foreseeable future.
In case such a setup ever needs to be supported, an optional `device_vendor` field
could be added for each entry of `InstanceInfinibandConfig` to disambiguate the
target device in case of conflicts:

```json
{
    "ib_interfaces": [{
        "device": "Ambiguous Device",
        "vendor": "VendorA",
        "fabric": "IbFabric1",
        "device_instance": 0,
        "function_type": "PhysicalFunction",
        "virtual_function_id": 0,
        "ib_partition_id": "some_partition_identifier",
    }, {
        "device": "Ambiguous Device",
        "vendor": "VendorB",
        "fabric": "IbFabric1",
        "device_instance": 0,
        "function_type": "PhysicalFunction",
        "virtual_function_id": 0,
        "ib_partition_id": "some_other_partition_identifier",
    }]
}
```

The Web UI can combine all the necessary information into a single combo-box.
E.g. it could show a combo box with the following content:

```text
 +-----------------------------------------------------------------------+
 | Select Device                                                         |
 +-----------------------------------------------------------------------+
 | [IbFabric1]: Mellanox Technologies MT28908 Family [ConnectX-6] - Nr 0 |
 | [IbFabric1]: Mellanox Technologies MT28908 Family [ConnectX-6] - Nr 1 |
 +-----------------------------------------------------------------------+
```

This single selector would provide all the information that all layers need
to configure the interface according to user requirements.

### Mapping from Tenant Configuration to actual hardware interfaces

If a tenant selects a network interface, we need to be able to
**uniquely** map the interface to a specific hardware interface.

E.g. this instance configuration request:

```json
{
    "device": "MT28908 Family [ConnectX-6]",
    "fabric": "IbFabric1",
    "device_instance": 1,
}
```

needs to map to the following hardware interface information:

```json
{
    "guid": "1234",
    "fabric": "IbFabric1",
    "pci_properties": {
        "slot": "0000:ca:00.0",
        "vendor": "Mellanox Technologies",
        "device": "MT28908 Family [ConnectX-6]",
        "numa_node": 1,
        "description": "TBD (not strictly required)"
    }
}
```

The `fabric` is directly copied, and the `model` fields map
to the `device` fields. The `vendor` field can be resolved by looking for any
`device` with the specified device name.
Thereby the only challenge is how to map `instance` in a non-ambiguous fashion.
We can achieve this by sorting the interfaces based on the PCI `slot`,
and pick the N-th slot that satisfies the criteria.

**Example 2:**

Assuming the following hardware information is available:

```json
[{
    "guid": "1234",
    "fabric": "IbFabric1",
    "pci_properties": {
        "slot": "0000:cb:00.0",
        "vendor": "Mellanox Technologies",
        "device": "MT28908 Family [ConnectX-6]"
    }
},{
    "guid": "2345",
    "fabric": "IbFabric2",
    "pci_properties": {
        "slot": "0000:cd:00.0",
        "vendor": "Mellanox Technologies",
        "device": "MT28908 Family [ConnectX-6]"
    }
},{
    "guid": "3456",
    "fabric": "IbFabric1",
    "pci_properties": {
        "slot": "0000:ea:00.0",
        "vendor": "Mellanox Technologies",
        "device": "MT28908 Family [ConnectX-6]"
    }
},{
    "guid": "4567",
    "fabric": "IbFabric2",
    "pci_properties": {
        "slot": "0000:eb:00.0",
        "vendor": "Mellanox Technologies",
        "device": "MT28908 Family [ConnectX-6]"
    }
}]
```

In this example a selection of

- `{device: "Mellanox ... MT28908 ...", fabric: "IbFabric1", device_instance: 0}`
  would select the interface with GUID `1234`.
- `{device: "Mellanox ... MT28908 ...", fabric: "IbFabric1", device_instance: 1}`
  would select the interface with GUID `3456`.
- `{device: "Mellanox ... MT28908 ...", fabric: "IbFabric2", device_instance: 0}`
  would select the interface with GUID `2345`.
- `{device: "Mellanox ... MT28908 ...", fabric: "IbFabric2", device_instance: 1}`
  would select the interface with GUID `4567`.

**An alternative seems to be to sort the interfaces by hardware `guid` instead of
PCI slot.** The downside of this mapping is that it won't be stable
across machines of the same instance type. E.g. the selection in our example
might sometimes select a device in slot 4 and sometimes a device in slot 5 in case the
GUIDs are different. Since the PCI slots are assumed to be deterministic
for Machines with the same hardware configuration, tenants can assume their selection
always affects the exact same piece of hardware.

### NICo Metadata Service (FMDS)

The NICo Metadata Service (FMDS) provides the Tenant's software
running on instance the capability to identify the infiniband configuration at
runtime. It also provides the ability to execute a configuration script
which configures the local Infiniband interfaces for the operating mode that the
Tenant desired for this instance. This script needs to configure all network interfaces
on the host. This includes

- setting the correct number of VFs per physical device
- writing GUIDs that NICo allocated for VF interfaces to the locations the OS
  expects them

Applying these settings configure the interfaces in software in a way that
allows them to send their traffic successfully to the connected Infiniband switches.

To perform this job, FMDS returns the applied instance configuration -
which is the desired `InstanceInfinibandConfig` plus the configuration data that
NICo allocates on behalf the tenant. This would be mostly the GUIDs.

Putting it together, the tenant machine would retrieve the following data via
FMDS, in a format that is still TBD:

```json
{
    "config": {
        "infiniband": {
            "ib_interfaces": [{
                // Selects the device (NIC and Port)
                "device": "MT28908 Family [ConnectX-6]",
                "fabric": "IbFabric1",
                "device_instance": 0,

                // Select the PF or a specific VF
                "function_type": "VirtualFunction",
                "virtual_function_id": 0,

                // Configures the partition this interface gets attached to
                "ib_partition_id": "some_partition_identifier",
            }]
        }
    },
    "status": {
        "infiniband": {
            "ib_interfaces": [{
                "guid": "1234",
                "lid": 123,
                "addresses": ["5.6.7.8", "::8:1:3:4:5"]
            }]
        }
    }
}
```

The FMDS client needs to perform the mapping from configuration
parameters to the actual Linux devicename (in `/sys/class/infiniband`) to apply
the necessary configuration. This requires the same knowledge about
the unique mapping of the configuration to the actual hardware that is residing
in NICo. A challenge here is however that the client running
on a tenants host is not able to resolve the fabric per interface. Since
the fabric is one part of the mapping in a multi-fabric context, the mapping would
no longer be unambiguous. An alternative to this is to extend
`status.infiniband.ib_interfaces` in a way that allows the software on the tenant
host to easier lookup the necessary device. E.g. we would return the hardware
guid of the associated physical function in every interface. Along:

```json
{
    "status": {
        "infiniband": {
            "ib_interfaces": [{
                "pf_guid": "1234",
                "guid": "1234",
                "lid": 123,
                "addresses": ["5.6.7.8", "::8:1:3:4:5"]
            }, {
                "pf_guid": "1234",
                "guid": "3457",
                "lid": 124,
                "addresses": ["5.6.7.9", "::8:1:3:4:56"]
            }]
        }
    }
}
```

## Alternatives considered

### Interface configuration via unique PCI address (`device_slot`)

The APIs described above make it slightly ambiguous which `device` (in terms of
PCI slot) a tenant would use for an interface. The tenant specifies the following
in an instance creation request

```json
{
    "device": "MT28908 Family [ConnectX-6]",
    "fabric": "IbFabric1",
    "device_instance": 2,
    "ib_partition_id": "partition_a"
}
```

and the system would look up what PCI address `device_instance: 2` refers to.
This mapping might not be obvious in a system which features multiple NICs with
one or multiple ports, and each of them connected to a mix of fabrics.
E.g. a tenant could be surprised that `device_instance` can have the
same value for 2 devices that utilize a different fabric, since the index is
per device & fabric combination. E.g. the following configuration is valid:

```json
[{
    "device": "MT28908 Family [ConnectX-6]",
    "fabric": "IbFabric1",
    "device_instance": 1,
    "ib_partition_id": "Partition_A"

},
{
    "device": "MT28908 Family [ConnectX-6]",
    "fabric": "IbFabric2",
    "device_instance": 1,
    "ib_partition_id": "Partition_B"
}]
```

It would select the 2nd device of type ConnectX-6 that is connected to `IbFabric1`
and configure it to use partition `Partition_A`. Whereas the 2nd device of type
ConnectX-6 that is connected to `IbFabric2`` will use partition `Partition_B`.

**To avoid this concern, we can move towards an API which uses the unique
PCI address/slot** for instance creation. In this model, a tenant would configure
the instance with the following request

```json
{
    "ib_interfaces": [{
        // This single parameters selects the device (NIC, Port and thereby Fabric)
        "device_slot": "0000:ca:00.0",

        // Select the PF or a specific VF. If a VF is required, the parameter
        // `virtual_function_id` also needs to be supplied
        "function_type": "PhysicalFunction",

        // Configures the partition this interface gets attached to
        "ib_partition_id": "some_partition_identifier",
    }, {
        "device_slot": "0000:ca:00.1",

        "function_type": "VirtualFunction",
        "virtual_function_id": 0,

        "ib_partition_id": "some_other_partition_identifier",
    }]
}
```

The hardware inventory data model already provides the `slot` address. Therefore
no additional changes are required here.

However the machine capability model needs to be extended to include the `slot`
information, since it is used by the NICo Admin UI to explain the tenant what devices
can be configured. E.g. the reported machine capability data could be:

```json
[
    {
        "type": "Infiniband",
        "name": "MT28908 Family [ConnectX-6]",
        "vendor": "Mellanox Technologies",
        "count": 1,
        "fabric": "IbFabric1",
        "slot": "0000:ca:00.0"
    },
    {
        "type": "Infiniband",
        "name": "MT28908 Family [ConnectX-6]",
        "vendor": "Mellanox Technologies",
        "count": 1,
        "fabric": "IbFabric2",
        "slot": "0000:ca:00.1"
    }
]
```

Since the slot is unique per device, the `count` field could never be anything
different than `1` for Infiniband capabilities.

#### Downsides of the `device_slot` based API

The `device_slot` based API is not preferred, because it makes it harder for API
users to spin up an instance without an excessive amount of "prior knowledge".

In the recommended model tenants that require to configure a single Infiniband
Interface will likely just need to specify the device name which is well known
(e.g. `MT28908 Family [ConnectX-6]`). The fabric field might not need to be specified
since it would be the site default, and the `device_instance` could simply be 0.

This simplicity would remain even if machine contains multiple devices that are
connected to the same fabric, and where the tenant wants to configure all of them.

The advantages of the `device_slot` based APIs would only show up in complex
deployments with multiple NICs **and** multiple Fabrics.

Another downside is that the `device_slot` based API strictly requires the
PCI `slot` addresses to be consistent between all machines of a certain instance type.
The preferred model can support different PCI `slot` addresses to the extent that
instance creation and configuration would still work as expected.

## Other considerations

### Terminology

A variety of different terms had been used to reference "things to send/receive infiniband traffic":

- Network Interface Cards (NICs)
- Network Adapters
- Host Channel Adapters (HCAs)
- Devices
- Interfaces

Each of those terms is sometimes used to reference to a full Infiniband card that
might provide more than 1 port, to just a single port on the card, or even to
a purely virtual output that is provided by the card (a VF).

To avoid confusion, The APIs presented in this document are consistently using the following
terms with meanings defined as follows:

#### Devices

- A `device` is a physical PCI device which can be used to send and receive Infiniband traffic.
- The operating system of a Tenants host shows each device separately. E.g.
  on Linux, each `device` shows up under `/sys/class/infiniband/`.
- A Network Interface Card (NIC) can provide 1 or more `device`s.
- The "Physical Function" (PF) of each PCI device leads to a `device` being
  made available. Besides that the usage of "Virtual Functions" (VFs) allows
  to configure additional `device`s that share the same hardware.

#### Interfaces

An `interface` represents a `device` that is configured towards a certain purpose.
For example a tenant can configure the first `device` of a certain type on their
host to be connected to `Partition A`, and the second `device` to `Partition B`.

Therefore, BB refers to `interfaces` when in instance configuration APIs and
when providing status information about running instances.

#### Open questions

- Should NICo documentation settle on a specific term to reference a full NIC?
  E.g. `NIC` or `Adapter`? It might be necessary in order to explain workflows
  for tools which do only show the complete NIC and not individual devices (e.g. `mlxconfig`)

### Numa Node awareness

We discussed a bit on whether the NUMA node that a device is connected to should be
exposed to the user, or whether a tenant should even be able to select a device by
NUMA node. This would help the tenant to achieve better locality between the device
and a connected GPU for some applications.

While this seems like an interesting feature, it would also complicate the APIs
even more by introducing yet another selector.

Even without introducing NUMA awareness on the API layer, tenants should be
able to achieve the same goal by exploiting the fact that the device mapping is
equivalent for all machines of an instance type: The Tenant can create a
test instance, and determine based on introspection of this particular instance
whether they have a suitable device configuration. They can modify the interface
selection (via `instance`) until they achieve their ideally desired configuration.
Once they have found the desired configuration, they would be able to carry it
over to other instances using the exact same configuration.
