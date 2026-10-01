## Visual Studio Code Remote Docker Workflow

This page describes a workflow on how to build and test NVIDIA Infra Controller (NICo) inside a remotely
running docker container. The advantage of this workflow is that it requires no tools
to be installed on your native Machine, but still can provide you a similar
development feeling.

### Prerequisites

- Install Visual Studio Code from [https://code.visualstudio.com](https://code.visualstudio.com)
- Install the [Remote Development Extension Pack](https://marketplace.visualstudio.com/items?itemName=ms-vscode-remote.vscode-remote-extensionpack)
- Enable the `code` command for MacBook:
    - Open VS Code
    - Press `Cmd + Shift + P` to open the Command Palette.
    - Type `Shell Command: Install 'code' command in PATH` and select it. This sets up the code command for your terminal.
- On the remote server, update the SSH daemon configuration to support port forwarding:
    - Edit the sshd configuration file:
        ```
        doas vi /etc/ssh/sshd_config
        ```
    - Add or update the following lines:
        ```
        AllowTcpForwarding yes
        GatewayPorts yes
        ```
    - Restart sshd daemon:
        ```
        doas systemctl restart sshd
        ```
    - For MacBook:
      - Port forwarding may fail initially.
      - To resolve this issue, remove the `~/.ssh/known_hosts` file.
    *Source*: [Stack Overflow](https://stackoverflow.com/questions/75837749/vscode-failed-to-set-up-socket-for-dynamic-port-forward-to-remote-port-connect)
      - **Note**: Be sure to back up the file before deleting it.

### Basic remote setup

Start `VS Code` using the `code` command in the same shell after running `nvinit`:

Click the remote button on the lower left of the IDE window:
![](../static/remote_button.png).
Select "Connect to Host", choose the remote hostname defined in [Prerequisites](#Prerequisites), and connect. A new Visual Studio Code window should open, which is now on that host.
Inside that window, open the folder which contains the NICo project.

Assuming that remote machine already has all dev tools installed, and you want to
work directly on the machine instead of inside a container, you could open up
Visual Studio Code's integrated terminal, and for example run:
```
cd api
cargo test
```

### Remote Rust Analyzer support

In order to get proper IDE support also while working on the remote host,
you can install the "Rust Analyzer" extension on the remote host. To do so:
- Open the extensions tab
- Look for the second column in it, which is labeled: `SSH: $hostname - Installed`.
- Click the download button next to it.
- Select Rust Analyzer, and all other extensions you want to install on the remote Host.
  Other recommended extensions are CodeLLDB for debugging Rust code, Better TOML
  for editing `.toml` files, and GitLens.

### Remote container setup

On top of developing on a remote host, one can develop inside a container that
contains all dev tools. The container can either run locally (if you work on a Linux machine),
or on a remote Linux machine.

To work inside the remote container, the following steps are performed:

1. Build the repository's pinned development image from the NICo directory:

    ```bash
    cargo make build-cargo-docker-image
    ```

1. Inside the NICo directory on the Linux host, create `.devcontainer/devcontainer.json` with the following contents:

    ```json
    // For format details, see https://aka.ms/devcontainer.json.
    {
        "name": "NICo development",
        "image": "carbide-build-x86_64",

        // Use 'forwardPorts' to make a list of ports inside the container available locally.
        // "forwardPorts": [],

        // Uncomment the next line to run commands after the container is created - for example installing curl.
        // "postCreateCommand": "apt-get update && apt-get install -y curl",

        // Uncomment when using a ptrace-based debugger like C++, Go, and Rust
        "runArgs": [ "--cap-add=SYS_PTRACE", "--security-opt", "seccomp=unconfined" ],

        // Uncomment to use the Docker CLI from inside the container. See https://aka.ms/vscode-remote/samples/docker-from-docker.
        "mounts": [ "source=/var/run/docker.sock,target=/var/run/docker.sock,type=bind" ]

        // Uncomment to connect as a non-root user if you've added one. See https://aka.ms/vscode-remote/containers/non-root.
        //"remoteUser": "youralias"
    }
    ```

    This configuration instructs the remote container extension to use the repository's development image, which contains the required tools.

1. Click the remote button in the lower-left corner of the IDE window.

    ![Visual Studio Code remote connection button](../static/remote_button.png)

    Select **Reopen in Container**. Visual Studio Code uses the specified image, launches the container, installs the remote server, and opens the editor in the container.

1. Confirm that the lower-left corner shows **Dev Container: NICo development**.

1. Open an integrated terminal and build the project.

1. Reinstall the extensions you need in the development container, such as Rust Analyzer.

### Enabling postgres inside the dev container

While the last step will allow you to build the project and run some unit tests,
all unit tests which require a database will fail. To fix this, start the postgres
server inside the development container:

1. Open another internal terminal tab
2. Start postgres:
    ```
    /etc/init.d/postgresql start
    ```
3. Create the user:
    ```
    su postgres -c "/usr/lib/postgresql/15/bin/createuser -d root"
    ```
4. Set permissions:
    ```
    sudo -u postgres psql -c "ALTER USER root WITH SUPERUSER;"
    ```
5. Create a database:
    ```
    createdb root
    ```
6. Set the `DATABASE_URL` environment variable:
    ```
    export DATABASE_URL="postgresql://%2Fvar%2Frun%2Fpostgresql"
    ```

With those steps completed, running `cargo test` should succeed.

If you also want to run or debug unit-test from within Visual Studio code
using the inline buttons "**Run Test**" and "**Debug**" that Rust-Analyzer
adds, you have to add the following configuration to the Visual Studio Code
json config file:
```json
"rust-analyzer.runnableEnv": {
    "DATABASE_URL": "postgresql://%2Fvar%2Frun%2Fpostgresql"
}
```

### Gotchas

- If you work as `root` inside the dev container, editing files might make them
  owned by `root`, which can prevent working on them from your regular desktop.
  You might need to reset ownership when going back to your regular environment:
  ```
  sudo chown -R yourAlias nico/*
  ```
- The same applies for using git inside the container as root. It will make
  files in `.git` be owned by `root`

Those problems might be avoidable by being able to set `remoteUser` in `devcontainer.json`
to one's alias. However when doing that I wasn't able to build the devcontainer image
anymore, since it is missing my user alias in `/etc/passwd`.


### References

- [https://code.visualstudio.com/docs/remote/remote-overview](https://code.visualstudio.com/docs/remote/remote-overview)
