// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package controller

import (
	"context"
	"errors"
	"fmt"
	"k8s.io/apimachinery/pkg/apis/meta/v1/unstructured"
	"k8s.io/apimachinery/pkg/runtime/schema"
	"time"

	provisioningv1 "github.com/nvidia/doca-platform/api/provisioning/v1alpha1"
	apierrors "k8s.io/apimachinery/pkg/api/errors"
	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
	"k8s.io/apimachinery/pkg/runtime"
	"k8s.io/apimachinery/pkg/types"
	ctrl "sigs.k8s.io/controller-runtime"
	"sigs.k8s.io/controller-runtime/pkg/client"
	ctrlopts "sigs.k8s.io/controller-runtime/pkg/controller"
	"sigs.k8s.io/controller-runtime/pkg/controller/controllerutil"
	"sigs.k8s.io/controller-runtime/pkg/log"

	"github.com/nvidia/infra-controller/dev/k8s/dpf-sim-controller/internal/carbide"
	"github.com/nvidia/infra-controller/dev/k8s/dpf-sim-controller/internal/simulator"
)

// DPUDeviceReconciler plays the DPF operator's role for simulation. NICo creates
// a DPUDevice (and a parent DPUNode) for every simulated DPU that reaches
// dpuinit; this reconciler ensures a matching DPU CR exists and advances its
// status.phase through the happy path so NICo's machine controller can proceed.
//
// It reconciles on DPUDevice (the CR NICo owns), sets a controller ownerRef on
// each DPU it creates so DPU status writes re-enqueue the parent DPUDevice
// (via Owns) and deleting the DPUDevice garbage-collects its DPU.
type DPUDeviceReconciler struct {
	client.Client
	Scheme *runtime.Scheme

	// Namespace to operate in (the DPF namespace NICo is configured with).
	Namespace string
	// PhaseDwell is how long each dwell-gated phase lingers before advancing.
	PhaseDwell time.Duration
	// Concurrency is the number of parallel reconciles. Reconciles are
	// per-DPUDevice and independent; the only shared writes are the
	// node-level reboot/hold patches, which are idempotent (same-key merge
	// patches, AlreadyExists-tolerant create) and conflict-retried. At fleet
	// scale a single worker serializes tens of thousands of phase writes
	// (4500 hosts x 2 DPUs x ~12 phases) and dominates the walk's wall time.
	Concurrency int
}

// RBAC - least privilege on exactly the DPF CRs the simulator touches. DPU
// cleanup rides the ownerRef GC when the DPUDevice goes away; delete on dpus
// exists only to recreate a DPU whose immutable flavor drifted from the
// deployment selecting its node.
//+kubebuilder:rbac:groups=provisioning.dpu.nvidia.com,resources=dpudevices,verbs=get;list;watch
//+kubebuilder:rbac:groups=provisioning.dpu.nvidia.com,resources=dpunodes,verbs=get;list;watch;update;patch
//+kubebuilder:rbac:groups=provisioning.dpu.nvidia.com,resources=dpunodemaintenances,verbs=get;list;watch;create
//+kubebuilder:rbac:groups=provisioning.dpu.nvidia.com,resources=dpus,verbs=get;list;watch;create;update;patch;delete
//+kubebuilder:rbac:groups=provisioning.dpu.nvidia.com,resources=dpus/status,verbs=get;update;patch
//+kubebuilder:rbac:groups=svc.dpu.nvidia.com,resources=dpudeployments,verbs=get;list;watch
//+kubebuilder:rbac:groups=svc.dpu.nvidia.com,resources=dpudeployments/status,verbs=get;update;patch

// errNoReferencingNode reports that no DPUNode currently lists the DPUDevice —
// the expected transient at creation time (NICo creates the two CRs in either
// order). It is the ONLY resolveNodeID outcome that may take the polling
// requeue path; real list/client failures must surface to controller-runtime.
var errNoReferencingNode = errors.New("no DPUNode references this DPUDevice")

// errDeviceNotReady reports that the DPUDevice is not yet populated with the
// fields the simulator must copy onto the DPU (e.g. the machine-id label).
// Creating the DPU without them would persist a mapping NICo can never use.
var errDeviceNotReady = errors.New("DPUDevice missing required NICo metadata")

