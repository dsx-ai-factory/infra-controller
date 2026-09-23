# Discovery OS cloud-init (Scout) <Badge intent="info">v2.2</Badge> <Badge intent="launch" minimal>New</Badge>

Scout is the NVIDIA Infra Controller (NICo) discovery OS. Site operators can
use cloud-init snippets to customize Scout during discovery boots. Most sites
do not need this customization; when no snippets are configured, PXE serves a
valid no-op cloud-config document.

## Before You Begin

<Warning>
Never reboot a host from a Scout snippet. Scout runs from a RAMdisk, so a reboot
causes a full re-PXE, discards the current root filesystem, and runs the same
snippets again. A snippet that reboots can create an endless discovery boot
loop.
</Warning>

Changes that require a reboot, such as kernel parameters, module changes, or
firmware activation, belong in the Scout image or in a later lifecycle state
that owns the reboot.

Make every snippet idempotent. Snippets run on every discovery boot against a
clean root filesystem, so effects outside that filesystem must be safe to
repeat. Examples include calling an API, modifying BMC or firmware state,
writing to persistent storage, or consuming a license seat.

Do not put secrets such as passwords, tokens, or private keys in snippets. PXE
serves them through `/public` without authentication, so any client that can
reach the service can read them. If a snippet needs privileged material,
retrieve it at runtime from an authenticated source.

## Create and Name the Snippets

Create one or more files containing user-data that cloud-init recognizes. A
cloud-config snippet must begin with `#cloud-config`.

Use filenames that:

- Are not empty.
- Contain only ASCII letters, digits, `.`, `_`, and `-`.
- Sort in the order in which cloud-init should process them.

For example, `10-site-validation.yaml` is a valid filename. Numeric prefixes
such as `10-`, `20-`, and `30-` make the processing order clear.

PXE validates filenames and file types, but it does not parse or validate the
snippet contents.

## Deploy the Snippets

Place the snippets under:

```text
<PXE static directory>/blobs/internal/cloud-init.d/scout
```

### Use the `nico-pxe` Helm Chart

The shipped `nico-pxe` chart does not expose arbitrary volumes or volume mounts
for the PXE container, so a ConfigMap cannot be mounted at the snippet path
directly through chart values.

With the default `bootArtifacts.servePath` of `/forge-boot-artifacts`, the full
snippet path is:

```text
/forge-boot-artifacts/blobs/internal/cloud-init.d/scout/
```

Use a `bootArtifactContainers` init container to copy snippets from an image
into the shared volume that the PXE container serves. The selected image must
contain the snippets and provide `sh`, `mkdir`, and `cp`; otherwise, use a
shell-capable image that includes them. For example, if the image stores its
snippets in `/snippets`, add this entry to the `nico-pxe` values:

```yaml
bootArtifactContainers:
  - name: scout-cloud-init-snippets
    repository: <your-registry>/scout-cloud-init-snippets
    tag: <tag>
    command:
      - sh
      - -c
      - |
        mkdir -p /forge-boot-artifacts/blobs/internal/cloud-init.d/scout
        cp -r /snippets/. /forge-boot-artifacts/blobs/internal/cloud-init.d/scout/
```

When using the umbrella NICo chart, place this configuration under its
`nico-pxe` key. The chart's shared-volume mount is fixed at
`/forge-boot-artifacts/blobs/internal`, so this mechanism assumes the default
`bootArtifacts.servePath`.

For deployments outside this chart, use any mount or copy mechanism that puts
the snippets under `<PXE static directory>/blobs/internal/cloud-init.d/scout`.
A patched Deployment can, for example, mount a ConfigMap at that path.

## Configure Merging for Multiple Snippets

Each cloud-config snippet is a separate cloud-config document. When cloud-init
merges these documents, it replaces lists rather than appending them by
default. If multiple cloud-config snippets define a shared list-valued key such
as `runcmd`, `packages`, or `write_files`, a later snippet can replace the
earlier value without causing the boot to fail.

To make list-valued configuration additive, add this merge policy to each
participating snippet:

```yaml
#cloud-config
merge_how: 'dict(recurse_array,no_replace)+list(append)'
```

A single cloud-config snippet does not need a custom merge policy. Cloud-config
snippets that use different top-level keys do not conflict. Other supported
user-data formats, such as scripts, do not use cloud-config merge policies.

## Verify the Served Configuration

Boot a host into Scout, then fetch `user-data` from that host. Run the request
from Scout because PXE resolves the datasource caller from its connection IP.
Set `PXE_URL` to the PXE base URL used by the host:

