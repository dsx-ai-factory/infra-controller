# machine-a-tron Build & Deployment Guide <Badge intent="info">v2.1</Badge> <Badge intent="launch" minimal>New</Badge>

machine-a-tron is a bare-metal simulator for NICo testing. It hosts mock DPUs and
servers via Redfish BMC, allowing end-to-end NICo flows without real hardware. This
guide documents everything needed to build the container image and deploy it on a
cluster.

<Note title="Version availability">
Everything in this guide — the `nico-machine-a-tron` Helm chart, `helm-prereqs/setup-machine-a-tron.sh`, and the values files it references — is available as of NVIDIA Infra Controller v2.1.
</Note>

## Overview

machine-a-tron runs in **Override Mode**: site-explorer redirects all Redfish traffic
to the mock BMC server running inside the pod. It is only suitable for
simulation-only clusters (no real hardware).

## Quick path: setup-machine-a-tron.sh

For a running NICo Core site, the fastest and most reliable way to deploy is the
end-to-end script, which performs every step in this guide (namespace, pull
secret, CA/Vault secret refresh, BMC credential seeding, `bmc_proxy`
configuration, DHCP-pool sizing, cert reissue, deploy, and verification) and is
idempotent:

```bash
export KUBECONFIG=/path/to/kubeconfig
export REGISTRY_PULL_SECRET=<NVIDIA_API_KEY>   # only if the pull secret is absent
helm-prereqs/setup-machine-a-tron.sh           # add -y for non-interactive
```

## Quick start with 4500-host simulated fleet

Build and push the image to the registry of your choice, then run the setup
script in scale mode. Nothing registry-specific is committed — the image
location, tag, and pull credentials all come from the environment, exactly
like `setup.sh`:

```bash
export KUBECONFIG=/path/to/site/kubeconfig
export NICO_IMAGE_REGISTRY=<registry>/<repo>     # e.g. registry.example.com/nico
export MAT_IMAGE_TAG=<tag>                       # tag you built and pushed (see §2)
export REGISTRY_PULL_SECRET=<api-key>            # omit if the pull secret already exists

MAT_MODE=scale HOST_COUNT=4500 helm-prereqs/setup-machine-a-tron.sh -y
```

That is the whole procedure. The script exits after its bounded verification
window while ingestion continues in-cluster; progress is visible with:

```bash
kubectl exec -n postgres <patroni-primary> -- su postgres -c \
  "psql -d nico_system_nico -tAc \"SELECT
     (SELECT count(*) FROM explored_endpoints) || ' explored / ' ||
     (SELECT count(*) FROM machines) || ' machines';\""
```

### What to expect

The following is measured on a 3-node dev cluster, with dev-sized Postgres.

4500 hosts × 2 DPUs = 13,500 BMC endpoints → 13,500 machines.

| Phase | Duration | Notes |
|-------|----------|-------|
| Deploy + DHCP registration | ~60–90 min | ~150–180 interfaces/min while 13.5k mock FSMs boot |
| Exploration sweep | ~3–5 h | overlaps DHCP; `explorations_per_run=120` per cycle |
| Preingestion | tracks the sweep | ~90% conversion, completes shortly after it |
| Identification + creation | final ~2–3 h | hosts identify in waves; creation drains at ~150–300 machines per explore cycle |
| **End to end** | **~6–9 h, unattended** | 100 hosts ≈ 12 min and 1000 hosts ≈ 25 min, for calibration |

The pipeline is autonomous once the script completes — it has run through
multi-hour client connectivity outages without intervention. Occasional
nico-api restarts under peak ingestion load are absorbed by the pipeline
(machines resume within a cycle). Re-running the script is always safe
(idempotent) and re-registers any expected machines that arrived late.

The rest of this document explains what that script does and why, and is the
reference for manual deployment or debugging. The script's header comments
enumerate the non-obvious failure modes it guards against.

## Prerequisites

- Docker with `buildx` and a `linux/arm64` builder available (see below)
- A container registry you can push to and the cluster can pull from
  (referenced via `NICO_IMAGE_REGISTRY`, same convention as `setup.sh`)