func (r *DPUDeviceReconciler) Reconcile(ctx context.Context, req ctrl.Request) (ctrl.Result, error) {
	l := log.FromContext(ctx)

	var device provisioningv1.DPUDevice
	if err := r.Get(ctx, req.NamespacedName, &device); err != nil {
		// DPUDevice gone: NICo tore the machine down. The DPU carries a
		// controller ownerRef to the DPUDevice, so the DPU is garbage-collected
		// automatically — nothing to do here.
		return ctrl.Result{}, client.IgnoreNotFound(err)
	}

	// Resolve the CR names from the NICo naming scheme. deviceID is the
	// DPUDevice's own name minus the "device-" prefix; the node_id comes from
	// the parent DPUNode that references this device (NICo owns both).
	deviceID := carbide.DeviceIDFromDeviceCRName(device.Name)
	nodeID, err := r.resolveNodeID(ctx, &device)
	if err != nil {
		if errors.Is(err, errNoReferencingNode) {
			// The parent DPUNode may not be created yet; requeue and retry.
			l.V(1).Info("parent DPUNode not found yet; requeueing", "device", device.Name)
			return ctrl.Result{RequeueAfter: r.PhaseDwell}, nil
		}
		// List/client failures (RBAC, transport, ...) go to controller-runtime
		// for logged, backed-off retries — never silently polled.
		return ctrl.Result{}, err
	}
	dpuName := carbide.DPUName(nodeID, deviceID)
	nodeName := carbide.DPUNodeName(nodeID)

	// 1. Ensure the DPU CR exists, phase=Initializing, machine-id label copied,
	//    ownerRef set to the DPUDevice.
	dpu, err := r.ensureDPU(ctx, &device, dpuName, nodeName)
	if err != nil {
		if errors.Is(err, errDPURecreating) {
			l.Info("DPU recreated under its DPUDeployment", "dpu", dpuName)
			return ctrl.Result{RequeueAfter: r.PhaseDwell}, nil
		}
		if errors.Is(err, errDeviceNotReady) {
			// NICo has not finished populating the DPUDevice; poll until it has.
			l.V(1).Info("DPUDevice not fully populated yet; requeueing", "device", device.Name, "reason", err.Error())
			return ctrl.Result{RequeueAfter: r.PhaseDwell}, nil
		}
		return ctrl.Result{}, err
	}

	// 2. Terminal? stop ticking. Reboot bookkeeping is node-level and
	//    timestamped (see rebootHandshake), so no per-DPU cleanup is needed
	//    for a future re-walk to start clean.
	if simulator.IsTerminal(dpu.Status.Phase) {
		l.V(1).Info("DPU terminal", "dpu", dpuName, "phase", dpu.Status.Phase)
		return ctrl.Result{}, nil
	}

	// 3. Gate on the current phase. Owns() re-enqueues on every DPU status
	//    write, so reconciles fire much more often than PhaseDwell — dwell
	//    phases are gated on the phase-entry timestamp, never on requeue
	//    cadence.
	switch simulator.Gate(dpu.Status.Phase) {
	case simulator.GateHold:
		held, err := r.maintenanceHoldActive(ctx, nodeName)
		if err != nil {
			return ctrl.Result{}, err
		}
		if held {
			l.V(1).Info("node-effect hold active; parking until NICo releases", "dpu", dpuName)
			return ctrl.Result{RequeueAfter: r.PhaseDwell}, nil
		}

	case simulator.GateReboot:
		// Request one node-level reboot cycle and wait for NICo to complete
		// it. A completed cycle satisfies every DPU that entered Rebooting
		// before its completion stamp (real DPF completes all rebooting DPUs
		// on a node from one annotation cycle).
		rebooted, err := r.rebootHandshake(ctx, nodeName, dpu)
		if err != nil {
			return ctrl.Result{}, err
		}
		if !rebooted {
			l.V(1).Info("waiting for NICo to complete host reboot", "node", nodeName)
			return ctrl.Result{RequeueAfter: r.PhaseDwell}, nil
		}

	case simulator.GateDwell:
		// TODO(#3323): per-phase dwell durations (OS Installing should linger
		// longer than the config phases); today every dwell phase uses the one
		// configured PhaseDwell.
		if entered, err := time.Parse(time.RFC3339, dpu.Annotations[carbide.AnnSimPhaseEnteredAt]); err == nil {
			if remain := r.PhaseDwell - time.Since(entered); remain > 0 {
				return ctrl.Result{RequeueAfter: remain}, nil
			}
		} else {
			// Missing/garbled stamp (pre-existing DPU, manual edit): stamp now
			// and wait one full dwell rather than advancing immediately.
			if err := r.setDPUAnnotation(ctx, dpu, carbide.AnnSimPhaseEnteredAt, time.Now().UTC().Format(time.RFC3339)); err != nil {
				return ctrl.Result{}, err
			}
			return ctrl.Result{RequeueAfter: r.PhaseDwell}, nil
		}
	}

	// 4. Advance to the next phase and write status.
	next, ok := simulator.Next(dpu.Status.Phase)
	if !ok {
		// Unknown/empty phase (e.g. a DPU created outside the happy path, or a
		// status subresource that was never populated). Re-seed to Initializing
		// rather than wedging silently with no requeue.
		l.Info("DPU has unknown phase; re-seeding to Initializing", "dpu", dpuName, "phase", dpu.Status.Phase)
		dpu.Status.Phase = provisioningv1.DPUInitializing
		if err := r.Status().Update(ctx, dpu); err != nil {
			return ctrl.Result{}, err
		}
		return ctrl.Result{RequeueAfter: r.PhaseDwell}, nil
	}
	dpu.Status.Phase = next
	if next == provisioningv1.DPUReady && dpu.Spec.BFB != "" {
		// NICo checks a Ready DPU against its DPUDeployment by comparing the
		// basename of status.bfbFile with "<namespace>-<bfb>.bfb"; a missing
		// or different file is treated as a failed deployment migration.
		dpu.Status.BFBFile = r.bfbFileFor(dpu.Spec.BFB)
	}
	if err := r.Status().Update(ctx, dpu); err != nil {
		if apierrors.IsConflict(err) {
			l.V(1).Info("phase advance conflicted; retrying", "dpu", dpuName, "phase", next)
			return ctrl.Result{Requeue: true}, nil
		}
		return ctrl.Result{}, err
	}
	l.Info("advanced DPU phase", "dpu", dpuName, "phase", next)

	// 5. Bookkeeping for the new phase, now that the advance is durable: stamp
	//    the entry time.
	if err := r.stampPhaseEntry(ctx, dpu); err != nil {
		// Non-fatal: the dwell gate self-heals a missing stamp on the next
		// reconcile by stamping then, at the cost of one extra dwell.
		l.V(1).Info("failed to stamp phase entry; dwell gate will re-stamp", "dpu", dpuName, "err", err.Error())
	}

	return ctrl.Result{RequeueAfter: r.PhaseDwell}, nil
}

