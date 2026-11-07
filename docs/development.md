# Development

NVIDIA Infra Controller (NICo) uses docker-compose to instantiate a development
environment.

## Local environment prep

1. Install rust by following the directions [here](https://www.rust-lang.org/tools/install).
   You will need to use the rustup based installation method to use the same Rust compiler utilized by the CI toolchain.
   You can find the target compiler version in `rust-toolchain.toml` in the root of this directory
   If rustup is installed, you can switch toolchain versions using `rustup toolchain`.

   Make sure you have a C++ compiler:

   Arch - `sudo pacman -S base-devel`

   Debian - `sudo apt-get -y install build-essential libudev-dev libssl-dev binutils-aarch64-linux-gnu pkg-config`

   Fedora - `sudo dnf -y install gcc-c++ systemd-devel binutils-aarch64-linux-gnu`
    - systemd-devel is needed for libudev-devel
    - binutils-aarch64-linux-gnu is for stripping the cross-compiled nico-dpu-agent - don't worry if you don't have this

2. Install additional cargo utilities

   `RUSTC_WRAPPER= cargo install cargo-watch cargo-make sccache mdbook@0.4.52 mdbook-plantuml@0.8.0 mdbook-mermaid@0.16.2`

3. Install docker following these [directions](https://docs.docker.com/engine/install/ubuntu/#install-using-the-repository), then add yourself to the docker group: `sudo usermod -aG docker $USER` (otherwise, you must always `sudo` docker`).
4. Install docker-compose using your system package manager

   Arch - `sudo pacman -S docker-compose`

   Debian - `sudo apt-get install -y docker-compose`

   Fedora - `sudo dnf install -y docker-ce docker-ce-cli containerd.io docker-compose-plugin docker-compose`

5. Install ISC kea using your system package manager

   Arch - `sudo pacman -S kea`

   Debian/Ubuntu
    - Install required libraries
        - `sudo apt-get install -y libboost-dev`
        - download libssl1 from [here](http://archive.ubuntu.com/ubuntu/pool/main/o/openssl/) and install `sudo dpkg -i <downloaded-lib>`.  `libssl1.1_1.1.0g-2ubuntu4_amd64.deb` is known to work but there are newer versions that haven't been tested

   - Install kea, but might be out of date:
     ```
     sudo apt-get update && sudo apt-get install -y isc-kea-dhcp4-server isc-kea-dev
     ```
   - Or, but has only been tested with Ubuntu 23.10, install kea:
     ```
     sudo apt-get update && sudo apt-get install -y kea-dev kea-dhcp4-server
     ```

   Fedora - `sudo dnf install -y kea kea-devel kea-libs`

6. You can install PostgreSQL locally, but it might be easier to start a
   docker container when you need to. The docker container is handy when running `cargo test` manually.
   `docker run -e POSTGRES_PASSWORD="admin" -p "5432:5432" postgres:14.1-alpine`

   a. Postgresql CLI utilities should be installed locally

   Arch - `sudo pacman -S postgresql-client`

   Debian - `sudo apt-get install -y postgresql-client`

   Fedora - `sudo dnf install -y postgresql`

7. Install qemu and ovmf firmware for starting VMs to simulate PXE clients

   Arch - `sudo pacman -S qemu edk2-omvf`

   Debian - `sudo apt-get install -y qemu qemu-kvm ovmf`

   Fedora - `sudo dnf -y install bridge-utils libvirt virt-install qemu-kvm`

8. Install `direnv` using your package manager

   It would be best to install `direnv` on your host. `direnv` requires a shell hook to work.  See `man direnv` (after install) for
   more information on setting it up. Once you clone the `infra-controller` repo, you need to run `direnv allow` the first time you cd into your local copy.
   Running `direnv allow` exports the necessary environmental variables while in the repo and cleans up when not in the repo.

   There are preset environment variables that are used throughout the repo. `${REPO_ROOT}` represents the top of the repo.

   For a list of preset environment variables, look in:
   `${REPO_ROOT}/.envrc`

   Arch - `sudo pacman -S direnv`

   Debian - `sudo apt-get install -y direnv`

   Fedora - `sudo dnf install -y direnv`

9. Install golang using whatever method is most convenient for you. `nico-vpc` (which is in a subtree of the `nico-provisioner` repo uses golang)

10. Install GRPC client `grpcurl`.

    Arch - `sudo pacman -S grpcurl`

    Debian/Ubuntu/Others - [Get latest release from github](https://github.com/fullstorydev/grpcurl/releases)

    Fedora - `sudo dnf install grpcurl`

11. Additionally, `prost-build` needs access to the protobuf compiler to parse proto files (it doesn't implement its own parser).

    Arch - `sudo pacman -S protobuf`

    Debian - `sudo apt-get install -y protobuf-compiler`

    Fedora - `sudo dnf install -y protobuf protobuf-devel`

12. Install `jq` from system package manager

    Arch - `sudo pacman -S jq`

    Debian - `sudo apt-get install -y jq`

    Fedora - `sudo dnf install -y jq`

13. Install `mkosi` and `debootstrap` from system package manager

    Debian - `sudo apt-get install -y mkosi debootstrap`

    Fedora - `sudo dnf install -y mkosi debootstrap`

14. Install `liblzma-dev` from system package manager

    Debian - `sudo apt-get install -y liblzma-dev`

    Fedora - `sudo dnf install -y xz-devel`

15. Install `swtpm` and `swtpm-tools` from system package manager

    Debian - `sudo apt-get install -y swtpm swtpm-tools`

    Fedora - `sudo dnf install -y swtpm swtpm-tools`

16. Install `cmake` from the system package manager:

    Debian - `sudo apt-get install -y cmake`

    Fedora - `sudo dnf install -y cmake`

17. Install `vault` for integration testing:

    `curl -Lo vault.zip https://releases.hashicorp.com/vault/1.13.3/vault_1.13.3_linux_amd64.zip && unzip vault.zip && chmod u+x vault && mv vault /usr/local/bin/`

    Or [there are deb/rpm repos here](https://developer.hashicorp.com/vault/tutorials/getting-started/getting-started-install#install-vault).

18. Build the `build-container` locally

    `cargo make build-x86-build-container`

19. Build the book locally

    `cargo make book`

    Then bookmark `file:///$REPO_ROOT/public/index.html`.

## Checking your setup / Running Unit Tests

To quickly set up your environment to run unit tests, you'll need an initialized PSQL service locally on your system. The docker-compose workflow
handles this for you, but if you're trying to set up a simple env to run unit tests run the following.

Start docker daemon:

`sudo systemctl start docker`

Start database container:

`docker run --rm -di -e POSTGRES_PASSWORD="admin" -p "5432:5432" --name pgdev postgres:14.1-alpine`

Test!

`cargo test`

If the tests don't pass ask in Slack #infra-controller.

Cleanup, otherwise docker-compose won't work later:

`docker ps; docker stop <container ID>`

## IDE

Recommended IDE for Rust development in the NICo project is CLion, IntelliJ works as well but includes a lot of extra components that you don't need. There are plenty
of options (VS Code, NeoVim etc), but CLion/IntelliJ is widely used.

One thing to note regardless of what IDE you choose: if you're running on Linux DO NOT USE Snap or Flatpak versions of the software packages. These builds introduce a number
of complications in the C lib linking between the IDE and your system and frankly it's not worth fighting.

## Cross-compiling for aarch64 (rough notes)

The DPU has an ARM core. To build software that runs there such as `nico-dpu-agent` you need an ARM8 machine. QEMU/libvirt can provide that.

Here's how I did it.

One time build:
 - copy / edit the Docker file from the repo into `myarm/Dockerfile`.
 - delete these lines:
```
 RUN /root/.cargo/bin/cargo install cargo-cache cargo-make mdbook@0.4.52 mdbook-plantuml@0.8.0 mdbook-mermaid@0.16.2 sccache && /root/.cargo/bin/cargo cache -r registry-index,registry-sources
 RUN curl -fsSL https://get.docker.com -o get-docker.sh && sh get-docker.sh
 RUN cd /usr/local/bin && curl -fL https://getcli.jfrog.io | sh
```
 - `docker build -t myarm myarm` # give it a cooler name
 - `docker run -it -v /home/user/src/infra-controller:/infra-controller myarm /bin/bash`

Daily usage:
 - `docker start <container id or name>`
 - `docker attach <container id or name>`

Now that you're in the container go into `/infra-controller` and work normally (`cargo build --release`). The binary rust produces will be aarch64. You can `scp` it to a DPU and run it.

The build may hang the first time. I don't know why. Ctrl-C and try again. You may want to `docker commit` after it succeeds to update the image.

Remember to `strip` before you scp so that scp goes faster. scp to DPU example (`nvinit` first): `scp -v /path/to/target/release/nico-dpu-agent ubuntu@<DPU_OOB_IP>:.`

## Next steps

Set up a QEMU host for your docker-compose services to manage:

1. [Build iPXE and bootable artifacts image](bootable_artifacts.md)
1. [Start QEMU server](development/vm_pxe_client.md)
