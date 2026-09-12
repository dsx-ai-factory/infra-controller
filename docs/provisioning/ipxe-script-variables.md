# iPXE Script Variables

When a machine network-boots, NICo's PXE service serves it an iPXE script. That script sets a small
number of variables before running the boot instructions, so anything those instructions reference —
including a custom `ipxeScript` supplied through the REST API or an operating-system definition — can
use them instead of hard-coding a site's URLs.

The variables are set by the PXE service, so they always carry the URLs that particular machine can
reach. A machine on a segment that reaches NICo under a different name gets that name substituted
automatically; a script that hard-codes a hostname instead does not.

## Available variables

| Variable | Value | Use it for |
|---|---|---|
| `${base-url}` | The boot-artifact tree, `…/public/blobs/` | Chaining to a kernel, initrd, or EFI image NICo serves |
| `${tenant-cloudinit-url}` | The NoCloud datasource for an assigned instance | `ds=nocloud-net;s=${tenant-cloudinit-url}` in an OS install script |
| `${dpu-cloudinit-url}` | The BlueField kickstart endpoint | Internal — DPU provisioning only |
| `${scout-cloudinit-url}` | The discovery OS's NoCloud datasource | Internal — set on the Scout kernel command line only |
| `${cloudinit-url}` | Alias of `${tenant-cloudinit-url}` | **Deprecated** — see below |

`${dpu-cloudinit-url}` and `${scout-cloudinit-url}` are listed for completeness. They serve NICo's own
provisioning flows, and referencing them from a tenant script will not do anything useful.

## Typical use

Booting an OS installer against a cloud-init datasource:

```text
#!ipxe
kernel ${base-url}/internal/x86_64/vmlinuz ip=dhcp autoinstall ds=nocloud-net;s=${tenant-cloudinit-url} initrd=initrd.magic
initrd ${base-url}/internal/x86_64/initrd
boot
```

The datasource that `${tenant-cloudinit-url}` points at serves the `user-data`, `meta-data`,
`vendor-data`, and `network-config` documents cloud-init's NoCloud datasource expects. `user-data` is
the `userData` value on the machine's operating system; when none is set, an empty document is served
rather than an error, so a boot without user-data still completes.

## Deprecations

### `${cloudinit-url}`

**Deprecated in favor of `${tenant-cloudinit-url}`. Update scripts that use it.**

`${cloudinit-url}` was the single cloud-init URL from when one endpoint served every consumer. The
cloud-init routes are now split by consumer — a tenant instance, a DPU being provisioned, and a host
running the discovery OS each have their own prefix — so that the URL a machine boots with identifies
what it is, rather than the service inferring it from data that cannot cleanly distinguish the three.

`${cloudinit-url}` remains an alias for `${tenant-cloudinit-url}` and continues to work unchanged, so
existing scripts keep booting. It will be removed in a future release. Scripts referencing it should
move to `${tenant-cloudinit-url}`; the substitution is the only change required.