// stampPhaseEntry records "now" as the current phase's entry time.
func (r *DPUDeviceReconciler) stampPhaseEntry(ctx context.Context, dpu *provisioningv1.DPU) error {
	patch := client.MergeFrom(dpu.DeepCopy())
	if dpu.Annotations == nil {
		dpu.Annotations = map[string]string{}
	}
	dpu.Annotations[carbide.AnnSimPhaseEnteredAt] = time.Now().UTC().Format(time.RFC3339)
	return r.Patch(ctx, dpu, patch)
}

// resolveNodeID returns the node_id for a DPUDevice by locating the parent
// DPUNode that references it. NICo creates the DPUNode with spec.dpus[].name
// listing its attached devices (crates/dpf/src/sdk.rs register_dpu_node); the
// node_id is that DPUNode's name with the "node-" prefix stripped.
//
// DPURef.Name is documented upstream as "Name of the DPU device"; the exact
// match below accepts either the DPUDevice CR name (device-{id}) or the raw
// device id — nothing broader, so devices sharing a name suffix can never
// bind to the wrong node.
func (r *DPUDeviceReconciler) resolveNodeID(ctx context.Context, device *provisioningv1.DPUDevice) (string, error) {
	var nodes provisioningv1.DPUNodeList
	if err := r.List(ctx, &nodes, client.InNamespace(r.Namespace)); err != nil {
		return "", err
	}
	deviceID := carbide.DeviceIDFromDeviceCRName(device.Name)
	for i := range nodes.Items {
		n := &nodes.Items[i]
		for _, ref := range n.Spec.DPUs {
			if ref.Name == device.Name || ref.Name == deviceID {
				return carbide.NodeIDFromNodeCRName(n.Name), nil
			}
		}
	}
	return "", fmt.Errorf("%w: %q", errNoReferencingNode, device.Name)
}

