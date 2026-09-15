# Scaling NICo with machine-a-tron: 100 → 1000 → 4500 simulated hosts

> **Status:** Scale testing validated with up to 13,500 BMC endpoints.

## Quick start

```bash
export KUBECONFIG=/path/to/site/kubeconfig
helm-prereqs/cleanup-machine-a-tron.sh -y
MAT_MODE=scale HOST_COUNT=1000 helm-prereqs/setup-machine-a-tron.sh -y
```

## What this work delivers

1. **`helm-prereqs/setup-machine-a-tron.sh`** — one idempotent script that
   takes a running NICo site from nothing to created machines: namespace,
   pull secret, CA/Vault secret refresh, the full BMC/UEFI credential chain,
   nico-core site-config changes, DHCP pool sizing with auto-fit, DB safety
   checks, helm deploy, and a verification loop that actively shepherds the
   ingestion pipeline.
1. **`helm-prereqs/cleanup-machine-a-tron.sh`** — the full inverse, so
   from-scratch runs are reproducible.
1. **`MAT_MODE=scale`** — a scale profile
   (`helm-prereqs/values/machine-a-tron-scale.yaml`) using Controller Mode
   with the `mat-k8s-controller` for dynamic per-BMC ClusterIP Services.

## Architecture: Controller Mode

The `mat-k8s-controller` dynamically creates one ClusterIP Service per BMC:
- Discovers machine-a-tron pods via `nvidia-infra-controller/mat-service=true` label
- Polls `/machines/status` from each pod
- Creates Services with ClusterIP = BMC IP (assigned by NICo DHCP)
- Services route to correct pod via `nvidia-infra-controller/pod-name` selector

**Requirements:**
- `bmcDhcpRelayAddress` must be within Kubernetes ServiceCIDR
- NICo siteConfig needs `allow_insecure_discovery = true` and a network
  covering the BMC IP range
- Leave `site_explorer.bmc_proxy` unset — NICo dials each BMC's ClusterIP directly

**Example NICo siteConfig:**
```toml
allow_insecure_discovery = true

[networks.MAT-BMC-SERVICES]
type = "underlay"
prefix = "10.96.64.0/18"
gateway = "10.96.64.1"
mtu = 1500
```

## Complete issue log

Every issue below was found live on dev6 and is fixed on the branch, encoded
in the scripts/charts with explanatory comments.

### Baseline (override-mode) end-to-end