- A cluster where machine-a-tron can reach `nico-api.nico-system.svc.cluster.local:1079`
- The `nico-machine-a-tron` Helm chart (`helm/charts/nico-machine-a-tron`)
- The image pull secret `machine-a-tron-pull` in the `nico-mat` namespace
  (created automatically by the setup script from `REGISTRY_PULL_SECRET`)

## Building the Container Image

### Why cross-compilation is required

machine-a-tron must run on **x86_64** cluster nodes. The Rust dependency `aws-lc-sys`
contains hand-written x86_64 assembly (`s2n-bignum`). Compiling under QEMU emulation
causes a SIGSEGV in the assembler (`bignum_madd_n25519.S`). We therefore use true
cross-compilation: a native `linux/arm64` Rust compiler targeting
`x86_64-unknown-linux-gnu`.

The `carbide-rpc` crate runs a protobuf build script that requires both `protoc`
and the protobuf well-known types (`libprotobuf-dev` on Debian). `libredfish` is a
Git dependency so `git` must also be present in the build stage.

### Build command

Run from the **repository root**:

```bash
# Same convention as setup.sh: your registry/repository prefix, no scheme.
REGISTRY=${NICO_IMAGE_REGISTRY:?export NICO_IMAGE_REGISTRY=<registry>/<repo>}
COMMIT=$(git rev-parse --short HEAD)
TAG="${COMMIT}-amd64"

docker buildx build \
  --platform linux/amd64 \
  -f crates/machine-a-tron/Dockerfile \
  --push \
  -t "${REGISTRY}/machine-a-tron:${TAG}" \
  .
```

<Note>
`--load` does not work for cross-platform builds — use `--push` directly. The build takes ~4–5 minutes on a cold cache; subsequent builds with warm cache are under 30 seconds.
</Note>

### Registry authentication

```bash
docker login "${NICO_IMAGE_REGISTRY%%/*}" \
  -u "${REGISTRY_PULL_USERNAME:-\$oauthtoken}" \
  -p "${REGISTRY_PULL_SECRET}"
```

Some registries use a fixed username with API-key auth — set `REGISTRY_PULL_USERNAME` accordingly (default: `$oauthtoken`).

## Cluster Prerequisites

### Namespaces and secrets

```bash
# Create the deployment namespace
kubectl create namespace nico-mat --dry-run=client -o yaml | kubectl apply -f -

# Image pull secret (replace <API_KEY> with your NVIDIA API key)
kubectl create secret docker-registry machine-a-tron-pull \
  -n nico-mat \
  --docker-server="${NICO_IMAGE_REGISTRY%%/*}" \
  --docker-username="${REGISTRY_PULL_USERNAME:-\$oauthtoken}" \
  --docker-password="${REGISTRY_PULL_SECRET}" \
  --dry-run=client -o yaml | kubectl apply -f -

# Copy nico-roots CA secret from nico-system
kubectl get secret nico-roots -n nico-system -o json \
  | jq 'del(.metadata.namespace,.metadata.resourceVersion,.metadata.uid,.metadata.creationTimestamp)' \
  | kubectl apply -n nico-mat -f -
```

### Vault secrets

machine-a-tron's service account needs the Vault secrets present in its namespace
(even though they are optional — their absence produces confusing log noise):

```bash
for secret in nico-vault-approle-tokens nico-vault-token vault-cluster-info; do
  kubectl get secret "$secret" -n nico-system -o json \
    | jq 'del(.metadata.namespace,.metadata.resourceVersion,.metadata.uid,.metadata.creationTimestamp)' \
    | kubectl apply -n nico-mat -f -
done
```

<Warning>
After a site reprovision, you **must re-copy** `nico-roots` from `nico-system`
as well. A reprovision recreates the nico-system CA; a machine-a-tron carried
over from before will trust the *old* CA (stale `nico-roots`) and present a
client cert signed by the old CA, so **every** mTLS call to nico-api fails
with `client error (Connect)`. Re-copy the CA and delete the old cert secret
(`nico-machine-a-tron-certificate`) so cert-manager reissues from the current
CA:

```bash
kubectl get secret nico-roots -n nico-system -o json \
  | jq 'del(.metadata.namespace,.metadata.resourceVersion,.metadata.uid,.metadata.creationTimestamp,.metadata.ownerReferences)' \
  | kubectl apply -n nico-mat -f -
kubectl delete secret nico-machine-a-tron-certificate -n nico-mat --ignore-not-found
```