// ensureDPU creates the DPU CR if absent, seeded at Initializing with the
// machine-id label copied off the DPUDevice (required for NICo reverse lookup)
// and a controller ownerRef to the DPUDevice for GC and Owns() re-enqueue.
// dpuDeploymentGVK addresses DPUDeployment without importing the svc API
// package: the pinned doca-platform module does not ship it, and the simulator
// reads only a few spec.dpus fields and status.conditions from the object.
var dpuDeploymentGVK = schema.GroupVersionKind{
	Group: "svc.dpu.nvidia.com", Version: "v1alpha1", Kind: "DPUDeployment",
}

// selectedDeployment mirrors the DPF DPUSet controller: a DPU belongs to the
// DPUDeployment whose dpuNodeSelector matches its DPUNode's labels. NICo uses
// the same rule when it resolves a node's deployment type to a DPUDeployment,
// so the simulator must land on the same object or NICo waits for a DPU that
// never appears. ok=false means no usable deployment selects the node; the
// legacy "sim" flavor is kept and NICo's name-based lookup still works.
//
// A deployment provisions from exactly one of spec.dpus.bfb or
// spec.dpus.blueFieldSoftware and names exactly one of spec.dpus.flavor or
// spec.dpus.flavorTemplate (both enforced by the DPUDeployment CRD). NICo
// compares a Ready DPU's status.bfbFile against bfb or spec.blueFieldSoftware
// against blueFieldSoftware, and spec.dpuFlavor against flavor. It does not
// compare dpuFlavor for flavorTemplate deployments because real DPF renders the
// template into a per-DPU DPUFlavor; the simulator has nothing to render, so it
// uses the template name as the flavor: non-empty as the DPU CRD requires,
// stable across recreates, and never compared by NICo.
type selectedDeployment struct {
	name, flavor, bfb, blueFieldSoftware string
}

// deploymentFields reads the provisioning source and flavor off a
// DPUDeployment. usable=false means the deployment declares no single source
// or no flavor and must not be selected; err reports a field of the wrong type.
func deploymentFields(d *unstructured.Unstructured) (dep selectedDeployment, usable bool, err error) {
	get := func(field string) (string, error) {
		v, _, err := unstructured.NestedString(d.Object, "spec", "dpus", field)
		if err != nil {
			return "", fmt.Errorf("spec.dpus.%s: %w", field, err)
		}
		return v, nil
	}
	dep.name = d.GetName()
	if dep.bfb, err = get("bfb"); err != nil {
		return dep, false, err
	}
	if dep.blueFieldSoftware, err = get("blueFieldSoftware"); err != nil {
		return dep, false, err
	}
	if dep.flavor, err = get("flavor"); err != nil {
		return dep, false, err
	}
	if dep.flavor == "" {
		if dep.flavor, err = get("flavorTemplate"); err != nil {
			return dep, false, err
		}
	}
	usable = (dep.bfb != "") != (dep.blueFieldSoftware != "") && dep.flavor != ""
	return dep, usable, nil
}

func (r *DPUDeviceReconciler) selectDeployment(ctx context.Context, nodeName string) (selectedDeployment, bool, error) {
	l := log.FromContext(ctx)
	var node provisioningv1.DPUNode
	if err := r.Get(ctx, types.NamespacedName{Namespace: r.Namespace, Name: nodeName}, &node); err != nil {
		return selectedDeployment{}, false, client.IgnoreNotFound(err)
	}
	list := &unstructured.UnstructuredList{}
	list.SetGroupVersionKind(schema.GroupVersionKind{
		Group: dpuDeploymentGVK.Group, Version: dpuDeploymentGVK.Version, Kind: dpuDeploymentGVK.Kind + "List",
	})
	if err := r.List(ctx, list, client.InNamespace(r.Namespace)); err != nil {
		return selectedDeployment{}, false, err
	}
	var matches []selectedDeployment
	var matchIdx []int
	for i := range list.Items {
		d := &list.Items[i]
		sets, _, err := unstructured.NestedSlice(d.Object, "spec", "dpus", "dpuSets")
		if err != nil {
			l.V(1).Info("DPUDeployment spec.dpus.dpuSets has unexpected type; ignoring", "deployment", d.GetName(), "err", err.Error())
			continue
		}
		selects := false
		for _, raw := range sets {
			set, _ := raw.(map[string]interface{})
			ml, _, err := unstructured.NestedStringMap(set, "dpuNodeSelector", "matchLabels")
			if err != nil {
				l.V(1).Info("DPUDeployment dpuNodeSelector.matchLabels has unexpected type; ignoring DPUSet", "deployment", d.GetName(), "err", err.Error())
				continue
			}
			if len(ml) == 0 {
				continue
			}
			all := true
			for k, v := range ml {
				if node.Labels[k] != v {
					all = false
					break
				}
			}
			if all {
				selects = true
				break
			}
		}
		if !selects {
			continue
		}
		dep, usable, err := deploymentFields(d)
		if err != nil {
			l.V(1).Info("DPUDeployment field has unexpected type; ignoring", "deployment", d.GetName(), "err", err.Error())
			continue
		}
		if !usable {
			l.Info("DPUDeployment selects the node but declares no usable provisioning source or flavor; ignoring",
				"deployment", d.GetName(), "node", nodeName)
			continue
		}
		matches = append(matches, dep)
		matchIdx = append(matchIdx, i)
	}
	if len(matches) == 0 {
		return selectedDeployment{}, false, nil
	}
	if len(matches) > 1 {
		names := make([]string, len(matches))
		for i, m := range matches {
			names[i] = m.name
		}
		l.Info("several DPUDeployments select the node; not selecting any", "node", nodeName, "deployments", names)
		return selectedDeployment{}, false, nil
	}
	if err := r.markDeploymentReady(ctx, &list.Items[matchIdx[0]]); err != nil {
		return selectedDeployment{}, false, err
	}
	return matches[0], true, nil
}

