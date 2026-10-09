# Testing Hardware Discovery with Sushy BMC Emulator

Sushy (`sushy-tools`) is an OpenStack Redfish emulator backed by libvirt.
It lets you test NICo's site explorer hardware discovery without physical
BMC hardware. The site explorer connects to Sushy via standard Redfish,
detects the `Sushy` hardware type, and produces an exploration report for
the managed VM.

Sushy also supports Redfish power control and boot-source override,
translating them into libvirt domain operations. This means NICo can
power on/off VMs and set PXE boot via Redfish — the full provisioning
flow works if DHCP/PXE network connectivity is available.

## Prerequisites

- libvirt with QEMU/KVM
- Podman (to run the Sushy container)
- An OpenShift cluster (CRC works) with NICo deployed

## Sushy Emulator Setup

### Create a libvirt VM

Create a VM with a fixed UUID and MAC address. Sushy uses the VM UUID
as the Redfish System ID.

```bash
virt-install --name edge-ipc-01 \
  --uuid aaaaaaaa-1001-4000-8000-aabbccddee01 \
  --memory 16384 --vcpus 8 \
  --os-variant generic --boot uefi \
  --disk size=50 \
  --network network=default,mac=aa:bb:cc:dd:ee:01 \
  --noautoconsole --noreboot
```

> **Networking:** The `default` libvirt network is sufficient for
> discovery. For full PXE provisioning, the VM must reach NICo's
> DHCP/PXE services — configure a bridged network with DHCP relay or
> place the VM on the same L2 segment as the NICo DHCP server.

### Configure Sushy

Create `/etc/sushy-emulator/sushy-emulator.conf`. This configuration
runs the emulator **without authentication** (`SUSHY_EMULATOR_AUTH_FILE`
is not set). The BMC credentials stored in Vault are accepted by the
Sushy vendor stub but are not actually verified by the emulator:

```python
SUSHY_EMULATOR_LISTEN_IP = "0.0.0.0"
SUSHY_EMULATOR_LISTEN_PORT = 10443
SUSHY_EMULATOR_SSL_CERT = "/etc/sushy-emulator/ssl/sushy.crt"
SUSHY_EMULATOR_SSL_KEY = "/etc/sushy-emulator/ssl/sushy.key"
SUSHY_EMULATOR_LIBVIRT_URI = "qemu:///system"
SUSHY_EMULATOR_ALLOWED_INSTANCES = [
    "aaaaaaaa-1001-4000-8000-aabbccddee01"
]
```

Generate a self-signed certificate with the host IP as a SAN:

```bash
HOST_IP="192.168.1.51"  # adjust to your host's IP
sudo mkdir -p /etc/sushy-emulator/ssl
sudo openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:secp384r1 \
  -nodes -days 3650 -subj '/CN=sushy-emulator' \
  -addext "subjectAltName=IP:${HOST_IP},IP:127.0.0.1,DNS:localhost" \
  -keyout /etc/sushy-emulator/ssl/sushy.key \
  -out /etc/sushy-emulator/ssl/sushy.crt
```

> **Note:** This development flow skips certificate verification.
> NICo's Redfish clients currently accept invalid certificates, and the
> `curl` examples below use `-k`. The self-signed certificate provides
> transport encryption but is not verified by either party.

### Patch the ServiceRoot template

The nv-redfish library requires a `Links` object in the Redfish
ServiceRoot. The default Sushy template does not include it. Create
`/etc/sushy-emulator/root.json`.

This file is a Jinja2 template — the `{% if %}` directives are
rendered by Sushy at request time:

```jinja2
{
    "@odata.type": "#ServiceRoot.v1_5_0.ServiceRoot",
    "Id": "RedvirtService",
    "Name": "Redvirt Service",
    "RedfishVersion": "1.5.0",
    "Vendor": "Sushy",
    "UUID": "85775665-c110-4b85-8989-e6162170b3ec",
    {% if feature_set == "full" %}
    "Chassis": {
        "@odata.id": "/redfish/v1/Chassis"
    },
    {% endif %}
    "Systems": {
        "@odata.id": "/redfish/v1/Systems"
    },
    {% if feature_set != "minimum" %}
    "Managers": {
        "@odata.id": "/redfish/v1/Managers"
    },
    {% endif %}
    {% if feature_set == "full" %}
    "Registries": {
        "@odata.id": "/redfish/v1/Registries"
    },
    "CertificateService": {
        "@odata.id": "/redfish/v1/CertificateService"
    },
    "UpdateService": {
        "@odata.id": "/redfish/v1/UpdateService"
    },
    {% endif %}
    "Links": {
        "Sessions": {
            "@odata.id": "/redfish/v1/SessionService/Sessions"
        }
    },
    "@odata.id": "/redfish/v1/",
    "@Redfish.Copyright": "Copyright 2014-2016 Distributed Management Task Force, Inc. (DMTF). For the full DMTF copyright policy, see http://www.dmtf.org/about/policies/copyright."
}
```

### Run Sushy as a systemd service

Create `/etc/systemd/system/sushy-emulator.service`.

> **Note:** `--privileged` and host networking are required because
> Sushy needs access to the libvirt socket. Run this only on a
> development workstation, not on shared or production hosts.

The `root.json` bind-mount path depends on the Python version inside the
container image. Verify with `podman run --rm <image> python3 --version`
and adjust if needed.

