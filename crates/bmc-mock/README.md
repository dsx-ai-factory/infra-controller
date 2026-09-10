# bmc-mock

`bmc-mock` is a standalone HTTPS Redfish simulator. Its optional libvirt mode
maps one `bmc-mock` process or container to one libvirt domain, matching the
real-world relationship between a Redfish endpoint and its host.

## Build the libvirt image

Run from the infra-controller repository root:

```console
docker buildx build \
  --platform linux/amd64 \
  -f crates/bmc-mock/Dockerfile \
  --build-arg VERSION=0.1.0 \
  -t bmc-mock:latest \
  --load \
  .
```

Use `--push` instead of `--load` when publishing directly to a registry.
Omit `--platform` to build an image matching the local Docker engine's native
architecture. The Dockerfile supports both `linux/amd64` and `linux/arm64`
without running the Rust compiler under emulation.

## Run one endpoint per virtual machine

The container needs access to the libvirt daemon that owns the domain. On a
Linux host, mount the daemon socket and configure the domain name:

```console
docker run --rm \
  --name bmc-node-01 \
  -p 1266:1266 \
  -v /run/libvirt/libvirt-sock:/run/libvirt/libvirt-sock \
  bmc-mock:latest \
  --machine-role host \
  --state-backend libvirt \
  --libvirt-domain dsx-node-01 \
  --hardware-profile generic_ami \
  --libvirt-uri qemu:///system
```

The simulator never infers a hardware identity from a VM. Explicitly configured
endpoints select a role, state backend, and hardware profile. The older
`--libvirt-domain DOMAIN --hardware-profile PROFILE` form remains accepted as
shorthand for `--machine-role host --state-backend libvirt`.

| Hardware profile | DPU count |
| --- | ---: |
| `dell-poweredge-r750` | Variable; defaults to 0 |
| `dell-poweredge-r760-bf4` | 1 |
| `wiwynn-gb200-nvl` | 2 |
| `lenovo-gb300-nvl` | 1 |
| `nvidia-dgx-gb300` | 1 |
| `supermicro-gb300-nvl` | 1 |
| `nvidia-dgx-vr` | 1 |
| `nvidia-dgx-h100` | 1 |
| `generic-ami` | Variable; defaults to 0 |
| `generic-supermicro` | Variable; defaults to 0 |
| `hpe-proliant-dl380a-gen11` | Variable; defaults to 0 |

Use `--dpu-count` to populate a variable-count host profile. For a fixed-count
profile, the flag is optional and is validated if supplied.

Multiple containers on a bridge network can all listen on their internal port
1266 and use their container or service names as distinct BMC endpoints. Only
published host ports need to be unique.

Clients must discover the ComputerSystem instead of assuming a system ID:

```console
curl --insecure https://localhost:1266/redfish/v1/Systems
```

Do not assume the first member is the host. Some profiles expose both an HGX
baseboard and a controllable host. Fetch the members and select the resource
that contains `PowerState` for power, boot override, BIOS, and VirtualMedia
requests.

The default libvirt device targets are `sdb` for `Cd` and `sdc` for `ConfigCd`.
Confirm those targets are unused with `virsh domblklist DOMAIN --details` before
inserting media. HTTP or HTTPS ISO URLs must be reachable from the host running
the QEMU process. File paths must exist in that host's filesystem.

## Apply boot-order changes with libvirt

Persistent Redfish boot-order updates change the saved libvirt domain
configuration. For a running VM, request `PowerCycle` to apply a changed order,
or shut it down and then request `On`. Starting a stopped VM with `On` or
`ForceOn` also applies the effective boot selection.

`GracefulRestart` and `ForceRestart` retain their normal libvirt reboot and
reset behavior: they keep the running VM's boot configuration. They do not
activate a newly saved boot order. Temporary boot-source overrides likewise
need a cold start to take effect; after a one-shot override, the backend
restores the persistent selection in the saved configuration for a subsequent
cold start.

## Run a separate DPU endpoint

A DPU is independently addressable hardware, so expose it through its own
`bmc-mock` process rather than adding its ComputerSystem to the host BMC
endpoint. This example exposes the second DPU from a Wiwynn GB200 host and uses
the in-process state machine because no DPU VM exists:

```console
bmc-mock \
  --port 8102 \
  --machine-role dpu \
  --state-backend internal \
  --hardware-profile wiwynn_gb200_nvl \
  --dpu-index 1 \
  --instance-index 1
```

Start the host and all of its DPU endpoints with the same `--hardware-profile`,
`--dpu-count`, and `--instance-index`. Their generated DPU serial numbers and
MAC addresses will then agree. `--state-backend libvirt --libvirt-domain NAME`
can be used for a DPU endpoint when a separate DPU VM exists.

The internal backend maintains power state entirely within the process. It does
not affect the host VM and intentionally does not expose libvirt virtual media.