// markDeploymentReady reports the DPUDeployment as reconciled, which is what
// the real DPF DPUDeployment controller does once it has created the
// deployment's DPUSets. NICo refuses to look at any DPU under a deployment
// whose DPUSetsReconciled condition is not True at the current generation, so
// without this every host waits forever for a DPU that is already there.
// Idempotent: a current condition is left untouched. Only DPUSetsReconciled is
// replaced; the merge patch carries the whole conditions array, so the other
// conditions are copied through unchanged.
func (r *DPUDeviceReconciler) markDeploymentReady(ctx context.Context, d *unstructured.Unstructured) error {
	gen := d.GetGeneration()
	conds, _, err := unstructured.NestedSlice(d.Object, "status", "conditions")
	if err != nil {
		return fmt.Errorf("DPUDeployment %s status.conditions: %w", d.GetName(), err)
	}
	kept := make([]interface{}, 0, len(conds)+1)
	for _, raw := range conds {
		c, _ := raw.(map[string]interface{})
		if c["type"] != "DPUSetsReconciled" {
			kept = append(kept, raw)
			continue
		}
		if c["status"] == "True" {
			if n, ok := c["observedGeneration"].(int64); ok && n == gen {
				return nil
			}
		}
	}
	patch := d.DeepCopy()
	now := time.Now().UTC().Format(time.RFC3339)
	cond := map[string]interface{}{
		"type":               "DPUSetsReconciled",
		"status":             "True",
		"reason":             "Simulated",
		"message":            "dpf-sim-controller: DPUSets reconciled",
		"lastTransitionTime": now,
		"observedGeneration": gen,
	}
	if err := unstructured.SetNestedSlice(patch.Object, append(kept, cond), "status", "conditions"); err != nil {
		return err
	}
	if err := unstructured.SetNestedField(patch.Object, gen, "status", "observedGeneration"); err != nil {
		return err
	}
	return r.Status().Patch(ctx, patch, client.MergeFrom(d))
}

// ownedByValue is the label value NICo computes: "<namespace>_<deployment>".
func (r *DPUDeviceReconciler) ownedByValue(dep selectedDeployment) string {
	return r.Namespace + "_" + dep.name
}

// bfbFileFor is the installed-image filename NICo compares against a Ready
// DPU: "<namespace>-<bfb CR name>.bfb".
func (r *DPUDeviceReconciler) bfbFileFor(bfb string) string {
	return r.Namespace + "-" + bfb + ".bfb"
}

// errDPURecreating reports that an existing DPU was deleted because its
// immutable spec no longer matches the deployment selecting its node; the
// next reconcile recreates it.
var errDPURecreating = errors.New("DPU deleted for recreation under its DPUDeployment")