| # | Issue | Root cause | Fix |
|---|-------|------------|-----|
| 1 | Every nico-api call fails `client error (Connect)` after a site reprovision | machine-a-tron trusts the old CA (stale `nico-roots` copy) and presents a cert signed by it | Script refreshes `nico-roots` + Vault secrets from nico-system and deletes the client-cert secret so cert-manager reissues from the current CA |
| 2 | Redfish redirect silently ignored | Docs said `override_target_host` — never a valid field; the real field is `bmc_proxy = "host:port"`, and it must be the **cross-namespace FQDN** (site-explorer runs in nico-system; a bare service name doesn't resolve) | Script sets `bmc_proxy` correctly; docs fixed |
| 3 | site-explorer aborts every run: `MissingCredentials` | `machines/bmc/site/root` isn't in default kvSeeds; the seeded UEFI creds ship with **empty** passwords which fail validation | Script seeds the full chain |
| 4 | Host BMCs 401 while DPUs explore fine | Host and DPU mock factory passwords differ (`factory_password` vs `0penBmc`); the host factory Vault path vendor segment is **lowercase** (`…/dell` — `BMCVendor`'s `Display` lowercases; the earlier capital-`Dell` seed was read by nobody) | Script seeds both factory creds on the correct paths |
| 5 | machine-a-tron's expected-machine registration 403s (logged misleadingly as "likely already ingested") | `Machineatron` principal missing from the `AddExpectedMachine` RBAC grant — an oversight; it holds the sibling grants (`DiscoverDhcp`, `CreateNetworkSegment`, `GetExpectedSwitch`) | One-line fix in `internal_rbac_rules.rs`; script includes a DB fallback for nico-api builds without it |
| 6 | Endpoints permanently stuck `AvoidLockout` (NICO-SITEEXPLORER-144) on a fresh deploy | Per-MAC rotated creds (`machines/bmc/<mac>/root`) survive cleanup; a fresh mock is at factory password but the per-MAC entry makes site-explorer present the old rotated one → 401 latch, self-perpetuating by design | Cleanup purges per-MAC creds; setup self-heals stale ones (only when the machine graph is truly empty — machines AND interfaces at 0) |
| 7 | `DiscoverDhcp` fails for every BMC: "no rows returned…" | The `machine_dhcp_records` VIEW inner-joins a singleton control row (`machine_interfaces_deletion` id=1); manual lease cleanup had deleted it | Script restores the singleton; documented: never hand-delete lease rows |
| 8 | Machines never created: admin pool exhausted | Real demand is OOB = `hosts×(1+dpus)` and admin = `hosts×(dpus+1)` (one host-PF IP per DPU **plus one per host at creation**); usable = `2^(32-mask) − reserve_first − 1` | Sizing check with auto-fit; `reserve_first` parsed from the live site config |

### Scale mode (100 hosts × 2 DPUs and up)

| # | Issue | Root cause | Fix |
|---|-------|------------|-----|
| 9 | helm deploy aborts: hundreds of `connection reset by peer` | helm's default burst (100 concurrent API calls) overwhelms SOCKS/ssh tunnels when creating hundreds of Services | `--qps 15 --burst-limit 30` (env-overridable) |
| 10 | nginx bmc-proxy CrashLoopBackOff: `host not found in upstream` | Chart template pointed the upstream at the bare chart name, which is not a Service | Point at the `-bmc-mock` Service (chart fix) |
| 11 | Every registry lookup 404s: `no router configured for host: 10.233.x.x` | nginx forwarded `host=$server_addr`, but kube-proxy DNATs the LB IP to the nginx **pod IP** before the connection arrives | `Forwarded "host=$host"` — the client-requested host is the LB IP end-to-end (chart fix) |
| 12 | LB IPs intermittently Unreachable in-cluster | Per-BMC Services use `externalTrafficPolicy: Local` and the chart's REQUIRED podAffinity stacked all proxies on the mat node | Required anti-affinity between proxy replicas (+ `maxUnavailable=1/maxSurge=0`; with replicas == nodes a surge pod deadlocks the rollout) — chart fix, kept for nginx-mode users |
| 13 | DHCP fails: `No network segment defined for relay addresses` | Config-driven segment creation is **bootstrap-once** — skipped entirely on multi-domain sites ("Multiple domains, skipping initial network creation") | Script clone-inserts the simulated segments from same-type templates; `allocation_strategy` forced to `dynamic` (templates may be `reserved`, which rejects all dynamic DHCP) |
| 14 | AvoidLockout storm on all DPU endpoints; preingestion pinned at exactly `hostCount` | The rotation dance is racy at scale: preingestion's initial BMC reset reboots the mock, which returns at the **factory** password while its per-MAC Vault entry says "rotated" | Pin mock passwords to the site root (`hostBmcPassword`/`dpuBmcPassword`) — site-explorer's documented fallback ("factory failed → sitewide root, no rotation") logs straight in; resets become harmless |
| 15 | Pipeline stalls at preingestion `initial`; manager idle | `waiting_for_explorer_refresh` (set when errors are cleared) gates endpoints out of preingestion and can linger after a healthy report lands (273/300 were parked) | Verification loop unparks endpoints whose reports are clean |
| 16 | Managed hosts identified but machines never created; cycles never finish | `explorations_per_run` was raised to 400 "for throughput" — but identification and creation only run **at the end of a completed explore cycle**, and 400 deep scans per cycle meant cycles stopped completing | Default lowered to 120: cycles complete in ~1–2 min and creation runs every cycle |
| 17 | `Resource pool lo-ip is empty` on the 3rd machine | Machine creation allocates one loopback IP per machine; pool **definitions are seed-once** ("Declaration has drifted since seed … not re-applying") so config widening is ignored; dev6 ships **3** lo-ip addresses | Script inserts free `resource_pool` rows directly for a simulated range (16k) when the pool is smaller than the machine target |

### A note on the verification loop

The script's final phase doesn't just poll — it actively shepherds:
re-clears `AvoidLockout`/`Unauthorized` latches (they are one-way by design;
on real hardware an operator runs `nico-admin-cli site-explorer refresh`) and
unparks healthy endpoints. On a simulation cluster with hundreds of
concurrent resets/explorations, transient races are guaranteed; the loop is
the "operator". Mocks have no lockout threshold, so this is safe here.

## Where we are today

| Stage | Scale | Result |
|-------|-------|--------|
| Baseline | 1 host × 1 DPU (override mode) | ✅ end-to-end: machines created, full credential rotation exercised |
| Stage 1 | 100 hosts × 2 DPUs = 300 BMCs (proxy-direct) | ✅ 300/300 endpoints stable, machines created and advancing through `hostinit`/`dpuinit` |
| Stage 2 | 1000 hosts × 2 DPUs = 3000 BMCs | ✅ **END TO END OK — 3000/3000 machines** in a single unattended script run (~25 min total; creation ≈ 240 machines/min) |
| Stage 3 | 4500 hosts × 2 DPUs = 13,500 BMCs | ✅ **13,500/13,500 machines — 100% fleet** (first run: 13,500 endpoints explored, 10k+ machines; consolidated rerun on the #2764 ClusterIP chart + a freshly provisioned cluster: every counter at 100% — 13,500 explored / 13,500 preingestion-complete / 4,500 hosts / 13,500 machines, ~15 h wall clock unattended incl. connectivity outages) |

Stage-3 observations worth reviewers' attention:

- **The ingestion pipeline is fully autonomous once configured.** Client
  connectivity to the cluster dropped twice for extended periods during
  stage 3; ingestion continued unattended both times (e.g. +720 machines
  through one outage, +4,000 through another). The shepherd loop's latch
  clearing — critical in earlier iterations — was a no-op for the entire
  stage-3 run thanks to pinned credentials.
- **Measured stage-3 rates on dev6 (3 nodes, dev-sized postgres):** DHCP
  ~110 interfaces/min; exploration ~120–360 endpoints/cycle; creation
  40–240 machines per completed explore cycle, sawtoothing with cycle
  phasing (identification rebuilds `explored_managed_hosts` each cycle).
- **Per-MAC Vault credential lifecycle needs batching at scale** (issue 18
  below): site-explorer stores one `machines/bmc/<mac>/root` entry per BMC —
  13,500 entries; deleting them one API round-trip at a time takes hours,
  batched server-side it takes seconds.
- `expected_machines` auto-registration worked at stage 3 (9,890+ registered
  by machine-a-tron via the API), confirming the RBAC grant path.

Additional issue found at stage 3:

| # | Issue | Root cause | Fix |
|---|-------|------------|-----|
| 18 | Stage-2→3 cleanup ran for over an hour "deleting credentials" | One `kubectl exec` per per-MAC Vault deletion × thousands of entries | Batch the deletion loop server-side on the vault pod — one exec total (both cleanup and setup self-heal) |

## Open questions — feedback wanted

1. **RBAC**: is granting `Machineatron` → `AddExpectedMachine` acceptable
   (commit `9a9ba072a`)? Until a nico-api image with it is deployed, the
   script registers expected machines via direct DB insert — okay as a
   documented simulation-only fallback?
1. **Seed-once reconcile semantics**: networks, and resource-pool
   definitions are all create-once; config changes on established sites are
   silently ignored (or warn-only). The script works around this with direct
   DB writes (segment clone-insert, pool row insertion). Should NICo support
   declarative updates for these instead?
1. **AvoidLockout at scale**: one-way latches are right for real BMCs, but
   simulation fleets guarantee latch storms during resets. Worth a
   site-config escape hatch (e.g. `site_explorer.lockout_protection = false`)
   instead of the script's DB-level clearing?
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