```ini
[Unit]
Description=Sushy Redfish Emulator for Libvirt
After=network-online.target libvirtd.service
Wants=network-online.target

[Service]
Type=simple
Restart=always
RestartSec=10
ExecStartPre=-/usr/bin/podman kill sushy-emulator
ExecStartPre=-/usr/bin/podman rm sushy-emulator
ExecStart=/usr/bin/podman run --rm --name sushy-emulator \
  --net=host \
  --privileged \
  -v /var/run/libvirt:/var/run/libvirt \
  -v /etc/sushy-emulator:/etc/sushy-emulator:ro \
  -v /etc/sushy-emulator/root.json:/usr/local/lib/python3.12/site-packages/sushy_tools/emulator/templates/root.json:ro \
  quay.io/metal3-io/sushy-tools:latest \
  sushy-emulator --config /etc/sushy-emulator/sushy-emulator.conf
ExecStop=/usr/bin/podman stop sushy-emulator

[Install]
WantedBy=multi-user.target
```

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now sushy-emulator
```

### Verify

```bash
curl -sk https://localhost:10443/redfish/v1/Systems | python3 -m json.tool
```

You should see your VM listed as a Redfish System.

To verify from inside an OpenShift pod:

```bash
oc run test-curl --rm -i --restart=Never \
  --image=registry.access.redhat.com/ubi9/ubi-minimal \
  -- curl -sk https://<host-ip>:10443/redfish/v1
```

## NICo Site Explorer Configuration

Add the following to the site explorer config in `nicoApiSiteConfig`:

```toml
[site_explorer]
explore_mode = "nv-redfish"
bmc_proxy = "<host-ip-reachable-from-cluster>:10443"
dpu_policy = "ignore"
run_interval = "30s"
create_machines = true
```

- `bmc_proxy`: The host IP and port that OpenShift pods can reach
  (not `localhost`). On CRC, the host's LAN IP works.
- `dpu_policy = "ignore"`: Sushy VMs have no DPUs.

## Register the Expected Machine

Register the VM as an expected machine. The MAC address must match the
VM's NIC, and `bmc_ip_address` provides the static BMC IP for the
site explorer to use with `bmc_proxy`.

> **Note:** The credentials below are test-only placeholders for a local
> emulator. Do not reuse them in shared environments. Prefer injecting
> credentials via Vault rather than passing them on the command line,
> which exposes them in shell history and process listings.

```bash
nico-admin-cli expected-machine add \
  --bmc-mac-address "aa:bb:cc:dd:ee:01" \
  --bmc-username "test-user" \
  --bmc-password "test-only-not-a-real-password" \
  --chassis-serial-number "437XR1138R2" \
  --bmc-ip-address "192.168.50.101" \
  --bmc-retain-credentials true \
  --dpu-policy ignore \
  --meta-name "edge-ipc-01"
```

The site explorer also needs:
- A **network segment** of type `underlay` with a prefix covering the
  `bmc_ip_address` range.
- **DHCP timestamps** on the preallocated machine interfaces (set
  `last_dhcp` in the `machine_interfaces` table, or call the
  `DiscoverDhcp` gRPC method).

## Vault Credentials

Use `vault kv put` with `@file` syntax to store JSON objects (not strings).

> **Note:** Replace the placeholder credentials below with values
> appropriate for your test environment.

```bash
CRED_FILE="$(mktemp)"
trap 'rm -f "$CRED_FILE"' EXIT
printf '{"UsernamePassword":{"username":"test-user","password":"test-only"}}' > "$CRED_FILE"
chmod 600 "$CRED_FILE"
```

The site explorer checks these three credentials at startup
(`REQUIRED_SITE_DEFAULT_CREDENTIAL_KEYS`) and fails if any are missing:

```bash
vault kv put secrets/machines/bmc/site/root @"$CRED_FILE"
vault kv put secrets/machines/all_hosts/site_default/uefi-metadata-items/auth @"$CRED_FILE"
vault kv put secrets/machines/all_dpus/site_default/uefi-metadata-items/auth @"$CRED_FILE"
```

These are consumed later by BMC metadata workflows (credential rotation,
factory-default lookups) and are not required for startup:

```bash
vault kv put secrets/machines/all_hosts/site_default/bmc-metadata-items/root @"$CRED_FILE"
vault kv put secrets/machines/all_dpus/site_default/bmc-metadata-items/root @"$CRED_FILE"
```

A Vault Kubernetes auth role for `nico-api` is also required:

```bash
vault write auth/kubernetes/role/nico-api \
  bound_service_account_names=nico-api \
  bound_service_account_namespaces=nico-system \
  policies=nico-vault-policy \
  ttl=24h
```

## What to Expect

Once everything is configured, the site explorer logs should show:

```text
endpoint_explorations=1 endpoint_explorations_success=1
endpoint_explorations_failures=0
```

The exploration report contains:
- `EndpointType: Bmc`
- `MachineSetupStatus.IsDone: true` (Sushy has no real BIOS to configure)
- One system and one chassis

## Limitations

- **PXE provisioning requires DHCP relay**: Sushy supports Redfish
  power control and boot-source override (translated to libvirt
  operations), so NICo can set PXE boot and power on VMs. However,
  the VMs must be able to reach NICo's DHCP/PXE services over the
  network for the full provisioning flow to work.
- **No DPU support**: Sushy VMs have no DPUs. Use `dpu_policy = "ignore"`.
- **No BIOS/lockdown**: All BIOS setup and lockdown checks are stubbed
  as no-ops for the Sushy hardware type.
- **No AccountService**: Sushy does not implement the Redfish
  `AccountService` endpoint. All credential operations (create user,
  change password) are accepted silently by the vendor implementation.
- **Single VM per Sushy instance**: NICo's site explorer selects one
  system per exploration (multi-system Redfish endpoints are not
  supported). Since Sushy exposes all libvirt domains through one
  `/Systems` collection, multiple expected machines routed through
  `bmc_proxy` to the same Sushy endpoint will all discover the same
  system. To test multiple VMs, run one Sushy instance per VM on
  different ports.