func (r *DPUDeviceReconciler) ensureDPU(
	ctx context.Context, device *provisioningv1.DPUDevice, dpuName, nodeName string,
) (*provisioningv1.DPU, error) {
	var dpu provisioningv1.DPU
	err := r.Get(ctx, types.NamespacedName{Namespace: r.Namespace, Name: dpuName}, &dpu)
	if err == nil {
		dep, ok, derr := r.selectDeployment(ctx, nodeName)
		if derr != nil {
			return nil, derr
		}
		if ok {
			// spec.dpuFlavor is immutable on the CRD, so a DPU created before
			// the deployment existed (or under the legacy "sim" flavor) cannot
			// be patched into compliance. Real DPF deletes the source-owned DPU
			// and lets the target deployment recreate it; do the same.
			// A DPU already terminating needs no second Delete, but is still
			// not ours to relabel or advance.
			if dpu.Spec.DPUFlavor != dep.flavor {
				if dpu.DeletionTimestamp == nil {
					if err := r.Delete(ctx, &dpu); err != nil && !apierrors.IsNotFound(err) {
						return nil, err
					}
				}
				return nil, errDPURecreating
			}
			wantCtl := device.Labels[carbide.LabelControlledDev]
			if dpu.Labels[carbide.LabelOwnedByDPUDeployment] != r.ownedByValue(dep) ||
				(wantCtl != "" && dpu.Labels[carbide.LabelControlledDev] != wantCtl) {
				patch := client.MergeFrom(dpu.DeepCopy())
				if dpu.Labels == nil {
					dpu.Labels = map[string]string{}
				}
				dpu.Labels[carbide.LabelOwnedByDPUDeployment] = r.ownedByValue(dep)
				if wantCtl != "" {
					dpu.Labels[carbide.LabelControlledDev] = wantCtl
				}
				if err := r.Patch(ctx, &dpu, patch); err != nil {
					return nil, err
				}
			}
		}
		return &dpu, nil
	}
	if !apierrors.IsNotFound(err) {
		return nil, err
	}

	// NICo maps DPU events back to a machine by the machine-id label, and its
	// Rebooting-phase watcher reads the HOST BMC IP off DPU.spec.bmcIP —
	// creating the DPU with either one empty persists a mapping NICo can
	// never use (a missing/invalid bmcIP silently skips the reboot callback,
	// crates/dpf/src/watcher.rs). Wait for NICo to finish populating the
	// DPUDevice instead.
	if device.Labels[carbide.LabelDPUMachineID] == "" {
		return nil, fmt.Errorf("%w: label %s is empty on %s", errDeviceNotReady, carbide.LabelDPUMachineID, device.Name)
	}
	if device.Labels[carbide.LabelHostBMCIP] == "" {
		return nil, fmt.Errorf("%w: label %s is empty on %s", errDeviceNotReady, carbide.LabelHostBMCIP, device.Name)
	}

	// Fallback when no usable deployment selects the node: the CRD requires
	// dpuFlavor and exactly one of bfb/blueFieldSoftware, none of which mean
	// anything to the simulator, so placeholder values keep the create accepted.
	flavor, bfb, blueFieldSoftware := "sim", "sim", ""
	labels := map[string]string{
		// MUST propagate: NICo maps DPU events back to a machine by this.
		carbide.LabelDPUMachineID: device.Labels[carbide.LabelDPUMachineID],
		carbide.LabelHostBMCIP:    device.Labels[carbide.LabelHostBMCIP],
	}
	// NICo scopes its DPU watch to controlled devices; the real DPUSet
	// controller carries this label from the DPUDevice onto the DPU.
	if v := device.Labels[carbide.LabelControlledDev]; v != "" {
		labels[carbide.LabelControlledDev] = v
	}
	if dep, ok, derr := r.selectDeployment(ctx, nodeName); derr != nil {
		return nil, derr
	} else if ok {
		flavor, bfb, blueFieldSoftware = dep.flavor, dep.bfb, dep.blueFieldSoftware
		labels[carbide.LabelOwnedByDPUDeployment] = r.ownedByValue(dep)
	}
	noEffect := true
	dpu = provisioningv1.DPU{
		ObjectMeta: metav1.ObjectMeta{
			Name:      dpuName,
			Namespace: r.Namespace,
			Labels:    labels,
			Annotations: map[string]string{
				carbide.AnnSimPhaseEnteredAt: time.Now().UTC().Format(time.RFC3339),
			},
		},
		Spec: provisioningv1.DPUSpec{
			DPUNodeName:   nodeName,
			DPUDeviceName: device.Name,
			SerialNumber:  device.Spec.SerialNumber,
			// The HOST BMC IP (bare address, no port): NICo's watcher parses
			// this into the RebootRequiredEvent that enqueues the host state
			// machine when the DPU reaches Rebooting. NOT the DPU's own BMC
			// (DPUDevice.spec.bmcIp) — NICo publishes the host's on this label.
			BMCIP: device.Labels[carbide.LabelHostBMCIP],
			// Inherited from the selecting deployment (see selectedDeployment)
			// or the fallback above. The pinned doca-platform DPUSpec has no
			// omitempty on bfb, so a blueFieldSoftware-only DPU is created
			// below as unstructured with spec.bfb removed.
			DPUFlavor:         flavor,
			BFB:               bfb,
			BlueFieldSoftware: blueFieldSoftware,
			// NoEffect: the simulator never touches real K8s node taints/drains.
			// NodeEffect embeds Action; NoEffect lives on Action, not NodeEffect directly.
			NodeEffect: provisioningv1.NodeEffect{
				Action: provisioningv1.Action{NoEffect: &noEffect},
			},
		},
	}
	// blockOwnerDeletion=false: the default (true) requires update rights on
	// the owner's finalizers, which this least-privilege ServiceAccount does
	// not have — the admission plugin would reject the create. GC of the DPU
	// on DPUDevice deletion works the same either way.
	if err := controllerutil.SetControllerReference(device, &dpu, r.Scheme,
		controllerutil.WithBlockOwnerDeletion(false)); err != nil {
		return nil, err
	}
	if blueFieldSoftware != "" && bfb == "" {
		// Unstructured only because the pinned type cannot omit bfb: the CRD
		// rejects an empty bfb next to blueFieldSoftware.
		obj, err := runtime.DefaultUnstructuredConverter.ToUnstructured(&dpu)
		if err != nil {
			return nil, err
		}
		unstructured.RemoveNestedField(obj, "spec", "bfb")
		u := &unstructured.Unstructured{Object: obj}
		u.SetGroupVersionKind(provisioningv1.DPUGroupVersionKind)
		if err := r.Create(ctx, u); err != nil {
			return nil, err
		}
		if err := runtime.DefaultUnstructuredConverter.FromUnstructured(u.Object, &dpu); err != nil {
			return nil, err
		}
	} else if err := r.Create(ctx, &dpu); err != nil {
		return nil, err
	}
	// Create does not persist the status subresource, so write the initial
	// phase explicitly rather than relying on the in-memory value.
	dpu.Status.Phase = provisioningv1.DPUInitializing
	if err := r.Status().Update(ctx, &dpu); err != nil {
		return nil, err
	}
	return &dpu, nil
}