</Warning>

### BMC credentials in Vault (required for site-explorer)

site-explorer's `check_preconditions` requires three site-default Vault
credentials before it will explore any endpoint:
`machines/bmc/site/root`, `machines/all_dpus/site_default/uefi-metadata-items/auth`,
and `machines/all_hosts/site_default/uefi-metadata-items/auth`. The two UEFI
paths are created by the nico-prereqs `kvSeeds` but with **empty passwords**,
which fails the check (`vault does not have a valid password entry`) — they
must be re-seeded with any non-empty password. `machines/bmc/site/root` is not
seeded at all; without it every run aborts with `MissingCredentials`.

Beyond the preconditions, the **credential rotation flow** requires this exact
chain (all handled by `setup-machine-a-tron.sh` Phase 4):

| Vault path | Value | Why |
|------------|-------|-----|
| `machines/all_hosts/factory_default/bmc-metadata-items/dell` | `root`/`factory_password` | Host BMC factory default (mock's `DUMMY_FACTORY_PASSWORD`). Path segment is **lowercase** `dell` — `BMCVendor`'s `Display` impl lowercases. |
| `machines/all_dpus/factory_default/bmc-metadata-items/root` | `root`/`0penBmc` | DPU BMC factory default (mock's `DUMMY_FACTORY_DPU_PASSWORD`) — note it differs from the host factory password. |
| `machines/bmc/site/root` | `root`/&lt;distinct&gt; | Rotation target. **Must differ from both factory passwords**, or the rotation is a no-op and the mock rejects with `403 Factory-default password must be changed` forever. |

site-explorer logs into each BMC with its factory default, rotates the password
to the site root value, then proceeds — using the wrong factory password (or a
site root equal to factory) yields `401 Unauthorized`, which latches a
self-perpetuating `AvoidLockout` (NICO-SITEEXPLORER-144) until an operator
clears it (`nico-admin-cli site-explorer refresh <bmc-ip>`).

## Deploying the Helm Chart

### Site values file

Copy `helm-prereqs/values/machine-a-tron.yaml` and fill in the site-specific values:

| Field | Description |
|-------|-------------|
| `image.tag` | Tag produced by [building the container image](#building-the-container-image) (e.g. `8c35783af-amd64`) |
| `machines.dell-hosts.oobDhcpRelayAddress` | Gateway of the OOB/underlay network from nico-core site config |
| `machines.dell-hosts.adminDhcpRelayAddress` | Gateway of the admin network from nico-core site config |
| `machines.dell-hosts.hostCount` | Must not exceed available OOB DHCP addresses (`hostCount + hostCount×dpuPerHostCount`) |

### SPIFFE URI override

<Warning title="Critical step">
This step is critical. Double-check that your values file includes this override.
</Warning>

The cert-manager `Certificate` resource auto-generates a SPIFFE URI based on the
deployment namespace: `spiffe://nico.local/nico-mat/sa/nico-machine-a-tron`.

nico-api's `spiffe_service_base_paths` only includes `/nico-system/sa/` (and two
others), so this URI is **not recognized**. The result is that machine-a-tron's
principal is only `TrustedCertificate` — not `SpiffeServiceIdentifier("machine-a-tron")`
— and every gRPC call beyond `Version` returns HTTP 403.

The values file already includes the fix:

```yaml
certificate:
  uris:
    - "spiffe://nico.local/nico-system/sa/machine-a-tron"
```

This overrides the auto-generated URI so nico-api can correctly identify and authorize
machine-a-tron as the `Machineatron` RBAC principal.

### Deploy

```bash
helm upgrade --install nico-machine-a-tron \
  helm/charts/nico-machine-a-tron \
  -n nico-mat \
  --create-namespace \
  -f helm-prereqs/values/machine-a-tron.yaml
```

## Configuring Override Mode

Configure nico-core's site-explorer to redirect all Redfish traffic to the mock.
Add this to the nico-core site config under `[site_explorer]`:

```toml
[site_explorer]
bmc_proxy = "nico-machine-a-tron-default-bmc-mock.nico-mat.svc.cluster.local:1266"
```

**Use the cross-namespace FQDN.** site-explorer runs inside nico-api in
`nico-system`; a bare service name resolves against that namespace and fails
("connection refused" on every Redfish call) because the mock's Service lives
in `nico-mat`.

**This setting does not survive a nico-core `helm upgrade`** — the ConfigMap
is chart-owned, so an upgrade silently reverts it. Re-run
`setup-machine-a-tron.sh` (idempotent) after any nico-core upgrade.

**Field name matters.** The config field is `bmc_proxy` — a single
`"host:port"` string (`crates/site-explorer/src/config.rs`). The older
`override_target_ip` / `override_target_port` fields are **deprecated**, and
`override_target_host` was never a valid field at all (earlier revisions of
this guide were wrong — a value under that key is silently ignored).

Setting `bmc_proxy` at launch also makes `allow_changing_bmc_proxy` default to
`true`. That is what allows the chart value `machineATron.configureBmcProxyHost`
to work: when set, machine-a-tron calls nico-api's `set_dynamic_config` to set
`bmc_proxy` at runtime, but that call is rejected with `PermissionDenied` unless
`allow_changing_bmc_proxy` is true. The two mechanisms are complementary — the
values file ships `configureBmcProxyHost:
"nico-machine-a-tron-default-bmc-mock.nico-mat.svc.cluster.local"` (FQDN, same
cross-namespace requirement), and the nico-core `bmc_proxy` setting both
enables that path and covers the case where the runtime call has not happened
yet.

Both mechanisms configure the dynamic `site_explorer.bmc_proxy` redirect.
This redirect applies to clients built from `nico-api`'s direct Redfish pool.

The redirect is independent of the static `[bmc_proxy]` configuration, which
routes eligible `nico-api` Redfish traffic through `nico-bmc-proxy`. When
the static section is enabled, eligible traffic uses a proxied pool that
ignores the dynamic redirect. The admin Redfish passthrough also uses the
static configuration when both are configured.

Apply via `helm upgrade` of the nico-core chart, or patch the configmap and
restart nico-api (site-explorer runs in-process in nico-api):

```bash
kubectl rollout restart deployment/nico-api -n nico-system
```

### Why machines get created (expected_machines)

site-explorer's `MachineCreator` refuses to create a managed host unless a
matching `expected_machines` row exists (by BMC MAC) — otherwise it logs
`Refusing to create managed host, expected machines entry not found`. machine-a-tron
auto-registers these when `machineATron.registerExpectedMachines: true` (the
default in the values file). DHCP discovery alone is **not** sufficient.

## Multi-pod simulation with Controller Mode

The chart can shard the simulated fleet across several machine-a-tron pods.
The `mat-k8s-controller` dynamically creates ClusterIP Services for each BMC,
with ClusterIP = BMC IP assigned by NICo DHCP. NICo dials each BMC IP directly
— no `bmc_proxy`. A validated example lives at
`helm-prereqs/values/machine-a-tron-multipod.yaml`.

Everything single-pod mode needs still applies (namespaces, CA copy, Vault
seeds, SPIFFE URI). Multi-pod with controller adds the following requirements:

1. **`oobDhcpRelayAddress` must be within Kubernetes ServiceCIDR.** All pods
   can share the same relay address — NICo assigns unique IPs from the network.
   Default ServiceCIDR ranges:
   - `10.96.0.0/12` - vanilla Kubernetes (kubeadm)
   - `10.96.0.0/16` - kind
   - `10.43.0.0/16` - k3d/K3s

   Check with: `kubectl cluster-info dump | grep service-cluster-ip-range`

1. **NICo siteConfig requirements:**

   ```toml
   allow_insecure_discovery = true

   [networks.MAT-BMC-SERVICES]
   type = "underlay"
   prefix = "10.96.64.0/18"
   gateway = "10.96.64.1"
   mtu = 1500
   ```

1. **Leave `site_explorer.bmc_proxy` unset.** The Redfish client dials each
   BMC's ClusterIP directly.

1. **Disjoint MAC pools per pod.** The Helm chart **auto-generates** unique
   MAC address pools per pod based on pod index. The format is
   `02:00:PP:XX:XX:XX` where `PP` is the pod index (0x00, 0x01, etc.).

   Example with 3 pods:
   - Pod mat-0: `02:00:00:00:00:00` (base)
   - Pod mat-1: `02:00:01:00:00:00` (base)
   - Pod mat-2: `02:00:02:00:00:00` (base)

   Enable with `macAddressPool.enabled: true`.

   Example values:

    ```yaml
    pods:
      default: null
      mat-0:
        machines:
          compute:
            hwType: wiwynn_gb200_nvl
            hostCount: 100
            dpuPerHostCount: 2
            oobDhcpRelayAddress: "10.96.64.1"  # All pods share same relay
            adminDhcpRelayAddress: "192.168.176.1"
      mat-1:
        machines:
          compute:
            hwType: wiwynn_gb200_nvl
            hostCount: 100
            dpuPerHostCount: 2
            oobDhcpRelayAddress: "10.96.64.1"  # NICo assigns unique IPs
            adminDhcpRelayAddress: "192.168.176.1"

    macAddressPool:
      enabled: true

    mat-k8s-controller:
      enabled: true
      config:
        insecureSkipVerify: true  # For self-signed certs
    ```

1. **Hardware-type specifics.** Vendors libredfish does not recognize (e.g.
   `wiwynn_gb200_nvl` reports `WIWYNN`) resolve to `unknown`, so seed the
   host factory credential at
   `machines/all_hosts/factory_default/bmc-metadata-items/unknown`. GB-class
   hosts (`NvidiaGBx00` flow) additionally require `expected_machines` rows
   with a working BMC credential before exploration completes
   (`NICO-SITEEXPLORER-141 Missing credential expected_machine`); on nico-api
   builds without the Machineatron `AddExpectedMachine` RBAC grant the
   auto-registration is 403'd and the rows must be inserted directly with the
   **pinned** password (`hostBmcPassword`), not the factory default. DPU BMCs
   explore without expected rows.

## Verifying Startup

Check that machine-a-tron passes the initial API calls:

```bash
kubectl logs -n nico-mat deployment/nico-machine-a-tron | grep -E "firmware|Got desired|Error:"
```

Expected: `Got desired firmware versions from the server: [...]`

Check nico-api for denied requests (should be empty after the SPIFFE fix):

```bash
kubectl logs -n nico-system deployment/nico-api | grep "Request denied.*machine-a-tron"
```

## DHCP Address Space

machine-a-tron allocates one OOB IP per BMC interface (1 per host + 1 per DPU). With
5 hosts and 2 DPUs each that is **15 IPs** (`5 + 5×2`). Ensure the OOB DHCP prefix in
nico-core is large enough. Note the usable count is smaller than the raw CIDR: a
`/28` (16 addresses) yields only ~10–13 usable after subtracting network,
broadcast, gateway, and any reserved addresses — on dev6 a `/28` yielded 10, so
`hostCount: 3 × 2 DPUs = 9` fit but `5 × 2 = 15` did not. Use at least a `/27`
for the default counts, or reduce `hostCount` / `dpuPerHostCount`. Symptom of
overflow: `No IP addresses left in prefix ...` and machines stuck in `BmcInit`.

If the prefix is exhausted from previous runs, do one of the following:

- Force-delete old machine records via the admin CLI
- Reprovision the site (which clears the database) and redeploy from scratch

<Warning>
Do NOT hand-delete rows from the `machine_interfaces`, `dhcp_entries`, or `machine_interface_addresses` tables to free leases.

The `machine_dhcp_records` view inner-joins the singleton control row `machine_interfaces_deletion` (id=1); if that row is deleted (easy to do by accident when clearing related tables) the view returns zero rows and `DiscoverDhcp` fails for **every** BMC with `Database Error: no rows returned by a query that expected to return at least one row`. If you hit that, restore the row:

```sql
INSERT INTO machine_interfaces_deletion (id) VALUES (1) ON CONFLICT DO NOTHING;
```

</Warning>

The `machine_dhcp_records` view inner-joins the singleton control row `machine_interfaces_deletion` (id=1); if that row is deleted (easy to do by accident when clearing related tables) the view returns zero rows and `DiscoverDhcp` fails for **every** BMC with `Database Error: no rows returned by a query that expected to return at least one row`. If you hit that, restore the row:

## Non-Obvious Fixes

| Problem | Root cause | Fix |
|---------|------------|-----|
| `--load` fails for cross-platform builds | Docker limitation | Use `--push` directly to registry |
| All endpoints latch `AvoidLockout` (NICO-SITEEXPLORER-144) after a cred fix | A previous Unauthorized is self-perpetuating in the exploration report | `nico-admin-cli site-explorer refresh <bmc-ip>` per endpoint (or re-run setup-machine-a-tron.sh, which clears it) |
| `client error (Connect)` on every nico-api call after a reprovision | Stale `nico-roots` CA + client cert signed by the old CA | Re-copy `nico-roots` from nico-system; delete `nico-machine-a-tron-certificate` so cert-manager reissues from the current CA |
| `DiscoverDhcp`: `no rows ... expected to return at least one row` | `machine_interfaces_deletion` singleton (id=1) deleted; breaks `machine_dhcp_records` view | `INSERT INTO machine_interfaces_deletion (id) VALUES (1) ON CONFLICT DO NOTHING;` — never hand-delete lease rows |
| DPU explorations stuck at `403 Factory-default password must be changed` | Site root password equals the factory password → rotation is a no-op | Seed `machines/bmc/site/root` with a password distinct from both factory defaults |
| `exec format error` in pod | Image was built for `arm64`, nodes are `x86_64` | Cross-compile with `--platform linux/amd64` and `x86_64-unknown-linux-gnu` Rust target |
| `File not found: google/protobuf/timestamp.proto` | `libprotobuf-dev` absent in build image | Add `libprotobuf-dev` to `apt-get install` in builder stage |
| `git fetch ... (exit status: 127)` | `libredfish` is a git dependency, `git` not in slim image | Add `git` to builder stage |
| Host BMCs 401 while DPUs explore fine | Host and DPU factory passwords differ (`factory_password` vs `0penBmc`); host factory cred missing or wrong | Seed `machines/all_hosts/factory_default/bmc-metadata-items/dell` = `root`/`factory_password` (lowercase `dell`) |
| HTTP 403 on every gRPC call | machine-a-tron cert SPIFFE URI not in nico-api's `service_base_paths` | Set `certificate.uris: ["spiffe://nico.local/nico-system/sa/machine-a-tron"]` in values |
| Machine creation fails `No IP addresses left in prefix <admin-cidr>` | Admin pool too small: creation needs one host-PF IP per DPU | Fit `hostCount×dpuPerHostCount` ≤ usable admin-pool IPs (the script auto-fits) |
| `No IP addresses left in prefix ...`; machines stuck in `BmcInit` | OOB DHCP pool too small for host×DPU count | Sizing: `hostCount + hostCount×dpuPerHostCount` ≤ usable pool IPs; use ≥ /27 or reduce counts |
| Redfish `connection refused` on every endpoint despite bmc_proxy set | Bare service name resolves against nico-system, not nico-mat | Use the cross-namespace FQDN in `bmc_proxy` |
| Redfish redirect ignored; `endpoint_explorations=0` | Wrong config field (`override_target_host` is not real) | Use `bmc_proxy = "nico-machine-a-tron-default-bmc-mock.nico-mat.svc.cluster.local:1266"` under `[site_explorer]` |
| `Refusing to create managed host, expected machines entry not found` | No `expected_machines` row for the discovered BMC MAC | Set `machineATron.registerExpectedMachines: true` (default) so machine-a-tron auto-registers them |
| `Refusing to create managed host`; machine-a-tron logs `PermissionDenied` on registration | nico-api build lacks the `Machineatron` → `AddExpectedMachine` RBAC grant | Rebuild nico-api with the grant (internal_rbac_rules.rs); the setup script also has a DB fallback |
| `SIGSEGV` compiling `aws-lc-sys` | QEMU emulates the `.S` assembler, which crashes | True cross-compilation (native arm64 host → x86_64 target) instead of QEMU |
| site-explorer aborts with `MissingCredentials .../uefi-metadata-items/auth` | kvSeeds create the UEFI creds with **empty** passwords, which fail validation | Re-seed both site_default UEFI creds with any non-empty password |
| site-explorer aborts with `MissingCredentials machines/bmc/site/root` | Site BMC root cred not in default `kvSeeds` | Seed `secrets/machines/bmc/site/root` = `root`/&lt;non-factory password&gt; in Vault |
