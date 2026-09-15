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

It is a **simulator**, not the operator: no BFB is flashed, no ARM OS boots,
no DPU cluster is joined. Only the CR status transitions NICo observes are
reproduced, on a configurable per-phase timer.

## Why Go + reuse from doca-platform

Per the #3323 discussion, this is a controller-runtime job and the CR types
come straight from the upstream module — no hand-written schema, no codegen,
no drift:

```go
import provisioningv1 "github.com/nvidia/doca-platform/api/provisioning/v1alpha1"
```

`github.com/nvidia/doca-platform/api/provisioning/v1alpha1` exports typed
structs + deepcopy + scheme registration for `DPU`, `DPUDevice`, `DPUNode`,
`DPUNodeMaintenance`, `BFB`, `DPUFlavor`, `DPUSet` (group
`provisioning.dpu.nvidia.com`), and the full `DPUPhase` constant set the
simulator walks. The real operator's per-phase logic lives in
`internal/provisioning/controllers/dpu/state/` upstream (one file per phase) —
not importable (`internal/`), but the definitive reference for entry/exit
criteria of each phase.

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
| host BMC IP label | `carbide.nvidia.com/host-bmc-ip` | on device+node |
| primary DPU label | `carbide.nvidia.com/is-primary-dpu` | on DPUDevice |
| node-effect hold | `DPUNodeMaintenance` CR `{node}-hold` carrying annotation `provisioning.dpu.nvidia.com/wait-for-external-nodeeffect` | **sim creates**; NICo patches the annotation to `"false"` to release |
| reboot signal | annotation `provisioning.dpu.nvidia.com/dpunode-external-reboot-required` on the DPUNode | **sim sets**; NICo clears once per node (removes the key) after the host powers back on |
| host BMC IP on the DPU | `DPU.spec.bmcIP` = the **host** BMC IP (bare address), copied from the `host-bmc-ip` label | sim must populate it or NICo's watcher silently skips the Rebooting callback |

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
the CRs NICo references) present on the cluster — never the DPF operator. Those
CRDs ship in this repo at `crates/dpf/crds/`, so `make deploy` applies them
directly. It does **not** depend on the setup.sh DPF-install work: that branch
installs the real operator, which you must **not** run here — the operator and
the simulator would both drive `DPU.status.phase` and fight.

The only coupling is version consistency: the simulator's Go types are pinned to
doca-platform **v26.4.0** (`go.mod`), so the applied CRDs must be v26.4.0
compatible. `crates/dpf/crds/` is that version.

## Quick start (against a machine-a-tron cluster)

**The usual path is automatic:** `helm-prereqs/setup-machine-a-tron.sh`
deploys this simulator by default (Phase 4b) whenever the nico-core config
has `[dpf] enabled = true`, using the `DPF_SIM_IMAGE`
(or `${NICO_IMAGE_REGISTRY}/dpf-sim-controller:latest`). This includes the
DPF CRDs, the `nico-api-dpf` RBAC, and the `CARBIDE_API_ALLOW_INSECURE_DISCOVERY`
flag.

On a site without DPF enabled, the phase is a no-op, and it hard-fails if
the REAL DPF operator is deployed (both would drive `DPU.status.phase` —
remove the operator, or pass `--skip-dpf-sim` to keep it). The manual
steps below remain for iterating on the simulator itself.

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
> dpuinit walk parks at `waitingfornetworkconfig`. Set the sanctioned test-env
> escape hatch on the nico-api deployment:
>
> ```bash
> kubectl -n nico-system set env deployment/nico-api \
>     CARBIDE_API_ALLOW_INSECURE_DISCOVERY=true
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
> ownerRef), not the `DPU` directly — the simulator recreates the same DPU
> name within milliseconds, so `kubectl delete dpu`, which waits for the name
> to disappear, hangs indefinitely.
>
> **Building on Apple Silicon:** the multi-stage `golang:1.25` build segfaults
> under QEMU. `make docker-build` sets `--platform linux/amd64`, which works on
> native amd64 (CI); on an Apple-Silicon laptop, cross-compile first
> (`GOOS=linux GOARCH=amd64 CGO_ENABLED=0 go build -o bin/dpf-sim-controller ./cmd`)
> and build a copy-only image, or build on an amd64 host.

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

The remaining `TODO(#3323)` marker flags a fidelity decision, not a blocker:
per-phase dwell durations (OS Installing should linger longer than the config
phases).