// rebootHandshake drives one node-level reboot cycle and reports whether this
// DPU's reboot need is satisfied. The reboot is a property of the DPUNode —
// NICo clears AnnRebootRequired by REMOVING it, once per node, after the host
// powers back on, and real DPF completes every rebooting DPU on the node from
// that single annotation cycle — so all bookkeeping lives on the DPUNode:
//
//   - request: AnnRebootRequired + AnnSimNodeRebootRequestedAt written in ONE
//     patch (intent can never be recorded without its marker, so a partial
//     write cannot lose it);
//   - completion: RequestedAt present with AnnRebootRequired gone means NICo
//     finished the cycle; CompletedAt is stamped and RequestedAt dropped in
//     one patch;
//   - satisfaction: a DPU is done iff a cycle completed AFTER it entered the
//     Rebooting phase (its AnnSimPhaseEnteredAt). One cycle therefore
//     releases every DPU already rebooting, while a DPU that enters
//     Rebooting later requests a fresh cycle — no duplicate host reboots for
//     the same walk, matching real hardware semantics.
//
// Everything here is idempotent, so a conflicted phase advance simply re-runs
// the satisfaction check on the next reconcile.
func (r *DPUDeviceReconciler) rebootHandshake(
	ctx context.Context, nodeName string, dpu *provisioningv1.DPU,
) (bool, error) {
	var node provisioningv1.DPUNode
	if err := r.Get(ctx, types.NamespacedName{Namespace: r.Namespace, Name: nodeName}, &node); err != nil {
		return false, client.IgnoreNotFound(err)
	}

	enteredAt, err := time.Parse(time.RFC3339, dpu.Annotations[carbide.AnnSimPhaseEnteredAt])
	if err != nil {
		// No usable entry stamp (manual edit, pre-existing CR): stamp now and
		// evaluate against it next reconcile rather than guessing.
		return false, r.stampPhaseEntry(ctx, dpu)
	}

	if completedAt, err := time.Parse(time.RFC3339, node.Annotations[carbide.AnnSimNodeRebootCompletedAt]); err == nil {
		if completedAt.After(enteredAt) {
			return true, nil // a cycle completed while this DPU was rebooting
		}
	}

	_, requested := node.Annotations[carbide.AnnSimNodeRebootRequestedAt]
	// NICo gates on presence, not value (crates/dpf/src/sdk.rs is_reboot_required).
	_, pending := node.Annotations[carbide.AnnRebootRequired]

	switch {
	case pending:
		return false, nil // cycle in flight; NICo has not finished the reboot
	case requested:
		// Requested and NICo has since removed AnnRebootRequired → the cycle
		// finished. Record its completion (one patch), then let the next
		// reconcile observe completedAt > enteredAt and advance.
		patch := client.MergeFrom(node.DeepCopy())
		node.Annotations[carbide.AnnSimNodeRebootCompletedAt] = time.Now().UTC().Format(time.RFC3339)
		delete(node.Annotations, carbide.AnnSimNodeRebootRequestedAt)
		return true, r.Patch(ctx, &node, patch)
	default:
		// No cycle satisfies this DPU and none is in flight: request one.
		patch := client.MergeFrom(node.DeepCopy())
		if node.Annotations == nil {
			node.Annotations = map[string]string{}
		}
		node.Annotations[carbide.AnnRebootRequired] = "true"
		node.Annotations[carbide.AnnSimNodeRebootRequestedAt] = time.Now().UTC().Format(time.RFC3339)
		return false, r.Patch(ctx, &node, patch)
	}
}