```bash
PXE_URL='<PXE URL>'
wget -qO- "${PXE_URL%/}/api/v0/cloud-init/scout/user-data"
```

When snippets are configured, the response begins with `#include` and lists
one URL for each servable snippet in filename order. Fetch any listed URL with
`wget -qO-` to inspect the original snippet.

When no snippets are configured, the response is a deliberate no-op document:

```yaml
#cloud-config
{}
```

After cloud-init completes, check its status and output on the Scout host:

```bash
cloud-init status --long
journalctl -u cloud-final.service
```

## Reference

### Paths and Endpoints

PXE exposes the NoCloud datasource URL used by Scout through the internal iPXE
variable `${scout-cloudinit-url}`. The Scout boot instruction passes it on the
kernel command line as `ds=nocloud;s=${scout-cloudinit-url}`.

PXE selects responses for these datasource endpoints using the request's source
IP. It does not authenticate the Scout process or verify that Scout originated
the request. Access therefore depends on network reachability and source-IP
resolution rather than caller identity.

| Purpose | Path or endpoint |
| --- | --- |
| Datasource | `/api/v0/cloud-init/scout/` |
| User-data | `/api/v0/cloud-init/scout/user-data` |
| Meta-data | `/api/v0/cloud-init/scout/meta-data` |
| Snippet directory | `<PXE static directory>/blobs/internal/cloud-init.d/scout` |
| Served snippets | `/public/blobs/internal/cloud-init.d/scout/` |

The datasource provides `user-data` and `meta-data`. It does not provide
`vendor-data`.

PXE renders `user-data` as a cloud-init `#include` document containing the
snippet URLs. Cloud-init fetches and processes each included document in
filename order.

### File Discovery Rules

PXE applies these rules when scanning the snippet directory:

- Dotfiles are skipped. This includes ConfigMap mount internals such as
  `..data`.
- Subdirectories are skipped.
- Regular files are served.
- Symbolic links that resolve to regular files are served, which supports
  ConfigMap-mounted snippets.
- Invalid or non-UTF-8 names are skipped with a warning.
- Directory entries that cannot be read or inspected are skipped with a
  warning.

### Empty or Unreadable Directory Behavior

If the directory is missing, empty, or contains no servable files, PXE returns
the no-op cloud-config document shown in the verification procedure. This is
the supported default for an unconfigured site.

If the directory exists but PXE cannot list it, PXE still returns the no-op
document. This prevents an operator-side permissions problem from making the
Scout datasource unavailable.

PXE also emits the `pxe_snippet_directory_unreadable` event and increments:

```text
carbide_pxe_boot_outcomes_total{endpoint="cloud_init_scout",reason="snippet_directory_unreadable"}
```

Use this signal to distinguish an unconfigured site from a configured directory
that PXE cannot read.

### Scout Wait and Timeout Behavior

Scout runs in service mode on the discovery OS; it is the default `--mode`.
Before registering the machine and proceeding with discovery, Scout runs
`cloud-init status --wait --long`. In a normal cloud-init run, every snippet has
finished, either successfully or with errors, before Scout proceeds.

Scout waits for at most 600 seconds. If the status command has not returned by
then, Scout stops waiting and continues with registration. Timing out the
status command does not stop `cloud-final.service`, so a long-running snippet
can still be running when Scout proceeds after this backstop.

Standalone mode (`forge-scout --mode=standalone`, used by the BlueField BFB
install flow) registers without this wait. The standalone BlueField flow does
not use the Scout cloud-init datasource, so the snippets documented on this
page do not apply to it.

Independently, cloud-init processes configuration in systemd-managed stages.
Final-stage modules, including `runcmd`, run under `cloud-final.service` and are
subject to its start timeout. A stage that exceeds this timeout is terminated,
and unfinished work is not retried later in the same boot. Check the value in
the running Scout image instead of assuming a fixed duration:

```bash
systemctl show -p TimeoutStartUSec cloud-final.service
```

### Metadata Precedence

PXE resolves `instance-id` in this order:

1. The `instance-id` from the resolved cloud-init metadata, when present
   (normally the machine ID).
1. The resolved machine interface ID.
1. The fixed fallback value `nico-discovery`.

PXE sets `local-hostname` only when the resolved machine interface has a
non-empty hostname. Otherwise, the field is omitted so cloud-init can apply its
own hostname behavior.

For the complete host discovery workflow, refer to [Ingesting
Hosts](ingesting-hosts.md).
