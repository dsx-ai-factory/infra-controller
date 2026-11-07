# dpf-sim-controller

A **development-only** simulator for the DOCA Platform Framework (DPF) operator,
built to let machine-a-tron simulated fleets progress past the `dpuinit`
machine state without real BlueField hardware or a real DPF install.

Tracking issue: [dsx-ai-factory/infra-controller#3323](https://github.com/dsx-ai-factory/infra-controller/issues/3323).

## What it does

In a real deployment NICo drives DPU provisioning through DPF like this:

```text
NICo (machine-controller/handler/dpf.rs)          DPF operator
  ── creates DPUDevice + DPUNode CRs ───────────►  watches them
                                                   creates a DPU CR
  watches DPU.status.phase  ◄────────────────────  walks phase:
                                                     Initializing → … → Ready
  when phase == "Rebooting":                        sets Rebooting + reboot annotation
    reboots host via Redfish (→ machine-a-tron)
    calls reboot_complete (clears annotation) ────► resumes → Ready
  when phase == "Ready": machine leaves dpuinit
```

There is **no** real DPF on a machine-a-tron simulation cluster (`dpf.enabled=true`
on every machine, zero DPU CRDs installed), so simulated hosts wedge in
`dpuinit` forever. This controller plays the operator's half:
it watches the `DPUDevice`/`DPUNode` CRs NICo creates and drives a matching
`DPU` CR through the authentic phase sequence, honoring the node-effect hold
and the reboot round-trip that NICo's state machine depends on.

It is a **simulator**, not the operator: no ARM OS boots and no DPU cluster is
joined. By default only the CR status transitions NICo observes are reproduced,
on a configurable per-phase timer. Optionally the simulator also installs the
managed BFB through the DPU BMC, refer to
[Installing the managed BFB](#installing-the-managed-bfb-optional).

## Why Go + reuse from doca-platform

Per the #3323 discussion, this is a controller-runtime job and the CR types
come straight from the upstream module: no hand-written schema, no codegen,
no drift:

```go
import provisioningv1 "github.com/nvidia/doca-platform/api/provisioning/v1alpha1"
```

`github.com/nvidia/doca-platform/api/provisioning/v1alpha1` exports typed
structs + deepcopy + scheme registration for `DPU`, `DPUDevice`, `DPUNode`,
`DPUNodeMaintenance`, `BFB`, `DPUFlavor`, `DPUSet` (group
`provisioning.dpu.nvidia.com`), and the full `DPUPhase` constant set the
simulator walks. The real operator's per-phase logic lives in
`internal/provisioning/controllers/dpu/state/` upstream (one file per phase).
It is not importable (`internal/`), but it is the definitive reference for
entry/exit criteria of each phase.

> **Pin the dependency** to the doca-platform release whose CRDs match the ones
> NICo ships in `crates/dpf/crds/*.yaml`, not `public-main`, so the simulator's
> types and the installed CRDs cannot skew.

## The NICo ⇄ DPF contract this reproduces

Sourced from `crates/dpf/src/sdk.rs` and `crates/machine-controller/src/dpf.rs`:

| Thing | Value | Owner |
|---|---|---|
| DPUNode CR name | `node-{dpf_id}` (`dpf_id` = host BMC MAC, `:`→`-`) | NICo creates |
| DPUDevice CR name | `device-{device_id}` | NICo creates |
| DPU CR name | `node-{dpf_id}-device-{device_id}` | **operator/simulator creates** |
| machine link label | `carbide.nvidia.com/dpu-machine-id` (copy DPUDevice→DPU) | must propagate |
| device marker | `carbide.nvidia.com/controlled.device=true` | on DPUDevice |
| host BMC IP label | `carbide.nvidia.com/host-bmc-ip`: IPv4 uses dotted decimal; IPv6 uses eight four-digit hexadecimal groups separated by hyphens | on device+node |
| primary DPU label | `carbide.nvidia.com/is-primary-dpu` | on DPUDevice |
| node-effect hold | `DPUNodeMaintenance` CR `{node}-hold` carrying annotation `provisioning.dpu.nvidia.com/wait-for-external-nodeeffect` | **sim creates**; NICo patches the annotation to `"false"` to release |
| reboot signal | annotation `provisioning.dpu.nvidia.com/dpunode-external-reboot-required` on the DPUNode | **sim sets**; NICo clears once per node (removes the key) after the host powers back on |
| host BMC IP on the DPU | `DPU.spec.bmcIP` = the **host** BMC IP (bare address), decoded from the `host-bmc-ip` label | sim must populate it or NICo's watcher silently skips the Rebooting callback |

**Phase sequence NICo tolerates** (it collapses intermediates to
`Provisioning(detail)` and only acts on `NodeEffect`/`Rebooting`/`Ready`/`Error`):

```text
Initializing → Node Effect → Pending → Prepare BFB → DPU Config →
Config FW Parameters → Initialize Interface → OS Installing → Rebooting →
DPU Cluster Config → Host Network Configuration → Node Effect Removal → Ready
```

(the authoritative order is `HappyPath` in `internal/simulator/phases.go`,
which follows the `DPUPhase` declaration order in doca-platform v26.4.0;
there is no Update Firmware phase in that release)

## Layout

```text
dpf-sim-controller/
├── PROJECT                          # kubebuilder project marker
├── go.mod
├── Makefile
├── Dockerfile
├── cmd/main.go                      # manager wiring, flags
├── internal/
│   ├── controller/
│   │   └── dpudevice_controller.go  # reconcile DPUDevice → ensure+advance DPU
│   ├── simulator/
│   │   └── phases.go                # phase state machine + dwell timing
│   └── carbide/
│       └── labels.go                # NICo label/annotation/name constants
└── config/
    ├── manager/manager.yaml         # Deployment
    ├── rbac/role.yaml               # least-privilege on DPF CRs
    └── samples/                     # standalone DPUDevice/DPUNode for local test
```

## Independence from the real DPF operator (and setup.sh)

This simulator needs **only the DPF CRDs** (`DPUDevice`, `DPUNode`, `DPU`, and
the CRs NICo references) present on the cluster, never the DPF operator. Those
CRDs ship in this repo at `crates/dpf/crds/`, so `make deploy` applies them
directly. It does **not** depend on the setup.sh DPF-install work: that branch
installs the real operator, which you must **not** run here. The operator and
the simulator would both drive `DPU.status.phase` and fight.

The only coupling is version consistency: the simulator's Go types are pinned to
doca-platform **v26.4.0** (`go.mod`), so the applied CRDs must be v26.4.0
compatible. `crates/dpf/crds/` is that version.

## Quick start (against a machine-a-tron cluster)

Deploy the simulator with `make deploy` below whenever the nico-core site
config has `[dpf] enabled = true`. Two prerequisites are owned elsewhere. The
`nico-api-dpf` Role comes from the `nico-api` chart (`dpf.rbacCreate: true`).
Discovery from a shared pod IP needs `allow_insecure_discovery = true` in the
nico-api site config. Both are set in
`helm-prereqs/values/nico-core-simulation.yaml`.

Never run the simulator beside the real DPF operator: both would drive
`DPU.status.phase`. Check before deploying. The command prints nothing when no
operator is deployed. Otherwise remove the operator first, or leave the
simulator out and accept that ingested hosts park in `dpuinit`:

```bash
kubectl get deploy -n dpf-operator-system -o name | grep -E 'dpf.*operator|operator.*dpf'
```

The manual steps below also serve for iterating on the simulator itself.

**Installing NICo alongside this simulator: use `setup.sh --skip-dpf`.**
The simulator applies the DPF CRDs itself (`make install-crds`), so letting
`setup.sh` also helm-install `dpf-operator` makes Helm try to adopt CRDs it
does not own and the install fails with an `exists and cannot be imported into
the current release: missing key app.kubernetes.io/managed-by` error. This only
shows up on a genuinely clean cluster: an environment that previously ran a
real DPF install has Helm-labelled CRDs already, which masks the collision.

Likewise, do not pre-create the `nico-api-dpf` Role/RoleBinding: the
`nico-api` chart owns those (`dpf.rbacCreate`), and a hand-applied copy blocks
the whole `nico` release the same way.

```bash
export KUBECONFIG=/path/to/site/kubeconfig

# In-cluster (recommended): build + push, then one-command deploy.
# `deploy` installs the CRDs, RBAC and Deployment. Pass PULL_SECRET if the
# nodes need creds to pull IMG (the secret must already exist in the namespace).
make image  IMG=<registry>/dpf-sim-controller:<tag>
make deploy IMG=<registry>/dpf-sim-controller:<tag> \
            DPF_NAMESPACE=dpf-operator-system \
            PULL_SECRET=<existing-pull-secret-name>

# Then bring up machine-a-tron: simulated hosts reach dpuinit, NICo writes
# DPUDevice/DPUNode CRs, the simulator creates DPUs and walks them to Ready.
```

> **Required nico-api setting for ANY machine-a-tron simulation** (#3561, cores
> after `v2.1.0-pr-294`): `DiscoverMachine` resolves callers by TCP source IP,
> and every simulated machine shares the MAT pod's IP, so all agent discovery
> fails with `machine_interface for discovery IP not found: <pod-ip>` and the
> dpuinit walk parks at `waitingfornetworkconfig`. Set the test-only root key
> in the nico-api site config (`nico-api.siteConfig.nicoApiSiteConfig` in the
> Core values, as `helm-prereqs/values/nico-core-simulation.yaml` does) and
> apply it with a Core `helm upgrade`. Do not `kubectl set env` the Deployment:
> the next Core upgrade reverts it.
>
> ```toml
> allow_insecure_discovery = true
> ```

Out-of-cluster alternative (no image needed), useful for local iteration:

```bash
make install-crds
make run DPF_NAMESPACE=dpf-operator-system PHASE_DWELL=3s
```

To exercise the simulator without NICo, apply the standalone sample and watch a
DPU walk to `Ready`:

```bash
kubectl apply -f config/samples/dpudevice_dpunode.yaml
kubectl get dpu -n dpf-operator-system -w
```

The walk parks in `Rebooting` with the reboot annotation set on the DPUNode;
delete that annotation (as NICo's `reboot_complete` would) and the walk
resumes to `Ready`:

```bash
kubectl annotate dpunode node-02-00-00-00-00-01 -n dpf-operator-system \
    provisioning.dpu.nvidia.com/dpunode-external-reboot-required-
```

> **Cleanup:** delete the `DPUDevice` (its `DPU` is garbage-collected via the
> ownerRef), not the `DPU` directly. The simulator recreates the same DPU
> name within milliseconds, so `kubectl delete dpu`, which waits for the name
> to disappear, hangs indefinitely.
>
> **Building on Apple Silicon:** the multi-stage `golang:1.27.3` build segfaults
> under QEMU. `make docker-build` sets `--platform linux/amd64`, which works on
> native amd64 (CI); on an Apple-Silicon laptop, cross-compile first
> (`GOOS=linux GOARCH=amd64 CGO_ENABLED=0 go build -o bin/dpf-sim-controller ./cmd`)
> and build a copy-only image, or build on an amd64 host.

## Installing the managed BFB (optional)

Real DPF writes the managed BFB to the DPU in OS Installing by POSTing
`UpdateService.SimpleUpdate` (target `DPU_OS`) to the DPU BMC's Redfish and
waiting for the task it returns. By default the simulator only dwells in that
phase. A DPU whose OS was reset, for example to the pre-ingestion image by a
decommission, therefore never gets the managed image back on a rebuild.

`--redfish-os-install` (`make deploy REDFISH_OS_INSTALL=true`) makes the
simulator do what the operator does:

1. Save a durable submission marker on the DPU with a resource-version check.
   Then POST `SimpleUpdate` to the DPUDevice's BMC address (`status` if set, else
   `spec`, port default 443). Use HTTPS, omit port 443 and bracket IPv6 addresses,
   for example `https://[2001:db8::5]`. Other ports stay in the URL, for example
   `https://[2001:db8::5]:8443`. Set `ImageURI` to the `spec.url` of the BFB CR
   the DPU references.
2. Prefer the Task resource identified in the response body over a monitor
   Location. Poll until `Completed`, then move to Rebooting. The DPU stays in
   OS Installing meanwhile, and `--os-install-dwell` does not apply.
   A monitor-only response is polled with 202 meaning running and 204 meaning
   completed. A monitor 200 with a `TaskState` uses that task state,
   progress and messages, because `Location` can point to a Task resource.
   An empty or non-string `TaskState` is a polling error that retries until
   the install timeout. Without `TaskState`, monitor 200 means completed and
   its action body is ignored.
   A Task resource's 200 must contain `TaskState`, otherwise
   polling retries until the install timeout. Resource 202/204 handling stays
   running/completed, respectively.
   The `sim.dpu.nvidia.com/bfb-install-task` annotation stores path and kind
   together, for example `{"path":"/monitor/7","kind":"monitor"}`, so a restart
   resumes with the same interpretation. After ignoring leading whitespace,
   values starting with `{` are JSON references and their paths must start
   with `/`. Other values are legacy bare paths interpreted as resources.
   Missing kind also means resource. Invalid JSON references fail `TaskLost`
   without resubmitting. Older controller versions cannot resume the new JSON
   format, so do not downgrade during an install. An old monitor saved as a
   bare path remains interpreted as a resource.
3. A `SimpleUpdate` response other than 202, a task in `Exception`, and
   `--os-install-timeout` (default 45m, the operator's default) move the DPU to
   `Error` with an `OSInstalled=False` condition. A task GET returning 404 with
   a Redfish JSON error body moves the DPU to `Error` immediately with reason
   `TaskLost`, retaining the task annotation for diagnosis without resubmitting
   the install. Before submitting, a missing Secret or BFB CR is retried.
   Once a POST may have been sent, a missing task path or transport error is
   `TaskLost`. A marker without a cached task path triggers an uncached DPU read
   to recover a successfully saved task. If no path was saved, the DPU fails
   without another POST, including after a restart between the marker and POST.
   Plain routing or proxy 404s and unreachable BMCs while polling a known task
   are retried every `--os-install-poll-interval` until `--os-install-timeout`.
   **Known limit:** a monitor that finishes with another 4xx or 5xx also retries
   until that timeout. Only the Redfish error 404 above fails immediately.
4. A DPU created from a `blueFieldSoftware` source has no BFB and is not
   installed.

**It is off by default** because it needs a network path from the simulator pod
to the DPU BMCs. Existing users, whose DPUDevices carry BMC addresses the pod
cannot reach, would otherwise stall in OS Installing until the timeout.

Credentials and TLS follow the operator. The simulator logs in with basic auth
as `--bmc-username` (default `root`) and the `password` key of the
`--bmc-secret-name` Secret (default `bmc-shared-password`) in the DPF
namespace, which NICo writes (`crates/dpf/src/sdk.rs`). It reads that one
Secret directly, not through the cache, and the Role grants `get` on that name
only, so a custom `--bmc-secret-name` needs the name in
`config/rbac/role.yaml` as well. TLS verification is skipped, as in the
operator's basic-auth client: DPU BMCs and machine-a-tron's mock BMCs serve
self-signed certificates. The operator's mTLS client is not reproduced.

**Network path on a machine-a-tron cluster.** machine-a-tron's controller
publishes every mock BMC as a Service whose `externalIPs` is the BMC address
and whose port 443 forwards to the bmc-mock listener
(`dev/k8s/machine-a-tron-controller`, refer to its README). kube-proxy programs
that on every node for pod traffic too, so the simulator pod reaches
`DPUDevice.spec.bmcIp:443` the same way NICo does, with no extra Service,
route or `hostNetwork`. The BMC network must stay outside the ServiceCIDR, the
pod CIDR and the node network, as the machine-a-tron chart already requires.
The mock BMC must accept the shared password and implement SimpleUpdate with a
task, which is what makes the DPU report the managed image.

## Status

Buildable and deployable. `go build` / `go vet` / `go test` pass; `go.mod` pins
doca-platform v26.4.0 with a committed `go.sum`. The reconcile loop, phase
walker, node-effect hold (a `DPUNodeMaintenance` `{node}-hold` CR the simulator
creates and NICo releases), reboot round-trip (node-level bookkeeping written
atomically with the request annotation, so one NICo cycle completes every
rebooting DPU on the node and a partial write can never lose intent), dwell
gating on a phase-entry timestamp (`Owns()` re-enqueues on every status write,
so requeue cadence alone would sprint through all phases), DPU ownerRef/GC,
identity resolution via the parent `DPUNode` (`spec.dpus[]`, exact-name match),
the required `DPU.Spec` fields including the host BMC IP, namespaced
least-privilege RBAC, manifests, and a one-command `make deploy` are all in
place. Verified end-to-end on a live cluster: sample DPU walks each dwell
phase at the configured cadence, parks in `Rebooting`, resumes on annotation
clear, and reaches `Ready` with clean bookkeeping.

Every dwell-gated phase lingers for `--phase-dwell`, except OS Installing,
which lingers for `--os-install-dwell` when that flag is set (default `0`,
meaning the same as `--phase-dwell`). A real BFB install takes minutes while
the config phases take seconds; setting `--os-install-dwell=5m` holds NICo in
its DPF provisioning wait for that long without slowing the rest of the walk.
Rebooting follows OS Installing in `HappyPath`, so the simulator cannot ask
NICo for the host reboot before the install has run its course (#4495).
