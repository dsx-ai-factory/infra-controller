# Scaling NICo with machine-a-tron: 100 → 1000 → 4500 simulated hosts

> **Status:** Scale testing validated with up to 13,500 BMC endpoints.

## Quick start

Reset the site between runs, check the BMC network against the ServiceCIDR,
then install a scale profile. Refer to
[Teardown and Reset](machine-a-tron-deployment.md#teardown-and-reset) for the
reset. [Cluster Prerequisites](machine-a-tron-deployment.md#cluster-prerequisites)
covers the namespace label and pull Secret the install expects.
[Controller Mode](machine-a-tron-deployment.md#controller-mode) lists the Core
values a simulation site needs.

```bash
export KUBECONFIG=/path/to/site/kubeconfig
helm uninstall nico-machine-a-tron -n nico-mat
python3 helm-prereqs/check-mat-service-cidr.py helm-prereqs/values/machine-a-tron-scale.yaml \
  --site-config helm-prereqs/values/nico-core-simulation.yaml &&
helm upgrade --install nico-machine-a-tron helm/charts/nico-machine-a-tron \
  -n nico-mat --create-namespace --qps 15 --burst-limit 30 \
  --set image.repository=<registry>/machine-a-tron --set image.tag=<tag> \
  --set mat-k8s-controller.image.repository=<registry>/mat-k8s-controller \
  --set mat-k8s-controller.image.tag=<tag> \
  -f helm-prereqs/values/machine-a-tron-scale.yaml
```

## What this work delivers

1. **Controller Mode** in the `nico-machine-a-tron` chart, with the
   `mat-k8s-controller` for dynamic per-BMC Services, and the scale profiles
   under `helm-prereqs/values/`: `machine-a-tron-scale.yaml`,
   `machine-a-tron-multipod.yaml`, `machine-a-tron-scale-4500.yaml`, and
   `machine-a-tron-scale-4500-proxy.yaml`. `machine-a-tron-10racks.yaml` adds
   the ten-rack GB200 NVL72 example with the Rack Management Service (RMS)
   mock behind the protocol gateway.
1. **`helm-prereqs/values/nico-core-simulation.yaml`**, the NICo Core values
   for a simulation site: `allow_insecure_discovery`, the site-explorer
   throughput knobs, the three simulated networks, and the pool sizes.
1. **`helm-prereqs/check-mat-service-cidr.py`**, the BMC network versus
   ServiceCIDR preflight, and **`helm-prereqs/ingestion-rate-report.sh`**,
   the per-run ingestion curves from the database timestamps.

## Architecture: Controller Mode

The `mat-k8s-controller` dynamically creates one Service per BMC:

- Discovers machine-a-tron pods via `nvidia-infra-controller/mat-service=true` label
- Polls `/machines/status` from each pod
- Creates Services with the BMC IP (assigned by NICo DHCP) as `externalIPs`
- Services route to correct pod via `nvidia-infra-controller/pod-name` selector

**Requirements:**

- The BMC network must lie outside the Kubernetes ServiceCIDR, pod CIDR,
  node network, and networks that nodes or pods must otherwise reach
  (BMC IPs are Service externalIPs, for which kube-proxy programs forwarding
  rules on every node). Run `helm-prereqs/check-mat-service-cidr.py` against
  the values file before every install. It resolves each BMC relay to its
  `[networks.*]` prefix and fails on an overlap or when it cannot determine
  the ServiceCIDR. `SCALE_SERVICE_CIDRS` replaces the cluster lookup, and
  `SCALE_BMC_PREFIXES` adds networks the site config does not declare yet
- NICo siteConfig needs `allow_insecure_discovery = true` and a network
  covering the BMC IP range
- Leave `site_explorer.bmc_proxy` unset - NICo dials each BMC IP directly

**NICo siteConfig** (`helm-prereqs/values/nico-core-simulation.yaml`):

```toml
allow_insecure_discovery = true

[networks.simulated-oob]
type = "underlay"
prefix = "10.200.0.0/18"
gateway = "10.200.0.1"
mtu = 9000
reserve_first = 1
```

The file also declares `simulated-admin` and `simulated-underlay` (the
`underlayDhcpRelayAddress` target) and sizes `[pools.lo-ip]` and
`[pools.fnn-asn]` for 4,500 hosts with 2 DPUs.

## Complete issue log

Every issue below was found live on a 3-node development cluster. The Fix
column names the file or section that now carries each fix.

### Baseline (override-mode) end-to-end

| # | Issue | Root cause | Fix |
|---|-------|------------|-----|
| 1 | Every nico-api call fails `client error (Connect)` after a site reprovision | machine-a-tron trusts the old CA (stale `nico-roots` copy) and presents a cert signed by it | The chart labels the namespace so ESO syncs `nico-roots`. After a reprovision delete the `<release>-<pod>-tls` Secrets by label (`controller.cert-manager.io/fao=true`) so cert-manager reissues from the current CA |
| 2 | Redfish redirect silently ignored | Docs said `override_target_host`, never a valid field. The real field is `bmc_proxy = "host:port"`, and it must be the **cross-namespace FQDN** (site-explorer runs in nico-system, where a bare service name does not resolve) | `bmc_proxy` is set in the Core values (commented line in `nico-core-simulation.yaml`); docs fixed |
| 3 | site-explorer aborts every run: `MissingCredentials` | `machines/bmc/site/root` isn't in default kvSeeds; the seeded UEFI creds ship with **empty** passwords which fail validation | `siteCredentials` in `helm-prereqs/values.yaml` renders all three as the nico-api credential file and generates any password left empty |
| 4 | Host BMCs 401 while DPUs explore fine | Host and DPU mock factory passwords differ (`factory_password` vs `0penBmc`). The host factory Vault path vendor segment is **lowercase** (`.../dell`, because `BMCVendor`'s `Display` lowercases, so the earlier capital-`Dell` seed was read by nobody) | `kvSeeds` in `helm-prereqs/values.yaml` (the host `dell` entry is commented). The scale profiles leave the chart's site credentials lookup enabled, so the mocks are pinned and no factory login happens |
| 5 | machine-a-tron's expected-machine registration 403s (each failed record is logged and startup exits non-zero) | `Machineatron` principal missing from the `AddExpectedMachine` RBAC grant - an oversight; it holds the sibling grants (`DiscoverDhcp`, `CreateNetworkSegment`, `GetExpectedSwitch`) | One-line fix in `internal_rbac_rules.rs`, merged on main. No fallback remains |
| 6 | Endpoints permanently stuck `AvoidLockout` (NICO-SITEEXPLORER-144) on a fresh deploy | Per-MAC rotated creds (`machines/bmc/<mac>/root`) survive cleanup; a fresh mock is at factory password but the per-MAC entry makes site-explorer present the old rotated one → 401 latch, self-perpetuating by design | The pinned passwords (the site root from the site credentials Secret) prevent the per-MAC writes under the scale profiles. Override Mode sites purge them as described in the deployment guide's Teardown and Reset section |
| 7 | `DiscoverDhcp` fails for every BMC: "no rows returned…" | The `machine_dhcp_records` VIEW inner-joins a singleton control row (`machine_interfaces_deletion` id=1); manual lease cleanup had deleted it | Documented in the deployment guide: never delete lease rows manually, and the restore statement |
| 8 | Machines never created: admin pool exhausted | Real demand is OOB = `hosts×(1+dpus)` and admin = `hosts×(dpus+1)` (one host-PF IP per DPU **plus one per host at creation**); usable = `2^(32-mask) − reserve_first − 1` | Demand formulas in the deployment guide's DHCP Address Space; `nico-core-simulation.yaml` sizes the segments for 4,500 hosts x 2 DPUs |

### Scale mode (100 hosts × 2 DPUs and up)

| # | Issue | Root cause | Fix |
|---|-------|------------|-----|
| 9 | helm deploy aborts: hundreds of `connection reset by peer` | helm's default burst (100 concurrent API calls) overwhelms SOCKS/ssh tunnels when creating hundreds of Services | `--qps 15 --burst-limit 30` on `helm upgrade --install` (chart README, Controller Mode) |
| 10 | nginx bmc-proxy CrashLoopBackOff: `host not found in upstream` | Chart template pointed the upstream at the bare chart name, which is not a Service | Point at the `-bmc-mock` Service (chart fix) |
| 11 | Every registry lookup 404s: `no router configured for host: 10.233.x.x` | nginx forwarded `host=$server_addr`, but kube-proxy DNATs the LB IP to the nginx **pod IP** before the connection arrives | `Forwarded "host=$host"` - the client-requested host is the LB IP end-to-end (chart fix) |
| 12 | LB IPs intermittently Unreachable in-cluster | Per-BMC Services use `externalTrafficPolicy: Local` and the chart's REQUIRED podAffinity stacked all proxies on the mat node | Required anti-affinity between proxy replicas (+ `maxUnavailable=1/maxSurge=0`; with replicas == nodes a surge pod deadlocks the rollout) - chart fix, kept for nginx-mode users |
| 13 | DHCP fails: `No network segment defined for relay addresses` | Config-driven segment creation is **bootstrap-once** and skipped entirely on multi-domain sites ("Multiple domains, skipping initial network creation") | Declare the segments in the Core values before nico-api first starts (`nico-core-simulation.yaml`). Established sites use `nico-admin-cli network-segment create` (deployment guide, Established Sites) |
| 14 | AvoidLockout storm on all DPU endpoints; preingestion pinned at exactly `hostCount` | The rotation dance is racy at scale: preingestion's initial BMC reset reboots the mock, which returns at the **factory** password while its per-MAC Vault entry says "rotated" | Pin mock passwords to the site root (the chart reads it from the site credentials Secret and pins every mock). site-explorer's documented fallback ("factory failed → sitewide root, no rotation") logs straight in and resets become harmless |
| 15 | Pipeline stalls at preingestion `initial`; manager idle | `waiting_for_explorer_refresh` (set when errors are cleared) gates endpoints out of preingestion and can linger after a healthy report lands (273/300 were parked) | `nico-admin-cli site-explorer refresh <bmc-ip>` unparks an endpoint. With pinned passwords the condition did not recur at 4,500 hosts |
| 16 | Managed hosts identified but machines never created, and cycles never finish | `explorations_per_run` was raised to 400 "for throughput", but identification and creation only run **at the end of a completed explore cycle**, and 400 deep scans per cycle meant cycles stopped completing | Default lowered to 120 at the time: cycles complete in about 1 to 2 min and creation runs every cycle. The default has since been raised to 360, and `nico-core-simulation.yaml` ships 360 with a cycle-completion caveat. Refer to [Large Site Sizing and Settings](large-site-sizing-and-settings.md) for details. |
| 17 | `Resource pool lo-ip is empty` on the 3rd machine | Machine creation allocates one loopback IP per machine. Pool **definitions are seed-once** ("Declaration has drifted since seed ... not re-applying"), so config widening is ignored, and the development site template ships **3** lo-ip addresses | `[pools.lo-ip]` in `nico-core-simulation.yaml` declares 16,382 addresses. Established sites grow the pool with `nico-admin-cli resource-pool grow` |

### A note on the verification loop

The retired setup script's final phase actively shepherded the pipeline. It
re-cleared `AvoidLockout` and `Unauthorized` latches (they are one-way by
design) and unparked healthy endpoints. With pinned BMC passwords the latch
clearing was a no-op for the whole stage-3 run, so the chart path carries no
equivalent. An operator clears a latched endpoint with
`nico-admin-cli site-explorer refresh <bmc-ip>` or
`nico-admin-cli site-explorer clear-error <bmc-ip>`.

## Where we are today

| Stage | Scale | Result |
|-------|-------|--------|
| Baseline | 1 host × 1 DPU (override mode) | ✅ end-to-end: machines created, full credential rotation exercised |
| Stage 1 | 100 hosts × 2 DPUs = 300 BMCs (proxy-direct) | ✅ 300/300 endpoints stable, machines created and advancing through `hostinit`/`dpuinit` |
| Stage 2 | 1000 hosts × 2 DPUs = 3000 BMCs | ✅ **END TO END OK - 3000/3000 machines** in one unattended run. About 25 min total, creation about 240 machines/min. |
| Stage 3 | 4500 hosts × 2 DPUs = 13,500 BMCs | ✅ **13,500/13,500 machines - 100% fleet.** The first run explored 13,500 endpoints and created over 10,000 machines. A rerun on the ClusterIP chart and a fresh cluster reached every counter: 13,500 explored, 13,500 preingestion-complete, 4,500 hosts, 13,500 machines, about 15 h unattended. |

Stage-3 observations worth reviewers' attention:

- **The ingestion pipeline is fully autonomous once configured.** Client
  connectivity to the cluster dropped twice for extended periods during
  stage 3; ingestion continued unattended both times (e.g. +720 machines
  through one outage, +4,000 through another). The shepherd loop's latch
  clearing, critical in earlier iterations, was a no-op for the entire
  stage-3 run thanks to pinned credentials.
- **Measured stage-3 rates on the 3-node development cluster:** DHCP
  ~110 interfaces/min; exploration ~120 to 360 endpoints/cycle; creation
  40 to 240 machines per completed explore cycle, sawtoothing with cycle
  phasing (identification rebuilds `explored_managed_hosts` each cycle).
- **Per-MAC Vault credential lifecycle needs batching at scale** (issue 18
  below): site-explorer stores one `machines/bmc/<mac>/root` entry per BMC,
  13,500 entries. Deleting them one API round-trip at a time takes hours,
  batched server-side it takes seconds.
- `expected_machines` auto-registration worked at stage 3 (9,890+ registered
  by machine-a-tron via the API), confirming the RBAC grant path.

Additional issue found at stage 3:

| # | Issue | Root cause | Fix |
|---|-------|------------|-----|
| 18 | Stage-2→3 cleanup ran for over an hour "deleting credentials" | One `kubectl exec` per per-MAC Vault deletion × thousands of entries | Batch the deletion on the Vault pod in one exec (deployment guide, Teardown and Reset). Pinned passwords avoid the entries altogether |

## Open Questions - Feedback Wanted

1. **RBAC**: `Machineatron` → `AddExpectedMachine` is granted on main
   (commit `9a9ba072a`), and machine-a-tron registers expected machines
   through the API. Resolved.
1. **Seed-once reconcile semantics**: networks, and resource-pool
   definitions are all create-once; config changes on established sites are
   silently ignored (or warn-only). The chart path declares them before first
   start and uses `nico-admin-cli network-segment create` and
   `resource-pool grow` on established sites. Should NICo support declarative
   updates for these instead?
1. **AvoidLockout at scale**: one-way latches are right for real BMCs, but
   simulation fleets guarantee latch storms during resets. Worth a
   site-config escape hatch (e.g. `site_explorer.lockout_protection = false`)
   instead of operator-side clearing?
1. **Mock fidelity**: the mock returns to its configured password after a
   BMC reset. Real BMCs persist a rotated password across resets. Should
   bmc-mock persist rotated credentials so the rotation path can be exercised
   at scale without pinning?
1. **lo-ip per machine**: is one loopback IP per machine the intended
   allocation at 13.5k machines, and is there guidance for sizing this pool
   in production site templates (dev templates ship 3)?
1. **Cycle economics**: identification/creation only run at the end of a
   completed `explore_site` cycle, so `explorations_per_run` trades sweep
   throughput against creation latency in a non-obvious way. Worth
   documenting (or decoupling creation from the exploration cycle)?