// maintenanceHoldActive reproduces the DPF operator's side of the node-effect
// hold. The handshake lives on a DPUNodeMaintenance CR named "<node>-hold"
// that the OPERATOR creates carrying the wait-for-external-nodeeffect
// annotation; NICo's release_maintenance_hold patches that annotation to the
// literal "false" (never deleting the CR, no-op'ing on 404). The simulator
// creates the CR on first entry and parks the DPU until NICo's release lands.
func (r *DPUDeviceReconciler) maintenanceHoldActive(ctx context.Context, nodeName string) (bool, error) {
	holdName := carbide.MaintenanceHoldName(nodeName)
	var m provisioningv1.DPUNodeMaintenance
	err := r.Get(ctx, types.NamespacedName{Namespace: r.Namespace, Name: holdName}, &m)
	if apierrors.IsNotFound(err) {
		m = provisioningv1.DPUNodeMaintenance{
			ObjectMeta: metav1.ObjectMeta{
				Name:      holdName,
				Namespace: r.Namespace,
				Annotations: map[string]string{
					carbide.AnnWaitForExternalNodeEffect: "true",
				},
			},
			Spec: provisioningv1.DPUNodeMaintenanceSpec{
				DPUNodeName: nodeName,
			},
		}
		if err := r.Create(ctx, &m); err != nil && !apierrors.IsAlreadyExists(err) {
			return false, err
		}
		return true, nil
	}
	if err != nil {
		return false, err
	}
	// NICo releases by writing the literal string "false"; treat an absent
	// key the same (a pre-released hold from an earlier walk).
	v, ok := m.Annotations[carbide.AnnWaitForExternalNodeEffect]
	return ok && v != "false", nil
}

func (r *DPUDeviceReconciler) setDPUAnnotation(ctx context.Context, dpu *provisioningv1.DPU, key, val string) error {
	patch := client.MergeFrom(dpu.DeepCopy())
	if dpu.Annotations == nil {
		dpu.Annotations = map[string]string{}
	}
	if val == "" {
		delete(dpu.Annotations, key)
	} else {
		dpu.Annotations[key] = val
	}
	return r.Patch(ctx, dpu, patch)
}

// SetupWithManager wires the reconciler: own DPU, watch DPUDevice as primary.
func (r *DPUDeviceReconciler) SetupWithManager(mgr ctrl.Manager) error {
	if r.PhaseDwell == 0 {
		r.PhaseDwell = 3 * time.Second
	}
	if r.Concurrency <= 0 {
		r.Concurrency = 16
	}
	return ctrl.NewControllerManagedBy(mgr).
		For(&provisioningv1.DPUDevice{}).
		Owns(&provisioningv1.DPU{}).
		WithOptions(ctrlopts.Options{MaxConcurrentReconciles: r.Concurrency}).
		Complete(r)
}
