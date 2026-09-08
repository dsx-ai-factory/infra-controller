# Building NICo Containers

This section provides instructions for building the containers for NVIDIA Infra Controller (NICo).

## Installing Prerequisite Software

An Ubuntu 24.04 host or VM with at least 150GB of free disk space is required, and git and make must also be installed (macOS is not supported).

Clone the repo and run the build-host bootstrap. It installs everything needed to build
the containers and boot artifacts -- system packages, rustup, the mkosi/ipxe git
submodules, Docker with cross-architecture emulation, and the cargo build tooling --
in one idempotent step:

```sh
git clone git@github.com:dsx-ai-factory/infra-controller.git
cd infra-controller
make bootstrap          # or: ./scripts/setup-build-host.sh
```

Reboot (or log out and back in) afterwards so the `docker` group membership and the
userns sysctl change take effect.

### Manual setup (what `make bootstrap` does)

`make bootstrap` runs `scripts/setup-build-host.sh`, which is equivalent to the following
steps on an `apt`-based distribution such as Ubuntu 24.04:

1. `apt-get install build-essential cpio direnv mkosi uidmap curl file fakeroot git docker.io docker-buildx sccache protobuf-compiler libopenipmi-dev libudev-dev libboost-dev libgrpc-dev libprotobuf-dev libssl-dev libtss2-dev kea-dev systemd-boot systemd-ukify jq zip`
2. [Add the correct hook for your shell](https://direnv.net/docs/hook.html)
3. Install rustup: `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh` (select Option 1)
4. Start a new shell to pick up changes made from direnv and rustup.
5. Clone NICo - `git clone git@github.com:dsx-ai-factory/infra-controller.git infra-controller`
6. `cd infra-controller`
7. `direnv allow`
8. `git submodule update --init --recursive`
9. Start Docker and register cross-architecture support:
   `sudo systemctl enable --now docker.socket`, then
   `sudo docker run --privileged --rm tonistiigi/binfmt@sha256:400a4873b838d1b89194d982c45e5fb3cda4593fbfd7e08a02e76b03b21166f0 --install all`
10. `cargo install cargo-make cargo-cache`
11. `echo "kernel.apparmor_restrict_unprivileged_userns=0" | sudo tee /etc/sysctl.d/99-userns.conf`
12. `sudo usermod -aG docker $(id -un)`
13. `reboot`

## Build all images with one command

Once the prerequisites above are installed, build the NICo container images from the
top of the repo with a single `make` command:

```sh
make images          # deployable stack: NICo Core (nico) + the REST service images
make images-all      # the above plus machine-validation and both boot-artifact images
```

Images are pushed as `linux/amd64` and `linux/arm64` manifests at
`localhost:5000/<name>:latest` by default. The Makefile starts a local registry named
`nico-build-registry` when that default is used. Override the registry and tag to publish
under your own registry; authenticate Docker to that registry before running the build:

```sh
make images IMAGE_REGISTRY=my-registry.example.com/nico IMAGE_TAG=v1.0.0
```

By default, every image group is built for both `amd64` and `arm64`. Each group has its
own override variable if you only need one architecture:

- `NICO_ARCHES` — the NICo control-plane images (`images-base`, `images-core`,
  `images-rest`, `images-machine-validation`)
- `BOOT_ARTIFACTS_ARCHES` — the x86 boot-artifact image (`images-boot-artifacts`)
- `DPU_ARCHES` — the DPU BFB boot-artifact image (`images-bfb`)

```sh
make images-all NICO_ARCHES=amd64 DPU_ARCHES=arm64
```

A single-architecture build still produces a valid tag at `$(IMAGE_TAG)` (the multi-arch
manifest just has one entry). Values other than `amd64`/`arm64` fail fast with an error.

`images-machine-validation` additionally requires `NICO_ARCHES` to include `amd64`:
the `machine-validation-runner` intermediate image it embeds is always built for
`amd64` regardless of which architectures you're publishing, and it depends on the
amd64 Core runtime base container that `images-base` only pushes when `amd64` is
requested. `NICO_ARCHES=arm64` alone fails fast with an error; use
`NICO_ARCHES="amd64 arm64"` (the default) or `NICO_ARCHES=amd64`.

Each architecture is built separately before the bare tag is assembled. This matches CI
and is required for the REST Dockerfiles: a single combined Buildx invocation would reuse
one builder stage and could copy an amd64 binary into the arm64 image. Building the
non-native architecture uses the platforms configured on the active Docker Buildx builder.

Run `make help` from the repo root to list the individual image targets (`images-core`,
`images-rest`, `images-machine-validation`, `images-boot-artifacts`, `images-bfb`). The
sections below document the per-image build commands that these targets wrap, for when you
need to build or debug a single image.

### Verifying the build

After `make images-all` completes, verify that each of the 14 deployable image tags
contains the platforms you actually built. Set `NICO_ARCHES`, `BOOT_ARTIFACTS_ARCHES`,
and `DPU_ARCHES` below to whatever you passed to `make` (they default to `amd64 arm64`,
matching the Makefile); the script derives each image's expected platform list from its
group automatically, so a narrowed selection doesn't produce false failures.

```bash
images=(
  nico nico-rest-api nico-rest-workflow nico-rest-site-manager
  nico-rest-site-agent nico-rest-db nico-rest-cert-manager nico-flow
  nico-psm nico-nsm nico-mcp machine-validation
  boot-artifacts-x86_64 boot-artifacts-aarch64
)

# Mirrors the Makefile defaults; set these to whatever you passed to `make`.
NICO_ARCHES="${NICO_ARCHES:-amd64 arm64}"
BOOT_ARTIFACTS_ARCHES="${BOOT_ARTIFACTS_ARCHES:-amd64 arm64}"
DPU_ARCHES="${DPU_ARCHES:-amd64 arm64}"

expected_platforms_for() {
  local arches
  case "$1" in
    boot-artifacts-x86_64) arches="${BOOT_ARTIFACTS_ARCHES}" ;;
    boot-artifacts-aarch64) arches="${DPU_ARCHES}" ;;
    *) arches="${NICO_ARCHES}" ;;
  esac
  echo "${arches}" | tr ' ' '\n' | sed 's#^#linux/#' | sort | paste -sd, -
}

for image in "${images[@]}"; do
  expected="$(expected_platforms_for "${image}")"
  platforms="$(docker buildx imagetools inspect --raw \
    "${IMAGE_REGISTRY}/${image}:${IMAGE_TAG}" | \
    jq -r '[.manifests[].platform | select(.os == "linux") | "\(.os)/\(.architecture)"] | unique | sort | join(",")')"
  if [ "${platforms}" != "${expected}" ]; then
    printf 'FAIL %s: got %s, want %s\n' "${image}" "${platforms}" "${expected}" >&2
    exit 1
  fi
  printf 'PASS %s: %s\n' "${image}" "${platforms}"
done
```

The loop should print exactly 14 successful checks:

| Image | Target |
|---|---|
| `nico` | `images-core` |
| `nico-rest-api` | `images-rest` |
| `nico-rest-workflow` | `images-rest` |
| `nico-rest-site-manager` | `images-rest` |
| `nico-rest-site-agent` | `images-rest` |
| `nico-rest-db` | `images-rest` |
| `nico-rest-cert-manager` | `images-rest` |
| `nico-flow` | `images-rest` |
| `nico-psm` | `images-rest` |
| `nico-nsm` | `images-rest` |
| `nico-mcp` | `images-rest` |
| `machine-validation` | `images-machine-validation` |
| `boot-artifacts-x86_64` | `images-boot-artifacts` |
| `boot-artifacts-aarch64` | `images-bfb` |

If the loop exits early, the `FAIL` line identifies which image has an incomplete
manifest. The three boot/validation images (`machine-validation`,
`boot-artifacts-x86_64`, `boot-artifacts-aarch64`) require the full mkosi + Rust
toolchain. Use `make images` instead of `make images-all` to build only the 11-image
deployable stack.

The architecture-specific Core base images and `-amd64`/`-arm64` service tags are build
inputs for the bare multi-arch tags. `machine-validation-runner` is the only local-only
intermediate image.

## Building X86_64 Containers

**NOTE**: Execute these tasks in order. All commands are run from the top of the `infra-controller` directory.

### Building the X86 build container

```sh
docker build --file dev/docker/Dockerfile.build-container-x86_64 -t nico-buildcontainer-x86_64 .
```

### Building the X86 runtime container

```sh
docker build --file dev/docker/Dockerfile.runtime-container-x86_64 -t nico-runtime-container-x86_64 .
```

### Building the boot artifact containers

```sh
cargo make --cwd pxe --env SA_ENABLEMENT=1 build-boot-artifacts-x86-host-sa
docker build --build-arg "CONTAINER_RUNTIME_X86_64=alpine:latest" -t boot-artifacts-x86_64 -f dev/docker/Dockerfile.release-artifacts-x86_64 .
```

## Building the Machine Validation images

```sh
docker build --build-arg CONTAINER_RUNTIME_X86_64=nico-runtime-container-x86_64 -t machine-validation-runner -f dev/docker/Dockerfile.machine-validation-runner .

docker save --output crates/machine-validation/images/machine-validation-runner.tar machine-validation-runner:latest 

// This copies `machine-validation-runner.tar` into the `/images` directory on the `machine-validation-config` container.  When using a kubernetes deployment model
// this is the only `machine-validation` container you need to configure on the `nico-pxe` pod.

docker build --build-arg CONTAINER_RUNTIME_X86_64=nico-runtime-container-x86_64 -t machine-validation-config -f dev/docker/Dockerfile.machine-validation-config .

```

## Building nico-core container

```sh
docker build --build-arg "CONTAINER_RUNTIME_X86_64=nico-runtime-container-x86_64" --build-arg "CONTAINER_BUILD_X86_64=nico-buildcontainer-x86_64" -f dev/docker/Dockerfile.release-container-sa-x86_64 -t nico .
```

## Building the AARCH64 Containers and artifacts

### Building the Cross-compile container

```sh
docker build --file dev/docker/Dockerfile.build-artifacts-container-cross-aarch64 -t build-artifacts-container-cross-aarch64 .
```

## Building the admin-cli

The `admin-cli` build does not produce a container. It produces a binary:

`$REPO_ROOT/target/release/nico-admin-cli`

```text
BUILD_CONTAINER_X86_URL="nico-buildcontainer-x86_64" cargo make build-cli
```

### Building the DPU BFB

```sh
cargo make --cwd pxe --env SA_ENABLEMENT=1 build-boot-artifacts-bfb-sa

docker build --build-arg "CONTAINER_RUNTIME_AARCH64=alpine:latest" -t boot-artifacts-aarch64 -f dev/docker/Dockerfile.release-artifacts-aarch64 .
```

**NOTE**: The `CONTAINER_RUNTIME_AARCH64=alpine:latest` build argument must be included. The aarch64 binaries are bundled into an x86 container.
