# Discovery OS cloud-init (Scout) <Badge intent="info">v2.2</Badge>

Scout is the NVIDIA Infra Controller (NICo) discovery OS. PXE exposes Scout's
NoCloud datasource URL through the internal iPXE variable
`${scout-cloudinit-url}`. The Scout boot instruction passes it on the kernel
command line as `ds=nocloud;s=${scout-cloudinit-url}`. The datasource endpoint
is `/api/v0/cloud-init/scout/`. The PXE service provides `user-data` and
`meta-data` at this prefix. It does not provide `vendor-data`.

Most sites do not need to customize Scout. The datasource is always available,
but configuring site-specific content is optional. When no content is
configured, PXE serves a valid no-op cloud-config document.

## Configure Scout snippets

Place site-specific cloud-config snippets in
`<PXE static directory>/blobs/internal/cloud-init.d/scout`.

PXE lists the servable files in filename order and renders `user-data` as a
cloud-init `#include` document containing their URLs. Cloud-init fetches and
processes each included document in that order. The snippets themselves are
served from `/public/blobs/internal/cloud-init.d/scout/`, which is
unauthenticated.

Each included file must use a format that cloud-init recognizes. In particular,
a cloud-config snippet must begin with `#cloud-config`.

PXE validates filenames and file types, but it does not parse or validate the
snippet contents.

### Populate snippets with the `nico-pxe` Helm chart

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

### Filename and entry rules

A snippet filename must be non-empty and contain only ASCII letters, digits,
`.`, `_`, and `-`. For example, `10-site-validation.yaml` is valid.

PXE applies these rules when scanning the directory:

- Dotfiles are skipped. This includes ConfigMap mount internals such as
  `..data`.
- Subdirectories are skipped.
- Regular files are served.
- Symbolic links that resolve to regular files are served, which supports
  ConfigMap-mounted snippets.
- Invalid or non-UTF-8 names are skipped with a warning.
- Directory entries that cannot be read or inspected are skipped with a
  warning.

If the directory is missing, empty, or contains no servable files, PXE returns
a deliberate no-op document:

```yaml
#cloud-config
{}
```

This is the supported default for an unconfigured site.

### Unreadable snippet directory

If the snippet directory exists but PXE cannot list it, PXE still returns the
no-op document. This prevents an operator-side permissions problem from making
the Scout datasource unavailable.

PXE also emits the `pxe_snippet_directory_unreadable` event and increments:

```text
carbide_pxe_boot_outcomes_total{endpoint="cloud_init_scout",reason="snippet_directory_unreadable"}
```

Use this signal to distinguish an unconfigured site from a configured directory
that PXE cannot read.

## Compose multiple snippets safely

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

## Understand Scout's cloud-init wait

Before registering the machine and proceeding with discovery, Scout runs
`cloud-init status --wait --long`. In a normal cloud-init run, every snippet has
finished, either successfully or with errors, before Scout proceeds.

Scout waits for at most 600 seconds. If the status command has not returned by
then, Scout stops waiting and continues with registration. Timing out the
status command does not stop `cloud-final.service`, so a long-running snippet
can still be running when Scout proceeds after this backstop.

Independently, cloud-init processes configuration in systemd-managed stages.
Final-stage modules, including `runcmd`, run under `cloud-final.service` and are
subject to its start timeout. A stage that exceeds this timeout is terminated,
and unfinished work is not retried later in the same boot. Check the value in
the running Scout image instead of assuming a fixed duration:

```bash
systemctl show -p TimeoutStartUSec cloud-final.service
```

## Never reboot from a Scout snippet

Never reboot a host from a Scout snippet.

Scout runs from a RAMdisk. Rebooting causes a full re-PXE, discards the current
root filesystem, and runs the same snippets again. A snippet that reboots can
therefore create an endless discovery boot loop.

Changes that require a reboot, such as kernel parameters, module changes, or
firmware activation, belong in the Scout image or in a later lifecycle state
that owns the reboot.

## Make snippets idempotent

Snippets run on every discovery boot against a clean root filesystem. Any
effect outside that filesystem must be safe to repeat. Examples include calling
an API, modifying BMC or firmware state, writing to persistent storage, or
consuming a license seat.

## Do not put secrets in snippets

Snippet files are served through `/public` without authentication. Anything in
a snippet is readable by any client that can reach the PXE service. Do not
embed passwords, tokens, private keys, or other secrets. If privileged material
is required, retrieve it at runtime from an authenticated source.

## Understand Scout metadata

PXE resolves `instance-id` in this order:

1. The `instance-id` from the resolved cloud-init metadata, when present
   (normally the machine ID).
1. The resolved machine interface ID.
1. The fixed fallback value `nico-discovery`.

PXE sets `local-hostname` only when the resolved machine interface has a
non-empty hostname. Otherwise, the field is omitted so cloud-init can apply its
own hostname behavior.

For the complete host discovery workflow, see [Ingesting
Hosts](ingesting-hosts.md).
