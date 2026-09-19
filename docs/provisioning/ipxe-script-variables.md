# iPXE Script Variables

When a machine network-boots, NICo's PXE service serves it an iPXE script. That script sets a small
number of variables before running the boot instructions, so anything those instructions reference—
including a custom `ipxeScript` supplied through the REST API or an operating-system definition—can
use them instead of hard-coding a site's URLs.

The variables are set by the PXE service, so they always carry the URLs that particular machine can
reach. A machine on a segment that reaches NICo under a different name gets that name substituted
automatically; a script that hard-codes a hostname instead does not.

## Available Variables

| Variable | Value | Use It For |
|---|---|---|
| `${base-url}` | The boot-artifact tree, `…/public/blobs/` | Chaining to a kernel, initrd, or EFI image NICo serves |
| `${tenant-cloudinit-url}` | The NoCloud datasource for an assigned instance | `ds=nocloud-net;s=${tenant-cloudinit-url}` in an OS install script |
| `${dpu-cloudinit-url}` | The BlueField kickstart endpoint | Internal—DPU provisioning only |
| `${scout-cloudinit-url}` | The discovery OS's NoCloud datasource | Internal—set on the Scout kernel command line only |
| `${cloudinit-url}` | Alias of `${tenant-cloudinit-url}` | **Deprecated**—refer to [Deprecations](#deprecations). |

`${dpu-cloudinit-url}` and `${scout-cloudinit-url}` are listed for completeness. They serve NICo's own
provisioning flows, and referencing them from a tenant script will not do anything useful.

## Typical Use

The following example boots an OS installer with a cloud-init data source:

```text
#!ipxe
kernel ${base-url}/internal/x86_64/vmlinuz ip=dhcp autoinstall ds=nocloud-net;s=${tenant-cloudinit-url} initrd=initrd.magic
initrd ${base-url}/internal/x86_64/initrd
boot
```

The `${tenant-cloudinit-url}` data source serves the `user-data`, `meta-data`, `vendor-data`, and
`network-config` documents that the cloud-init NoCloud data source expects. `user-data` contains the
`userData` value from the machine's operating system. If no value is set, NICo serves an empty
document, and the boot continues.

## Deprecations

### ${cloudinit-url} Variable

**Deprecated in favor of `${tenant-cloudinit-url}`. Update scripts that use it.**

`${cloudinit-url}` is the original single cloud-init URL. Each consumer now has a separate prefix:
tenant instances, DPUs being provisioned, and hosts running the discovery OS. The prefix identifies
the consumer without requiring the service to infer it from ambiguous data.

`${cloudinit-url}` remains an alias for `${tenant-cloudinit-url}` and continues to work unchanged, so
existing scripts keep booting. This alias is deprecated. Scripts referencing it should move to
`${tenant-cloudinit-url}`; the substitution is the only change required.
