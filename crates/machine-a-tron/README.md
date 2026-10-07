# Machine-A-Tron

A rust tool that uses the api client to simulate machines in the nico development environment.

The purpose of this tool is similar to the bootstrap scripts in `dev/bin` to build machines in the local-dev
environment. I will generate machine information from the files in the template directory and fill in the dynamic data
as needed (product serial, mac address, etc). this allows it to create multiple managed hosts. I will stay running and
periodically report health network observations.

## Usage

```text
target/debug/machine-a-tron -h
Usage: machine-a-tron [OPTIONS] --relay-address <RELAY_ADDRESS> <NUM_HOSTS> [NICO_API]

Arguments:
  <NUM_HOSTS>    The number of host machines to create
  [NICO_API]  the api url

Options:
      --nico-root-ca-path <NICO_ROOT_CA_PATH>
          Default to NICO_ROOT_CA_PATH environment variable or $HOME/.config/nico_api_cli.json file. [env: NICO_ROOT_CA_PATH=]
      --client-cert-path <CLIENT_CERT_PATH>
          Default to CLIENT_CERT_PATH environment variable or $HOME/.config/nico_api_cli.json file. [env: CLIENT_CERT_PATH=]
      --client-key-path <CLIENT_KEY_PATH>
          Default to CLIENT_KEY_PATH environment variable or $HOME/.config/nico_api_cli.json file. [env: CLIENT_KEY_PATH=]
      --template-dir <TEMPLATE_DIR>
          directory containing template files.
      --relay-address <RELAY_ADDRESS>
          relay address for env.
  -h, --help
          Print help

```

## High Level Code Organization

In order to separate work and hopefully avoid bottlenecks, the code runs different systems in tasks using channels for
communication between them.
The following are broken into tasks:

* dhcp_relay - a service that tries to simulate a dhcp relay working on behalf of a machine. The API sees the request as
  if it was sent from a relay
  and responses accordingly. The API requires DHCP reqeusts come from a relay and I had trouble getting the actual relay
  in the dev environment
  to work correctly. Requests are made through the client object and passed a one-shot channel for the response (
  avoiding a lookup to find the machine for a response).
* machine-a-tron - the application-level task that starts all services and waits for a shutdown signal.
* host_machine - a task for each host that progresses through states, makes API requests, and periodically runs the
  DPU state machines owned by the host.
* bmc - runs a bmc-mock that responds to redfish calls using templates in the configured directory

## How to run against a development instance

If you configure your `mat.toml` to connect to your nico instance, by default it will request IP's via DHCP for each
BMC it is mocking, and it will try to bind to said IP locally. It will even run `ip addr add` to add an alias for that
IP. But the default configuration will also try to listen on port 2000 for each mock. This means that for correct
behavior, your nico instance will need to have the following set:

```toml
[site_explorer]
override_target_port = 2000
# Probably also want to configure these, although it is unrelated to the port issue
enabled = true
create_machines = true
run_interval = "10s"
```

If you're using the `skaffold dev` workflow to configure this, you'll want to
edit `envs/local-dev/site/site-controller/files/generated/nico-api-site-config.toml` to add these lines.

## Deploying with kubernetes in development environment

Machine-a-tron can run as a kubernetes service in your k3s development environment, which can be helpful if you want
your environment to be seeded with mock machines as part of your workflow. To do this, you'll need to configure a custom
DPU network in the `nicod` repo, configuring it to enable machine-a-tron and specifying how many mock hosts/DPUs you
want.

To do this, make a copy of `envs/local-dev/dpu_networks/custom-dpu-config.json` in the `nicod` repo (any path will
work), and populate it.

For example, copying it to `/home/me/dpu-config.json`, you can configure:

```json
{
  "name": "dpu-net-0",
  "ip_prefix": "192.168.200",
  "starting_ip": 5,
  "ending_ip": 99,
  "dpu_bmc_underlay_name": "ACORN-0",
  "dpu_bmc_underlay_prefix": "192.168.201.0/24",
  "dpu_bmc_underlay_gateway": "192.168.201.1",
  "x86_bmc_underlay_name": "ACORN-0",
  "x86_bmc_underlay_prefix": "192.168.202.0/24",
  "x86_bmc_underlay_gateway": "192.168.202.1",
  "admin_network_prefix": "192.168.200.0/24",
  "admin_network_gateway": "192.168.200.1",
  "dpu_loopback_ip_range_start": "10.180.62.1",
  "dpu_loopback_ip_range_end": "10.180.62.62",
  "use_machine_a_tron_mocks": true,
  "mock_host_count": 10,
  "mock_dpu_per_host_count": 1
}
```

This config uses 192.168.200 for the admin net, 192.168.201 for the DPU BMC net, and 192.168.202 for the x86 BMC net.
Most important are the lines near the bottom, which specify that we want to use machine-a-tron to mock the DPUs.

Next, run `CUSTOM_DPU_PATH=/home/me/dpu-config.json just setup-k3s-env-ips` in the nicod repo, which will generate a
nico site config *and* a machine-a-tron config matching what you've specified.

Now when you run `skaffold dev` in the nico repo, it will deploy the machine-a-tron config you generated
via `just setup-k3s-env-ips` to your cluster, and machine-a-tron will run with that config. Notably nico will also be
setup with the appropriate overrides for redfish so that it will send all requests to the machine-a-tron service.

> Note: If you follow the above steps to configure nico and machine-a-tron in your cluster, then running
> machine-a-tron locally will not work, because nico will be configured to always use the in-cluster machine-a-tron
> for all libredfish calls. If you want to go back to running locally, you'll want to manually edit the
> generated nico-api-site-config.toml and drop the `override_target_host` line. You may also want to edit the
> `mat.toml` in the same directory and set the host_count to 0 so that the in-cluster machine-a-tron doesn't run any
> mock machines.
