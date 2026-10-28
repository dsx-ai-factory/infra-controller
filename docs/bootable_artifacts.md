# Generating Bootable Artifacts

To rebuild only the iPXE EFI files, go directly to [Build iPXE Image](#3-build-ipxe-image).
For complete boot-artifact images, including Scout, follow
[Building the Boot Artifact Containers](manuals/building_nico_containers.md#building-the-boot-artifact-containers).
The Bootstrap CA selection below applies to BFB builds.

## 1. Install Build Tools

For complete image builds, follow the dependency and submodule setup in
[Building NICo Containers](manuals/building_nico_containers.md).
The image tasks use the repository-pinned `pxe/mkosi/bin/mkosi`.
For iPXE-only builds, use the prerequisites below.

## 2. Select the Bootstrap CA Bundle

Site-specific BlueField bootstream (BFB) builds accept an operator-supplied
certificate authority (CA) bundle in Privacy-Enhanced Mail (PEM) format through
`BOOTSTRAP_CA_PATH`:

```bash
export BOOTSTRAP_CA_PATH=/absolute/path/to/site-bootstrap-roots.pem
```

Build the production BFB from a supported environment with the existing
aarch64 cross-build, BFB tooling, registry, and network prerequisites:

```bash
cargo make --cwd pxe build-boot-artifacts-bfb
```

`embedded` mode is available only when this variable is supplied explicitly.
There is no fallback to `FORGE_CA_PATH`, a repository certificate, or a
developer certificate. When it is absent, the dedicated embedded payload is
removed, while existing legacy artifact inputs remain unchanged. Selecting
`embedded` with such an artifact fails closed instead of downloading a CA.

The artifact stores this build-time input at the dedicated embedded source
`/opt/forge/embedded_forge_root.pem`. At DPU boot, embedded mode copies it into
the final `/opt/forge/forge_root.pem` location. This source is intentionally
distinct from `mounted` mode, which expects the provisioning environment to
populate the final path directly, so one mode cannot silently use material
intended for the other.

The `[dpu_config].bootstrap_ca_source` policy is sent only on DPU provisioning
paths. Host Scout boots do not consume it. Use the same bundle for every BFB
variant deployed at a site.

For a root rotation, first build a bundle containing both the old and new
roots, publish and deploy those artifacts, and reprovision every DPU. Verify
that every DPU installed the overlap bundle at `/opt/forge/forge_root.pem` and
can authenticate the NICo API. Rotate the API server chain to the new root and
verify authentication again while the overlap bundle is installed. Only then
publish artifacts without the old root, reprovision and verify the fleet again,
and retire the old root and artifacts. A non-DPF `mounted` deployment instead
expects the provisioning environment to place the operator-managed bundle at
`/opt/forge/forge_root.pem`. NICo does not create that mount. Apply the same
fleet-wide installation and authentication gates when rotating a mounted
bundle.

Upgrade the NICo control plane and publish compatible artifacts before enabling
`embedded` or `mounted`. Older artifacts support only the legacy download.
Verify that the NICo API serves the intermediate certificate with its leaf when
the artifact pins only the root. The bundle performs TLS server certificate
validation whether the agent uses a client certificate for mutual TLS.

Embedding moves trust into the artifact. Protect artifact publication and
distribution with verified signatures. Enforce Secure Boot or an equivalent
chain of trust before treating the embedded CA as a pinned anchor.

## 3. Build iPXE Image

Build the EFI files for each architecture your site boots. The following tasks build
only iPXE. They do not rebuild Scout, operating system images, or BFBs.

### Prerequisites

Use a Linux build environment with the following tools:

- Rust and Cargo from the repository's `rust-toolchain.toml`, plus `cargo-make`.
- Git, `patch`, GNU Make, Perl, GCC, and GNU binutils. The iPXE linker scripts require
  GNU `ld`. Do not substitute LLVM `lld` or `mold`.
- For `aarch64`, the `aarch64-linux-gnu-` toolchain, including GCC and binutils.
  This toolchain allows an `x86_64` Linux host to build the `aarch64` EFI files.
- For `x86_64`, an `x86_64` build host.

Run the commands below from the NICo repository root with `SA_ENABLEMENT` unset.
A nonempty `SA_ENABLEMENT` selects `/usr/bin/ld.bfd` for `x86_64` and runs the
`aarch64` cross-compiler in the `build-artifacts-container-cross-aarch64` Docker image.

Use a separate clean NICo checkout at the same revision for each target
architecture. The tasks copy configuration headers and apply architecture-specific
patches in place. They do not remove patches from earlier builds.
In each checkout, initialize the upstream iPXE revision recorded by NICo:

```bash
git submodule update --init pxe/ipxe/upstream
```

### Build the EFI Files

In each checkout, create the output directories before running its build task:

```bash
cargo make --cwd pxe mkdir-static
```

In the `x86_64` checkout, run only the following architecture task:

```bash
cargo make --cwd pxe ipxe-x86_64
```

In the separate `aarch64` checkout, run only the following architecture task:

```bash
cargo make --cwd pxe ipxe-aarch64
```

Both tasks embed `pxe/ipxe/local/embed.ipxe` and apply
`pxe/ipxe/local/0004-http-ipv6-source.patch` before compiling. The embedded script
and HTTP source-address support are part of the resulting EFI files, not runtime
configuration downloaded from Core.

### Publish the EFI Files

Each local build produces two files in the architecture's output directory:

| Architecture | Output Directory | Files |
| --- | --- | --- |
| `x86_64` | `pxe/static/blobs/internal/x86_64/` | `ipxe.efi`, `golan.efi` |
| `aarch64` | `pxe/static/blobs/internal/aarch64/` | `ipxe.efi`, `golan.efi` |

The tasks rename upstream `snponly.efi` to `ipxe.efi` when installing the output.
Publish only files from a task that completed successfully.

Publish the architecture-matching `ipxe.efi` at
`/public/blobs/internal/<architecture>/ipxe.efi` on the PXE server that hosts your
boot artifacts. If your site boots `golan.efi`, publish that file in the same
architecture directory. Updating Core does not replace separately served EFI files.
DHCPv4 and DHCPv6 HTTP boot clients use the same `ipxe.efi` for each architecture.
After replacing it, verify both boot paths used by your site.
When packaging boot-artifact images, retain Scout and the other required artifacts.
The iPXE-only tasks do not produce a complete replacement artifact image.
Follow [Building the Boot Artifact Containers](manuals/building_nico_containers.md#building-the-boot-artifact-containers)
for that workflow. If your deployment copies artifacts from those images at pod
startup, select the updated image and roll out new PXE pods to copy the files.

For IPv6 deployment prerequisites and verification, refer to
[Deploy DHCPv6](provisioning/dhcpv6-deployment.md#embedded-ipxe-boot-over-ipv6).
