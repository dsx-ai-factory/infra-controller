# Generating bootable artifacts

### 1. Install build tools

Install 'mkosi' and 'debootstrap' from the repository -- for Debian it was

```
sudo apt install mkosi debootstrap
```

### 2. Select the Bootstrap CA Bundle

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

### 3. Build iPXE Image

Run

```
cd $NICo_ROOT_DIR/pxe && cargo make build-boot-artifacts-x86_64
```

Because you cannot build `aarch64` artifacts on an `x86_64` host, we only create the necessary directories to satisfy the `docker-compose` workflow:

```
cd $NICo_ROOT_DIR/pxe && cargo make mkdir-static-aarch64
```



> **NOTE**: Running NICo using `docker-compose` and QEMU `clients` only works with `x86_64` binaries. CI/CD is used for testing on `aarch64` systems such as a BlueField


or

download pre-built artifacts - ideal if the `ipxe-x86_64` gives you
errors. Extract the latest boot artifacts (available from your NICo distribution package)
into `$NICo_ROOT_DIR/pxe/static/blobs/internal/x86_64/` (you'll need
to create the hierarchy).

`build-boot-artifacts-x86_64` will also rebuild binaries we package as part of the boot artifacts (like `nico-scout`), while
the latter command will only package already existing artifacts.
Therefore prefer the former if you change applications.

**Note:** the last step will exit uncleanly because it wants to compress for CI/CD and upload, but it's not necessary locally. It's fine as long as the contents of this directory look similar to:

```
$ exa -alh pxe/static/blobs/internal/x86_64/
Permissions Size User      Date Modified Name
.rw-rw-r--    44 $USER     18 Aug 15:35  .gitignore
drwxr-xr-x     - $USER     24 Aug 09:59  .mkosi-t40tggmu
.rw-r--r--   55M $USER     24 Aug 10:01  nico.efi
.rw-r--r--   26k $USER     24 Aug 10:01  nico.manifest
.rw-r--r--  298M $USER     24 Aug 10:01  NICo.root
.rw-rw-r--  1.1M $USER     24 Aug 10:05  ipxe.efi
.rw-rw-r--  402k $USER     24 Aug 10:03  ipxe.kpxe
```

**Note:** you'll also need to `chown` the directory recursively back to
your user because mkosi will only run as root; otherwise, your next
docker-compose build won't have the permissions it needs:

```
sudo chown -R `whoami` pxe/static/*
```
