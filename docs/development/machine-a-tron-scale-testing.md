# Scaling NICo with machine-a-tron: 100 → 1000 → 4500 → 10,000 → 20,000 simulated hosts

> **Status:** Scale testing validated with up to 78,952 BMC endpoints (1,112
> GB200 NVL72 racks). Refer to
> [Sizing and Scale Guidance](../getting-started/sizing-and-scale.md) for the
> fleet-level scale table.

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

## Replicating the 250-Rack Fleet

The quick start above installs host-count fleets. The 250-rack measurement in
[Large Site Sizing and Settings](large-site-sizing-and-settings.md) ran ten
machine-a-tron pods of 25 GB200 NVL72 racks each in Controller Mode behind the
protocol gateway. To replicate it on a fresh site:

1. In `helm-prereqs/values.yaml`, set `siteCredentials.enabled: true`
   ([Site Credentials Secret](https://github.com/dsx-ai-factory/infra-controller/blob/main/helm-prereqs/README.md#site-credentials-secret)),
   and raise `postgresql.resources.limits` to the Postgres limits in
   [Settings Changed From the Defaults](large-site-sizing-and-settings.md#settings-changed-from-the-defaults)
   before `setup.sh` runs. Teardown deletes the `postgres` namespace, so a
   resize applied to the live cluster is lost on the next install.
1. Copy `helm-prereqs/values/nico-core-simulation.yaml`, fill in the site
   blanks, and point `nico-api.credentials.file.existingSecret.name` at the
   Secret from step 1. The copy already carries the `[site_explorer]` budget,
   `max_concurrency`, and the nico-api CPU limit from the sizing page. It also
   sets `max_database_connections = 900` and enables the hardware-health rate
   limiter, which keep the connection pool and the Redfish session rate within
   limits at this scale.
1. In the same copy, widen `[networks.simulated-oob]` to a `/17` before
   nico-api first starts, as the sizing page's
   [fleet paragraph](large-site-sizing-and-settings.md#settings-changed-from-the-defaults)
   explains, and point `nico-api.rms.apiUrl` at the protocol gateway
   ([Pointing NICo at It](machine-a-tron-rms-mock.md#pointing-nico-at-it)).
1. Deploy the DOCA Platform Framework (DPF) simulator, then run
   `./setup.sh --skip-dpf --core-values <copy>` from `helm-prereqs/`. The
   overlay header and the
   [dpf-sim-controller quick start](https://github.com/dsx-ai-factory/infra-controller/blob/main/dev/k8s/dpf-sim-controller/README.md#quick-start-against-a-machine-a-tron-cluster)
   give the `make deploy` command and the namespace it creates.
1. Check the rack profile. nico-api derives an expected rack's profile id from
   the expected rack group that declares it: the group topology in upper case,
   an underscore, the compute vendor in upper case, and the suffix
   `_NO_POWERSHELF` when the group declares no power shelf. It replaces the
   requested id with the derived one and rejects the rack unless that id exists
   under `[rack_profiles]`. With
   [PR 6995](https://github.com/dsx-ai-factory/infra-controller/pull/6995),
   machine-a-tron declares one group per rack from its rack type, so
   `rack_profile_id` in the machine-a-tron values must equal the derived id
   (`GB200_NVL72R1_C2G4_WIWYNN` for `wiwynn_gb200_nvl72`,
   `GB300_NVL72R1_C2G4_LENOVO` for `lenovo_gb300_nvl72`), which the nico-api
   chart ships; that PR also updates the deployment page and the 10-rack
   values. On a site installed before it, create the groups with
   `nico-admin-cli expected-rack-group` first. Inspect the declarations with
   `nico-admin-cli expected-rack-group show` and `nico-admin-cli expected-rack show`.
1. Create the labeled `nico-mat` namespace and pull Secret
   ([Cluster Prerequisites](machine-a-tron-deployment.md#cluster-prerequisites)),
   run `helm-prereqs/check-mat-service-cidr.py` against the machine-a-tron
   values with `--site-config` pointing at the copy from step 2, then install
   machine-a-tron from `helm-prereqs/values/machine-a-tron-250racks.yaml`,
   whose per-pod `ids`, `rack_profile_id`, `resources`, and `startupProbe` are
   already sized for 25 racks per pod.
   [Deploying a 250-Rack Site](machine-a-tron-deployment.md#deploying-a-250-rack-site)
   gives the per-pod sizing and the Service count to expect.
1. Leave `max_concurrency` at the overlay's 80 or raise it to 120.
   [Time to Ready Is a Concurrency Setting](large-site-sizing-and-settings.md#time-to-ready-is-a-concurrency-setting)
   gives the measured range and how to set it, and the settings table lists the
   `[api_admission_control]` value used for the simulated fleet.

## 10,000-Host Simulated Fleet

`helm-prereqs/values/machine-a-tron-556racks.yaml` and
`helm-prereqs/values/nico-core-10k-hosts.yaml` define a fleet of 556 GB200
NVL72 racks: 10,008 hosts with 2 data processing units (DPUs) each, 5,004
NVLink switch trays, and 4,448 power shelves, which is 39,476 BMC endpoints
and 30,024 NICo machines. It runs as 23 machine-a-tron pods behind the
protocol gateway with
[per-pod Services](https://github.com/dsx-ai-factory/infra-controller/blob/main/helm/charts/nico-machine-a-tron/README.md#per-pod-services),
two Services per pod instead of one per BMC, so the cluster ServiceCIDR no
longer caps the fleet. The figures in this section come from simulated fleets
of that size on a three-node site controller. Two runs are cited. The earlier
run ran a build of the v2.4 line. The later run ran a build of the v2.5 line,
taken from `main` after the v2.4 release branch was cut, with the scaling
fixes named on this page and
[pull request 8142](https://github.com/dsx-ai-factory/infra-controller/pull/8142)
applied, so the site explorer and the partition monitor it measured behave
differently from a release. Unless a sentence names the earlier run, the
figures are from the later run. Read the figures as expectations rather than
as a reproduction target.
Times count from fleet-up, the point at which every simulator pod is Ready,
the protocol gateway answers, and the controller has published every pod's
Services, so every simulated BMC address is reachable.
[Large Site Sizing and Settings](large-site-sizing-and-settings.md) records
the 250-rack measurements that these settings build on. This section is the
10,000-host fleet definition behind the
[site controller sizing guidance](large-site-sizing-and-settings.md#site-controller-sizing).

### Hardware Baseline

Three control-plane nodes of the class the 250-rack measurement used, 96 cores
and 256 GiB each, carrying NICo, its Postgres, and the simulator. Treat 96
cores and 256 GiB per node as the tested baseline for this fleet. Smaller nodes
were not run. The measured per-node peaks were 43.1 cores (p90 39.8) and 98.9
GiB of working set on the node carrying the Postgres primary and nico-api, and
15.8 and 22.6 cores with 51.7 and 58.2 GiB on the other two nodes. The same
fleet on the earlier run, with nico-api and Postgres at 16 cores each, peaked
at 56 to 75 cores
sustained and 69 to 99 GiB per node. The Postgres primary peaked at 36.9 GiB of
its 96 GiB limit and ran a mean of 14 to 15 of its 32 cores from the third
hour after fleet-up to convergence, with a 26.8-core peak in the first minutes.
nico-api ran its 32 Tokio workers at 49 percent busy over the run and 59 to 68
percent during the ready ramp, peaked at 18.9 cores, and grew to 22.8 GiB of
its 32 GiB limit.

### Commands

Run every command from the repository root with the site kubeconfig and the
image variables exported. Replace each placeholder:

```bash
export KUBECONFIG=<kubeconfig>              # path to the site cluster kubeconfig
export NICO_IMAGE_REGISTRY=<registry>       # registry and repository prefix of the NICo images, without https://
export NICO_CORE_IMAGE_TAG=<core-tag>       # NICo Core image tag
export NICO_REST_IMAGE_TAG=<rest-tag>       # NICo REST image tag from the same main revision as the charts (refer to the caveats)
export REGISTRY_PULL_SECRET=<registry-key>  # registry password or API key, unset for a public or preloaded registry
export REGISTRY_PULL_USERNAME='$oauthtoken'  # registry user for the pull Secrets, $oauthtoken for NGC
export MAT_IMAGE_TAG=<mat-tag>              # machine-a-tron and mat-k8s-controller image tag
export DPF_SIM_IMAGE_TAG=<dpf-sim-tag>      # dpf-sim-controller image tag
```

`setup.sh` creates the `imagepullsecret` Secret that the Core values reference
from `REGISTRY_PULL_SECRET`. The protocol gateway serves the Rack Manager
Service (RMS) mock, so the install skips the RMS chart with `--skip-rms` and
needs no `NICO_RMS_IMAGE_TAG`.

1. Size Postgres and enable the site credentials Secret in
   `helm-prereqs/values.yaml`. `setup.sh` has no flag or environment variable
   for the prerequisites values, and `helmfile.yaml` fixes the `nico-prereqs`
   release to `values.yaml` and `values/nico-prereqs-dynamic.yaml.gotmpl`, so
   edit the file. The Postgres limits apply to each of the three Patroni
   instances:

   ```yaml
   siteCredentials:
     enabled: true
   postgresql:
     resources:
       limits:
         cpu: "32"
         memory: "96Gi"
       requests:
         cpu: "2"
         memory: "4Gi"
   ```

   Teardown deletes the `postgres` namespace, so a resize applied to the live
   cluster is lost on the next install.
1. Copy the Core template and fill in the fields marked `REQUIRED`. The copy
   holds site addresses, so keep it out of version control:

   ```bash
   cp helm-prereqs/values/nico-core-10k-hosts.yaml helm-prereqs/values/nico-core-10k-hosts.site.yaml
   ```

   The template already points
   `nico-api.credentials.file.existingSecret.name` at `nico-site-credentials`,
   the Secret that step 1 renders under its default
   `siteCredentials.secretName`. It carries the `[site_explorer]` budget of
   3200 explorations, 300 hosts, and 480 concurrent explorations per run,
   `max_database_connections = 900`, `max_concurrency = 80`,
   `partition_monitor_max_concurrent_groups = 64`, the nico-api limits of 32
   CPU and 32 GiB with `TOKIO_WORKER_THREADS=32`,
   `apiAdmissionControl.enabled: false`, `nico-api.rms.apiUrl` pointed at the
   gateway, and the simulated segments and pools sized for the fleet:
   `simulated-oob` as a `/16`, `simulated-admin` and `simulated-underlay` as
   a `/17`, 65,534 loopback addresses, and 30,000 autonomous system numbers
   (ASNs). The `/16` is the smallest prefix that holds 39,476 BMC leases. Do
   not widen it further, because the allocator cost grows with the prefix
   ([DHCP Address Space](machine-a-tron-deployment.md#dhcp-address-space)).
   The site TOML also carries `machine_creation_concurrency = 8`,
   `switch_and_power_shelf_pass_interval = "5m"`, and
   `admin_lock_admission_permits = 32` from
   [pull request 8142](https://github.com/dsx-ai-factory/infra-controller/pull/8142).
   A nico-api build without that change logs each of them as an unknown
   configuration key and starts with its own defaults, because
   `deny_unknown_fields` is off by default.
1. Deploy the DOCA Platform Framework (DPF) simulator, then install NICo.
   `setup.sh` resolves `--core-values` against `helm-prereqs/`, so it runs
   from there in a subshell. For a registry that needs credentials, create
   the pull Secret before `make deploy` and keep the `PULL_SECRET` argument.
   Otherwise drop the two `kubectl` commands and the argument. The Secret
   holds only an `auth` entry, the base64 of `user:password`, so a quote or
   backslash in the key cannot break the JSON. It is built from a process
   substitution fed by the shell's `printf` builtin, the pattern `setup.sh`
   uses for its DPF pull Secrets, so the key never appears in a process
   argument. `DPF_NAMESPACE` on the `make` line overrides a namespace the
   Makefile remembers from an earlier deploy:

   ```bash
   kubectl create namespace dpf-operator-system --dry-run=client -o yaml | kubectl apply -f -
   kubectl -n dpf-operator-system create secret generic dpf-sim-pull \
     --type=kubernetes.io/dockerconfigjson \
     --from-file=.dockerconfigjson=<(printf '{"auths":{"%s":{"auth":"%s"}}}' "${NICO_IMAGE_REGISTRY%%/*}" \
       "$(printf '%s:%s' "${REGISTRY_PULL_USERNAME:-\$oauthtoken}" "${REGISTRY_PULL_SECRET}" | base64 | tr -d '\n')") \
     --dry-run=client -o yaml | kubectl apply -f -
   make -C dev/k8s/dpf-sim-controller deploy DPF_NAMESPACE=dpf-operator-system \
     IMG="${NICO_IMAGE_REGISTRY}/dpf-sim-controller:${DPF_SIM_IMAGE_TAG}" PULL_SECRET=dpf-sim-pull
   (cd helm-prereqs && ./setup.sh --skip-dpf --skip-rms --core-values values/nico-core-10k-hosts.site.yaml)
   ```

1. Create the labeled `nico-mat` namespace and its pull Secret, run the
   ServiceCIDR check, and install the fleet. The values file's
   `rack_profile_id`, `GB200_NVL72R1_C2G4_WIWYNN`, is the id nico-api derives
   from the rack group machine-a-tron declares, and the nico-api chart ships
   it. Step 5 of the 250-rack list explains the derivation and the
   `nico-admin-cli expected-rack-group` commands for a site installed before
   machine-a-tron declared its groups. Create `machine-a-tron-pull` in
   `nico-mat` with the Secret command of step 3, changing `-n` and the Secret
   name, or skip it when the registry needs no credentials. The `helm template`
   count prints 0 when the render carries no machine group.

   ```bash
   kubectl create namespace nico-mat --dry-run=client -o yaml | kubectl apply -f -
   kubectl label namespace nico-mat nico.nvidia.com/managed=true --overwrite
   helm template nico-machine-a-tron helm/charts/nico-machine-a-tron \
     -f helm-prereqs/values/machine-a-tron-556racks.yaml | grep -c '^ *\[machines\.'
   python3 helm-prereqs/check-mat-service-cidr.py helm-prereqs/values/machine-a-tron-556racks.yaml \
     --site-config helm-prereqs/values/nico-core-10k-hosts.site.yaml &&
   helm upgrade --install nico-machine-a-tron helm/charts/nico-machine-a-tron \
     -n nico-mat --qps 15 --burst-limit 30 --set createNamespace=false \
     --set image.repository="${NICO_IMAGE_REGISTRY}/machine-a-tron" \
     --set image.tag="${MAT_IMAGE_TAG}" \
     --set mat-k8s-controller.image.repository="${NICO_IMAGE_REGISTRY}/mat-k8s-controller" \
     --set mat-k8s-controller.image.tag="${MAT_IMAGE_TAG}" \
     -f helm-prereqs/values/machine-a-tron-556racks.yaml
   ```

   The controller publishes two Services per pod, 46 in total, once the pods
   hold leases. A device whose Redfish port pair differs from its pod's keeps
   a per-device Service and the controller logs a warning, so a count above 46
   names a misconfigured mock rather than a controller fault:

   ```bash
   kubectl -n nico-mat get svc -l app.kubernetes.io/managed-by=mat-k8s-controller --no-headers | wc -l
   ```

### What to Expect

From `helm upgrade` to fleet-up took 10 to 15 minutes on the 556-rack
simulations, with all 23 pods Ready within 10 minutes. Measured from fleet-up
on the later run:

| Milestone | Time after fleet-up |
|---|---|
| First machines `ready` | 1 h 40 min |
| All power shelves `ready` | 1 h 50 min |
| All 30,024 machines created | 2 h 15 min |
| All switches `ready` | 4 h 15 min |
| First racks `ready` | 4 h 45 min |
| Half of the racks `ready` | 6 h 35 min |
| All 556 racks `ready` | 7 h 55 min |

The per-machine pipeline took 4.4 h at the median from creation to `ready`
(p90 5.2 h, maximum 5.4 h). Machines were 50, 90, and 100 percent `ready` at
5.8, 7.1, and 7.5 h, and racks at 6.6, 7.7, and 7.9 h. Plan for about 8 hours
from fleet-up to convergence with these settings, plus about an hour of
teardown, install, and simulator deploy before fleet-up. The same fleet on the
earlier run, with nico-api and Postgres at 16 cores each, created its last
machine at 2.9 h, took 6 h at the median from creation to `ready`, and
converged at 10.5 h.

### What to Watch

- Postgres primary memory and CPU. The working set rises toward 37 GiB over
  the run, most of it in the last three hours as the nico-api connection pool
  opens toward its 900 cap, and the primary runs at 14 to 15 of its 32 cores
  from the third hour. Both are expected. A non-zero `oom_kill` count is not.
  On the earlier run, at a 16-core limit, the primary was CPU-bound from
  the sixth hour. The command resolves the primary through its
  `spilo-role=master` label and reads cgroup v2 files, which the measured
  nodes expose. On a cgroup v1 node `memory.current` does not exist, so the
  chain stops before the `oom_kill` check:

  ```bash
  kubectl -n postgres exec "$(kubectl -n postgres get pods -l cluster-name=nico-pg-cluster,spilo-role=master -o jsonpath='{.items[0].metadata.name}')" -c postgres -- sh -c \
    'cat /sys/fs/cgroup/memory.current /sys/fs/cgroup/memory.max && grep oom_kill /sys/fs/cgroup/memory.events'
  ```

- Total Postgres connections. The nico-api pool reached 898 of its 900 cap on
  the later run, against the `max_connections = 1024` that helm-prereqs sets
  for the cluster it shares with the REST, Temporal, and Keycloak databases.
  The total across databases was not measured. Check it on the primary during
  the last three hours:

  ```bash
  kubectl -n postgres exec "$(kubectl -n postgres get pods -l cluster-name=nico-pg-cluster,spilo-role=master -o jsonpath='{.items[0].metadata.name}')" -c postgres -- \
    su postgres -c "psql -tAc 'select count(*) from pg_stat_activity'"
  ```

- nico-api restarts. The later run had none. A restart under load points
  at the connection pool or at admission control (refer to the caveats):

  ```bash
  kubectl -n nico-system get pods -l app.kubernetes.io/name=nico-api
  ```

- Explorer and partition monitor iteration time, from the nico-api metrics
  endpoint on port 1080 (`carbide_site_explorer_iteration_latency_milliseconds`
  and `carbide_nvlink_partition_monitor_iteration_latency_milliseconds`, refer
  to [Core Metrics](../observability/core_metrics.md)). The partition monitor
  averaged 10.4 s per iteration over the later run, and its 10 minute window
  means peaked at 24 s as the last racks came `ready`. An iteration longer than
  the 60 s monitor interval is an overrun, and the later run logged none. On
  the earlier run the mean was 17.1 s, the window means reached 42 s, and 8
  overruns occurred during the rack ramp. The maximum iteration time is not
  recorded, because the top finite bucket of the latency histogram is 10 s.

### Caveats

- Admission control stays off for the simulator. The 23 machine-a-tron pods
  present one client key, so the per-client limits of `[api_admission_control]`
  reject the whole fleet at once and the agents retry together, which turns a
  rejection into a retry storm. On this fleet with admission control enabled,
  the global limits raised to 512 requests in flight and 8,192 pending, and
  the per-client limits of 8 and 64 and the 5 s pending timeout at their
  defaults, nico-api rejected 97.4 million of 113.4 million calls in 8 hours,
  86 percent, most of them DPU agent calls. Only 1,386 of 30,024 machines and
  no rack reached `ready`, and the simulator pods restarted 16 times at
  startup on the rejections. Keep
  `apiAdmissionControl.enabled: false` for the simulated fleet, and keep
  `apiAdmissionControl.enabled: true`, the chart default, on any fleet where
  each DPU has its own client identity.
- Give each simulator pod 12 GiB. At a 6 GiB limit, pods of this fleet were
  killed by the out-of-memory (OOM) killer on a prior run while nico-api was
  slow, and a restarted pod discards its mock state and repeats its
  registration, under 10 minutes measured and budgeted at up to 30 minutes by
  the startupProbe.
  `machine-a-tron-556racks.yaml` sets limits of 4 CPU and 12 GiB and requests
  of 1 CPU and 4 GiB per pod.
- Build the REST image from the same `main` revision as the charts. `setup.sh`
  installs the in-tree `helm/rest` charts, and a REST image older than those
  charts can fail to start because the charts pass flags that the older binary
  does not know. With such a tag, `nico-rest-site-manager` crash-looped on an
  unknown flag and `setup.sh` failed phase `[7g/7]` on its Helm wait.
  `NICO_REST_IMAGE_TAG` is `git describe --tags --first-parent --always` of
  that `main` commit with the `-amd64` suffix, the per-architecture tag the
  REST build workflow publishes for every `main` commit (the same name without
  the suffix is the multi-architecture manifest).
- Reset the DPF custom resources (CRs) with the inventory. At this size the
  truncate in [Inventory Reset](machine-a-tron-deployment.md#inventory-reset)
  leaves about 20,000 objects each of `dpudevices` and `dpus` behind. Run the
  collection delete given there until every kind lists no objects. Expect
  about 5 minutes for the four kinds.
- The later run used a DPF simulator image built before
  [pull request 5984](https://github.com/dsx-ai-factory/infra-controller/pull/5984)
  and
  [pull request 6696](https://github.com/dsx-ai-factory/infra-controller/pull/6696),
  so a `dpf-sim-controller` built from `main` is newer than the one measured.
- machine-a-tron's `ScoutStream` calls are denied unless the site TOML sets
  `allow_machineatron_scout_stream = true` under `[auth]`, a simulation-only
  setting that lets the simulator claim any machine ID. The denials count in
  `carbide_auth_denied_total` and do not block convergence. Tracked in
  [issue 7796](https://github.com/dsx-ai-factory/infra-controller/issues/7796).
- The Service count is 46, not the BMC count. The 250-rack check that the
  controller-managed Services approach the BMC count does not apply at pod
  granularity.

## 20,000-Host Simulated Fleet

`helm-prereqs/values/machine-a-tron-1112racks.yaml` and
`helm-prereqs/values/nico-core-20k-hosts.yaml` define a fleet of 1,112 GB200
NVL72 racks: 20,016 hosts with 2 DPUs each, 10,008 NVLink switch trays, and
8,896 power shelves, which is 78,952 BMC endpoints and 60,048 NICo machines,
twice the 556-rack fleet. It runs as 45 machine-a-tron pods behind the
protocol gateway with per-pod Services, 90 Services in total, and its BMC
leases split between two `/16` segments because they exceed one. The figures
in this section come from two completed runs of that fleet on the same
three-node site controller as the 556-rack runs: the first converged 14 h
after fleet-up and the second, three days later with the DPF simulator at a
higher CPU limit and a clean start, 11 h 30 min. Both ran a build of the v2.5
line taken from `main` after the 10,000-host values merged, which carries
[pull request 8142](https://github.com/dsx-ai-factory/infra-controller/pull/8142)
and every scaling fix named on
[Sizing and Scale Guidance](../getting-started/sizing-and-scale.md), all
merged to `main`, with no unmerged Core change. The machine-a-tron and controller images carried
the RMS mock job-retention change of
[issue 8998](https://github.com/dsx-ai-factory/infra-controller/issues/8998),
which is not on `main` yet. Refer to the caveats. Read the figures as
expectations rather than as a reproduction target. Times count from fleet-up,
as defined in the 10,000-host section. This section is the 20,000-host fleet
definition behind the
[site controller sizing table](../getting-started/sizing-and-scale.md#site-controller-sizing-by-fleet-size).

### Hardware Baseline

The same three nodes as the 10,000-host fleet, 96 logical CPUs and 251 GiB of
allocatable memory each, carrying NICo, its Postgres, the 45 simulator pods,
and the DPF simulator. The Core values keep nico-api off the Postgres
primary's node with a required anti-affinity, so the two heaviest components
never share a node. The scheduler placed nico-hardware-health with the
Postgres primary in the first run and on the third node in the second.

Measured from fleet-up to convergence, the nodes averaged 16 to 42 cores. The
first run peaked at 20, 50, and 63 cores and 91, 118, and 142 GiB of working
set per node, with both the 63-core and the 142 GiB peaks on the node that
carried the Postgres primary and hardware-health. The second run peaked at
31, 51, and 52 cores and 97, 121, and 117 GiB, with no node carrying two of
the three heavy components. By component, summed over the nodes:

| Component | Mean cores | Peak cores | Peak working set |
|---|---|---|---|
| nico-api, 64 CPU and 64 GiB limits, 64 Tokio workers | 25 to 26 | 36 | 46 GiB |
| Postgres primary, 48 CPU and 128 GiB limits | 23 to 24 | 33 to 38 | 56 to 68 GiB |
| nico-hardware-health | 6 to 7 | 7 | 17 to 18 GiB |
| 45 simulator pods together | 9 | 13 to 14 | 60 to 76 GiB |
| DPF simulator at 8 CPU | 3.7 | 7.9 | 2 GiB |
| calico-node, three nodes together | 7 | 8 | 1 GiB |
| kube-proxy, three nodes together | 3 | 3.7 | 0.5 GiB |

A converged fleet is not idle for the site controller. Sampled for 52 hours
after the first run converged, with nothing changing in the inventory,
nico-api averaged 35 cores with a 43-core peak and a 51 GiB working set, the
Postgres primary 27 cores with a 40-core peak and 70 GiB, and hardware-health
7 cores and 25 GiB, which kept the nico-api and Postgres nodes at 47 and 46
cores mean. Size a 20,000-host site for that resting load, not only for
ingestion. Smaller nodes were not run.

### Cluster Prerequisites

Two limits of the Kubernetes cluster itself are reached before the fleet
converges. Both are properties of the simulator's Service model, which
publishes every BMC and switch NVOS address as a Service `externalIP`, and
neither applies to a site with real hardware. Apply both changes before the
simulator is deployed.

- **kube-proxy ipset size.** In IP Virtual Server (IPVS) mode kube-proxy records every
  Service `externalIP` and port in the `KUBE-EXTERNAL-IP` ipset on each node,
  and it creates that set with `maxelem 65536`, a value compiled into
  kube-proxy with no flag or configuration field (kube-proxy v1.30 was
  measured, and the upstream request to make it configurable,
  [kubernetes/kubernetes issue 129389](https://github.com/kubernetes/kubernetes/issues/129389),
  was closed without a change). This fleet needs 98,968 entries per node, 89
  per rack: 71 BMC addresses on one port and 9 switch NVOS addresses on two
  ports. A stock kube-proxy logs `Hash is full` once the set is full and
  leaves the remaining addresses unprogrammed on that node, so the same BMC
  answers from one node and refuses connections from another. On the first
  attempt at this fleet machine creation plateaued at 55,110 of 60,048 and no
  rack converged until kube-proxy was rebuilt on the live cluster. The
  arithmetic puts the ceiling of a stock kube-proxy at about 736 GB200 NVL72
  racks with per-pod Services, and the 556-rack fleet fits under it. The measured runs ran a kube-proxy rebuilt from the cluster's
  Kubernetes version with `MaxElem` raised to 1,048,576 and `HashSize` to
  65,536 in `pkg/proxy/ipvs/ipset/ipset.go`, rolled out one node at a time
  with `kube-proxy --cleanup` run on the node before its new pod started, so
  that the set is recreated with the new size. The first full sync after a
  cleanup took 14 to 16 minutes per node at this fleet size. Verify the limit
  and the fill on each node once every lease is out:

  ```bash
  ipset list -t KUBE-EXTERNAL-IP   # Header: maxelem, and "Number of entries"
  ```

  Every node should report 98,968 entries. kube-proxy in iptables or nftables
  mode was not measured.
  [Issue 9003](https://github.com/dsx-ai-factory/infra-controller/issues/9003)
  tracks the documentation of this limit.
- **calico-node probes.** With about 89,000 addresses on each node's
  `kube-ipvs0` interface, the BIRD daemon in calico-node takes longer than
  the default probe timeout of 10 s to answer, the kubelet kills calico-node,
  and that node's data plane and the machine creation that depends on it stop
  until the pod is back. Before the simulator is deployed, switch the
  DaemonSet to `OnDelete`, so that running pods are not restarted by the
  change, raise both probes to a 120 s timeout and 60 failures, then replace
  the calico-node pods one at a time, waiting for each replacement to be
  Ready:

  ```bash
  kubectl -n kube-system patch daemonset calico-node --type=merge \
    -p '{"spec":{"updateStrategy":{"type":"OnDelete","rollingUpdate":null}}}'
  kubectl -n kube-system patch daemonset calico-node --type=json -p='[
    {"op":"replace","path":"/spec/template/spec/containers/0/livenessProbe/timeoutSeconds","value":120},
    {"op":"replace","path":"/spec/template/spec/containers/0/livenessProbe/failureThreshold","value":60},
    {"op":"replace","path":"/spec/template/spec/containers/0/readinessProbe/timeoutSeconds","value":120},
    {"op":"replace","path":"/spec/template/spec/containers/0/readinessProbe/failureThreshold","value":60}]'
  ```

  Container 0 of the DaemonSet is `calico-node` on a kubeadm or Kubespray
  cluster. The measured runs had no calico-node restart with this override.
  Restore the `RollingUpdate` strategy after the run. A re-apply of the
  Calico manifest reverts the probes.
  [Issue 7805](https://github.com/dsx-ai-factory/infra-controller/issues/7805)
  tracks a preflight for it.

### Commands

Export the variables listed in the 10,000-host section and run every command
from the repository root. The steps are those of the 10,000-host fleet with
the 20,000-host values and limits.

1. Size Postgres and enable the site credentials Secret in
   `helm-prereqs/values.yaml`. The database of the 556-rack fleet measured
   4.4 GiB, so the 10Gi default volume leaves no room for twice the rows and
   the write-ahead log. The limits apply to each of the three Patroni
   instances:

   ```yaml
   siteCredentials:
     enabled: true
   postgresql:
     volumeSize: "20Gi"
     resources:
       limits:
         cpu: "48"
         memory: "128Gi"
       requests:
         cpu: "2"
         memory: "4Gi"
   ```

1. Apply the two cluster prerequisites above.
1. Copy the Core template and fill in the fields marked `REQUIRED`. The copy
   holds site addresses, so keep it out of version control:

   ```bash
   cp helm-prereqs/values/nico-core-20k-hosts.yaml helm-prereqs/values/nico-core-20k-hosts.site.yaml
   ```

   The template carries everything the 10,000-host template carries, with
   the `[site_explorer]` budget raised to 4500 explorations, 450 hosts, 680
   concurrent explorations, and `machine_creation_concurrency = 12` per run,
   `max_concurrency = 120`, `partition_monitor_max_concurrent_groups = 96`,
   nico-api limits of 64 CPU and 64 GiB with `TOKIO_WORKER_THREADS=64` and
   a required anti-affinity from the Postgres primary, two BMC segments
   (`simulated-oob` at `10.200.0.0/16` and `simulated-oob-b` at
   `10.202.0.0/16`), `simulated-admin` and `simulated-underlay` as a `/16`
   each, 60,000 ASNs, a 10 s liveness probe timeout for nico-hardware-health,
   and probe and request settings for nico-ssh-console-rs. The 65,534
   loopback addresses stay, with 60,048 used. Two `/16` BMC segments rather
   than one `/15`: a `/15` at `10.200.0.0` would contain `simulated-underlay`,
   and the allocator cost grows with the prefix
   ([DHCP Address Space](machine-a-tron-deployment.md#dhcp-address-space)).
1. Deploy the DPF simulator, raise its limits, then install NICo. The
   simulator ships with limits of 2 CPU and 4Gi, and at 40,032 DPUs it runs at
   its CPU limit through the DPU phase: at 2 CPU the DPF objects of this fleet
   were all created 5 h 26 min after fleet-up, at 8 CPU 5 h 00 min, and at 8
   CPU it still held 7.9 cores through the first five hours, so the limit
   shortens the phase without removing the bottleneck
   ([issue 9002](https://github.com/dsx-ai-factory/infra-controller/issues/9002)).
   Create the pull Secret as in the 10,000-host section when the registry
   needs one:

   ```bash
   kubectl create namespace dpf-operator-system --dry-run=client -o yaml | kubectl apply -f -
   make -C dev/k8s/dpf-sim-controller deploy DPF_NAMESPACE=dpf-operator-system \
     IMG="${NICO_IMAGE_REGISTRY}/dpf-sim-controller:${DPF_SIM_IMAGE_TAG}" PULL_SECRET=dpf-sim-pull
   kubectl -n dpf-operator-system set resources deployment/dpf-sim-controller \
     --requests=cpu=1,memory=2Gi --limits=cpu=8,memory=8Gi
   kubectl -n dpf-operator-system rollout status deployment/dpf-sim-controller
   (cd helm-prereqs && ./setup.sh --skip-dpf --skip-rms --core-values values/nico-core-20k-hosts.site.yaml)
   ```

   After `setup.sh`, confirm that nico-api and the Postgres primary are on
   different nodes. The anti-affinity is checked at scheduling only, so a
   Patroni failover during a run can put the primary on nico-api's node:

   ```bash
   kubectl -n nico-system get pods -l app.kubernetes.io/name=nico-api -o wide
   kubectl -n postgres get pods -l cluster-name=nico-pg-cluster,spilo-role=master -o wide
   ```

1. Create the labeled `nico-mat` namespace and its pull Secret as in the
   10,000-host section, run the ServiceCIDR check against both BMC segments,
   and install the fleet:

   ```bash
   python3 helm-prereqs/check-mat-service-cidr.py helm-prereqs/values/machine-a-tron-1112racks.yaml \
     --site-config helm-prereqs/values/nico-core-20k-hosts.site.yaml &&
   helm upgrade --install nico-machine-a-tron helm/charts/nico-machine-a-tron \
     -n nico-mat --qps 15 --burst-limit 30 --set createNamespace=false \
     --set image.repository="${NICO_IMAGE_REGISTRY}/machine-a-tron" \
     --set image.tag="${MAT_IMAGE_TAG}" \
     --set mat-k8s-controller.image.repository="${NICO_IMAGE_REGISTRY}/mat-k8s-controller" \
     --set mat-k8s-controller.image.tag="${MAT_IMAGE_TAG}" \
     -f helm-prereqs/values/machine-a-tron-1112racks.yaml
   ```

   The check prints the three networks it resolved, `10.200.0.0/16`,
   `10.201.0.0/16`, and `10.202.0.0/16`, and fails when the site config does
   not declare the second BMC segment. The controller publishes two Services
   per pod, 90 in total, once the pods hold leases:

   ```bash
   kubectl -n nico-mat get svc -l app.kubernetes.io/managed-by=mat-k8s-controller --no-headers | wc -l
   ```

### What to Expect

From `helm upgrade` to fleet-up took 16 minutes on the second run, with all
45 pods Ready within 14 minutes. The first run's deploy was interrupted by a
client-side outage and is not a measure. Measured from fleet-up on the second
run, with the first run's figure in parentheses where it differs by more than
a few minutes:

| Milestone | Time after fleet-up |
|---|---|
| First machines created | 20 min |
| First machines `ready` | 1 h 30 min |
| All power shelves `ready` | 3 h 30 min |
| All 60,048 machines created | 4 h 00 min (4 h 15 min) |
| All 40,032 DPF objects created | about 5 h (5 h 26 min) |
| First racks `ready` | 7 h 40 min (5 h 30 min) |
| All switches `ready` | 8 h 50 min |
| Half of the racks `ready` | 9 h 30 min (10 h 45 min) |
| All machines `ready` | 10 h 30 min (13 h 05 min) |
| All 1,112 racks `ready` | 11 h 30 min (14 h 00 min) |

Against the 556-rack fleet, twice the hosts took 2.1 times as long to create
every machine, 2.0 times as long to bring every switch `ready`, and 1.5 to
1.8 times as long to converge. Plan for 12 to 14 hours from fleet-up to
convergence with these settings, plus about an hour and a half of teardown,
install, and simulator deploy before fleet-up. Machine creation ran at 71 to
87 machines a minute averaged over the run, with peaks of 240 to 475 a
minute, and hosts reached `ready` at a steady 25 to 33 a minute after
creation finished.

### What to Watch

The commands of the 10,000-host section apply. The figures to compare against
change as follows.

- nico-api and Postgres primary placement, with the commands above. The two
  must stay on different nodes for the whole run.
- Postgres primary memory and CPU. The working set rose to 56 and 68 GiB of
  the 128 GiB limit on the two runs and to 70 GiB at rest, and the primary ran
  27 to 34 cores from the fourth hour after fleet-up to convergence, with a
  peak of 38. Both are expected. A non-zero `oom_kill` count is not. Expect the
  nico-api connection pool to approach its 900 cap.
- nico-api CPU. 25 to 26 cores mean and 36 at peak during ingestion, then 35
  mean and 43 peak with the fleet converged. nico-api did not restart on
  either run.
- nico-hardware-health restarts. None on either run with the 10 s liveness
  timeout in the Core values. At the chart's 1 s timeout the collector was
  killed 10 times in about 12 hours at this host count.
- Partition monitor pass time. The monitor averaged 17 to 19 s per pass from
  fleet-up to convergence, with 30-minute means up to 30 to 33 s as the racks
  came `ready`, and overran its 60 s interval once in the first run and never
  in the second. Read
  `carbide_nvlink_partition_monitor_iteration_overruns_total` on the nico-api
  metrics endpoint, since the pass-time histogram tops out at 10 s.
- The kube-proxy ipset fill, with the `ipset` command above. Every node must
  hold 98,968 entries once every lease is out. A lower count on one node is
  the cap, and machine creation stalls short of 60,048 when it is hit.
- Restarts of nico-ssh-console-rs and the nico-rest site workers. Both crash
  loop at this machine count for the reasons in the caveats, and neither is on
  the convergence path.

### Caveats

The caveats of the 10,000-host section apply, with these additions.

- machine-a-tron's RMS mock keeps a job's status for one hour on `main`. One
  switch controller pass over 10,008 switches takes about 39 minutes at the
  default switch controller concurrency, and a certificate job needs three
  touches one pass apart, so the mock forgot 48 of about 1,550 certificate
  jobs in flight on the first attempt at this fleet, and the 48 switches stayed
  in `error` with their 48 racks in `discovering`. The switch controller does
  not retry from `error` on its own
  ([issue 5883](https://github.com/dsx-ai-factory/infra-controller/issues/5883)).
  Both completed runs carried the retention change of
  [issue 8998](https://github.com/dsx-ai-factory/infra-controller/issues/8998),
  which keeps jobs for 24 hours, and saw no switch in `error`. Build the
  simulator with that change until it is on `main`.
- nico-ssh-console-rs crash loops at this machine count. Its BMC connection
  pool opens one connection per machine and exhausts the container's 65,535
  file descriptors once about 22,000 machines are `ready`, after which the
  process exits every few minutes
  ([issue 9001](https://github.com/dsx-ai-factory/infra-controller/issues/9001)).
  The probe and request settings in the Core values let it start and stay
  Ready for the first hours instead of being killed before it binds port 22,
  and they do not fix it. The console is not on the convergence path.
- The nico-rest site workers crash loop at their chart default of 1Gi. Their
  inventory sync loads every machine of a page with up to 1,000 pages in
  flight, so the working set tracks the machine count: the 1Gi default is
  exceeded near 1,800 machines, and an 8Gi limit
  (`nico-rest-workflow.siteWorker.resources` in
  `helm-prereqs/values/nico-rest.yaml`) reaches about 35,000 before the
  restarts return
  ([issue 9000](https://github.com/dsx-ai-factory/infra-controller/issues/9000)).
  Sizing by limit does not reach 60,048 machines. The workers are not on the
  convergence path.
- The DPF simulator runs at its CPU limit through the DPU phase at any limit
  measured, 2 and 8 CPU. Its reconcile concurrency is the next lever
  ([issue 9002](https://github.com/dsx-ai-factory/infra-controller/issues/9002)).
- nico-hardware-health's chart default liveness timeout of 1 s kills the
  collector at this host count. The Core values set 10 s, and
  [issue 8999](https://github.com/dsx-ai-factory/infra-controller/issues/8999)
  tracks the chart default.
- Resetting the DPF custom resources leaves tens of thousands of `dpudevices`
  and `dpus` objects behind the inventory truncate. Run the collection
  delete of
  [Inventory Reset](machine-a-tron-deployment.md#inventory-reset) until every
  kind lists no objects.
- The Service count is 90, not the BMC count.

## What this work delivers

1. **Controller Mode** in the `nico-machine-a-tron` chart, with the
   `mat-k8s-controller` for dynamic per-BMC Services, and the scale profiles
   under `helm-prereqs/values/`: `machine-a-tron-scale.yaml`,
   `machine-a-tron-multipod.yaml`, `machine-a-tron-scale-4500.yaml`, and
   `machine-a-tron-scale-4500-proxy.yaml`. `machine-a-tron-10racks.yaml` adds
   the ten-rack GB200 NVL72 example with the Rack Management Service (RMS)
   mock behind the protocol gateway. `machine-a-tron-250racks.yaml`,
   `machine-a-tron-556racks.yaml`, and `machine-a-tron-1112racks.yaml` add the
   250-, 556-, and 1,112-rack fleets.
1. **`helm-prereqs/values/nico-core-simulation.yaml`**, the NICo Core values
   for a simulation site: `allow_insecure_discovery`, the site-explorer
   throughput knobs, the three simulated networks, and the pool sizes.
   `nico-core-10k-hosts.yaml` is the same template sized for the 556-rack
   fleet (a `/16` OOB segment), and `nico-core-20k-hosts.yaml` for the
   1,112-rack fleet (two `/16` OOB segments).
1. **`helm-prereqs/check-mat-service-cidr.py`**, the BMC network versus
   ServiceCIDR preflight, and **`helm-prereqs/ingestion-rate-report.sh`**,
   the per-run ingestion curves from the database timestamps.

## Architecture: Controller Mode

The `mat-k8s-controller` dynamically creates one Service per BMC:

- Discovers machine-a-tron pods via `nvidia-infra-controller/mat-service=true` label
- Polls `/machines/status` from each pod
- Creates Services with the BMC IP (assigned by NICo DHCP) as `externalIPs`
- Services route to correct pod via `nvidia-infra-controller/pod-name` selector
- With `mat-k8s-controller.config.serviceGranularity: pod` it creates two
  Services per pod instead, one carrying every BMC address and one every
  switch NVOS address, so a pod takes two clusterIPs rather than one per
  device. Use it when the fleet's BMC count approaches the ServiceCIDR size.
  Refer to the chart README section Per-pod Services.

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
| 16 | Managed hosts identified but machines never created, and cycles never finish | `explorations_per_run` was raised to 400 "for throughput", but identification and creation only run **at the end of a completed explore cycle**, and 400 deep scans per cycle meant cycles stopped completing | The default was first lowered to 120, so cycles complete in about 1 to 2 min and creation runs every cycle, and later raised to 360. `nico-core-simulation.yaml` ships 2000 with a cycle-completion caveat. Refer to [Large Site Sizing and Settings](large-site-sizing-and-settings.md) for details. |
| 17 | `Resource pool lo-ip is empty` on the 3rd machine | Machine creation allocates one loopback IP per machine. Pool **definitions are seed-once** ("Declaration has drifted since seed ... not re-applying"), so config widening is ignored, and the development site template ships **3** lo-ip addresses | `[pools.lo-ip]` in `nico-core-simulation.yaml` declares 16,382 addresses. Established sites grow the pool with `nico-admin-cli resource-pool grow` |

### A note on the verification loop

The retired setup script's final phase actively shepherded the pipeline. It
re-cleared `AvoidLockout` and `Unauthorized` latches (they are one-way by
design) and unparked healthy endpoints. With pinned BMC passwords the latch
clearing was a no-op for the whole stage-3 run, so the chart path carries no
equivalent. An operator clears a latched endpoint with
`nico-admin-cli site-explorer refresh <bmc-ip>` or
`nico-admin-cli site-explorer clear-error <bmc-ip>`.

## Results by Stage

| Stage | Scale | Result |
|-------|-------|--------|
| Baseline | 1 host × 1 DPU (override mode) | ✅ end-to-end: machines created, full credential rotation exercised |
| Stage 1 | 100 hosts × 2 DPUs = 300 BMCs (proxy-direct) | ✅ 300/300 endpoints stable, machines created and advancing through `hostinit`/`dpuinit` |
| Stage 2 | 1000 hosts × 2 DPUs = 3000 BMCs | ✅ **END TO END OK - 3000/3000 machines** in one unattended run. About 25 min total, creation about 240 machines/min. |
| Stage 3 | 4500 hosts × 2 DPUs = 13,500 BMCs | ✅ **13,500/13,500 machines - 100% fleet.** The first run explored 13,500 endpoints and created over 10,000 machines. A rerun on the ClusterIP chart and a fresh cluster reached every counter: 13,500 explored, 13,500 preingestion-complete, 4,500 hosts, 13,500 machines, about 15 h unattended. |
| Stage 4 | 556 GB200 NVL72 racks = 10,008 hosts × 2 DPUs, 39,476 BMCs (Controller Mode, per-pod Services) | ✅ **30,024/30,024 machines and 556/556 racks ready** 7.9 h after fleet-up on the later run, with no nico-api or simulator restart. The first convergence of this fleet, on the earlier run with nico-api and Postgres at 16 cores each, took 10.5 h. Refer to the section [10,000-Host Simulated Fleet](#10000-host-simulated-fleet) on this page. |
| Stage 5 | 1,112 GB200 NVL72 racks = 20,016 hosts × 2 DPUs, 78,952 BMCs (Controller Mode, per-pod Services, two BMC segments) | ✅ **60,048/60,048 machines and 1,112/1,112 racks ready** 11.5 h after fleet-up on the second run and 14 h on the first, with no nico-api, Postgres, simulator, or gateway restart. Needs the kube-proxy and calico-node changes of the cluster prerequisites. Refer to the section [20,000-Host Simulated Fleet](#20000-host-simulated-fleet) on this page. |

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
