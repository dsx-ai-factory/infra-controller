// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package controller

import (
	"context"
	"errors"
	"testing"

	provisioningv1 "github.com/nvidia/doca-platform/api/provisioning/v1alpha1"
	apierrors "k8s.io/apimachinery/pkg/api/errors"
	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
	"k8s.io/apimachinery/pkg/apis/meta/v1/unstructured"
	"k8s.io/apimachinery/pkg/runtime"
	"k8s.io/apimachinery/pkg/types"
	"sigs.k8s.io/controller-runtime/pkg/client"
	"sigs.k8s.io/controller-runtime/pkg/client/fake"
	"sigs.k8s.io/controller-runtime/pkg/client/interceptor"

	"github.com/nvidia/infra-controller/dev/k8s/dpf-sim-controller/internal/carbide"
)

const testNamespace = "dpf-operator-system"

var (
	testNodeLabels = map[string]string{"dpu-enabled": "true", "deployment-type": "gb200"}
	// testSelector is a DPUSet dpuNodeSelector that matches testNodeLabels.
	testSelector = map[string]string{"dpu-enabled": "true", "deployment-type": "gb200"}
	// otherSelector matches no test node.
	otherSelector = map[string]string{"dpu-enabled": "true", "deployment-type": "bf4"}
)

// newDeployment builds a DPUDeployment (generation 3) whose single DPUSet
// selects DPUNodes carrying selector; dpus holds the other spec.dpus fields.
func newDeployment(t *testing.T, name string, selector map[string]string, dpus map[string]interface{}) *unstructured.Unstructured {
	t.Helper()
	d := &unstructured.Unstructured{}
	d.SetGroupVersionKind(dpuDeploymentGVK)
	d.SetNamespace(testNamespace)
	d.SetName(name)
	d.SetGeneration(3)
	spec := map[string]interface{}{}
	for k, v := range dpus {
		spec[k] = v
	}
	matchLabels := map[string]interface{}{}
	for k, v := range selector {
		matchLabels[k] = v
	}
	spec["dpuSets"] = []interface{}{
		map[string]interface{}{
			"nameSuffix":      "default",
			"dpuNodeSelector": map[string]interface{}{"matchLabels": matchLabels},
		},
	}
	if err := unstructured.SetNestedField(d.Object, spec, "spec", "dpus"); err != nil {
		t.Fatal(err)
	}
	return d
}

func newNode(name string, labels map[string]string) *provisioningv1.DPUNode {
	return &provisioningv1.DPUNode{
		ObjectMeta: metav1.ObjectMeta{Namespace: testNamespace, Name: name, Labels: labels},
	}
}

func newReconciler(t *testing.T, objs ...client.Object) *DPUDeviceReconciler {
	t.Helper()
	s := runtime.NewScheme()
	if err := provisioningv1.AddToScheme(s); err != nil {
		t.Fatal(err)
	}
	dep := &unstructured.Unstructured{}
	dep.SetGroupVersionKind(dpuDeploymentGVK)
	c := fake.NewClientBuilder().
		WithScheme(s).
		WithObjects(objs...).
		WithStatusSubresource(dep, &provisioningv1.DPU{}).
		Build()
	return &DPUDeviceReconciler{Client: c, Scheme: s, Namespace: testNamespace}
}

// recordCreates appends a copy of every object handed to Create, as the
// reconciler sent it. The fake client stores a DPU typed whatever form it was
// created in, so a Get afterwards cannot show which fields were on the wire.
func recordCreates(t *testing.T, r *DPUDeviceReconciler, created *[]client.Object) {
	t.Helper()
	ww, ok := r.Client.(client.WithWatch)
	if !ok {
		t.Fatalf("client %T is not a client.WithWatch", r.Client)
	}
	r.Client = interceptor.NewClient(ww, interceptor.Funcs{
		Create: func(ctx context.Context, c client.WithWatch, obj client.Object, opts ...client.CreateOption) error {
			*created = append(*created, obj.DeepCopyObject().(client.Object))
			return c.Create(ctx, obj, opts...)
		},
	})
}

func getDeployment(t *testing.T, r *DPUDeviceReconciler, name string) *unstructured.Unstructured {
	t.Helper()
	d := &unstructured.Unstructured{}
	d.SetGroupVersionKind(dpuDeploymentGVK)
	if err := r.Get(context.Background(), types.NamespacedName{Namespace: testNamespace, Name: name}, d); err != nil {
		t.Fatalf("get DPUDeployment %s: %v", name, err)
	}
	return d
}

// conditionsByType indexes status.conditions by type, failing on duplicates.
func conditionsByType(t *testing.T, d *unstructured.Unstructured) map[string]map[string]interface{} {
	t.Helper()
	conds, _, err := unstructured.NestedSlice(d.Object, "status", "conditions")
	if err != nil {
		t.Fatal(err)
	}
	out := map[string]map[string]interface{}{}
	for _, raw := range conds {
		c, ok := raw.(map[string]interface{})
		if !ok {
			t.Fatalf("condition %v is %T, want map", raw, raw)
		}
		typ, _ := c["type"].(string)
		if _, dup := out[typ]; dup {
			t.Fatalf("condition %q appears twice: %v", typ, conds)
		}
		out[typ] = c
	}
	return out
}

func TestSelectDeployment(t *testing.T) {
	bfb := map[string]interface{}{"bfb": "bf-bundle", "flavor": "gb200-flavor"}
	bfs := map[string]interface{}{"blueFieldSoftware": "bfs-abc", "flavor": "bf4-flavor"}
	template := map[string]interface{}{"blueFieldSoftware": "bfs-abc", "flavorTemplate": "astra-template"}
	noSource := map[string]interface{}{"flavor": "gb200-flavor"}
	bothSources := map[string]interface{}{"bfb": "bf-bundle", "blueFieldSoftware": "bfs-abc", "flavor": "gb200-flavor"}
	noFlavor := map[string]interface{}{"bfb": "bf-bundle"}

	cases := []struct {
		name        string
		deployments []*unstructured.Unstructured
		want        selectedDeployment
		wantOK      bool
	}{
		{
			name:        "bfb and flavor",
			deployments: []*unstructured.Unstructured{newDeployment(t, "gb200", testSelector, bfb)},
			want:        selectedDeployment{name: "gb200", flavor: "gb200-flavor", bfb: "bf-bundle"},
			wantOK:      true,
		},
		{
			name:        "blueFieldSoftware and flavor",
			deployments: []*unstructured.Unstructured{newDeployment(t, "bf4", testSelector, bfs)},
			want:        selectedDeployment{name: "bf4", flavor: "bf4-flavor", blueFieldSoftware: "bfs-abc"},
			wantOK:      true,
		},
		{
			name:        "blueFieldSoftware and flavorTemplate uses the template name as flavor",
			deployments: []*unstructured.Unstructured{newDeployment(t, "astra", testSelector, template)},
			want:        selectedDeployment{name: "astra", flavor: "astra-template", blueFieldSoftware: "bfs-abc"},
			wantOK:      true,
		},
		{
			name: "two deployments select the node",
			deployments: []*unstructured.Unstructured{
				newDeployment(t, "gb200-a", testSelector, bfb),
				newDeployment(t, "gb200-b", testSelector, bfb),
			},
		},
		{
			name:        "no provisioning source",
			deployments: []*unstructured.Unstructured{newDeployment(t, "gb200", testSelector, noSource)},
		},
		{
			name:        "both provisioning sources",
			deployments: []*unstructured.Unstructured{newDeployment(t, "gb200", testSelector, bothSources)},
		},
		{
			name:        "no flavor",
			deployments: []*unstructured.Unstructured{newDeployment(t, "gb200", testSelector, noFlavor)},
		},
		{
			name: "unusable deployment does not shadow a usable one",
			deployments: []*unstructured.Unstructured{
				newDeployment(t, "broken", testSelector, noSource),
				newDeployment(t, "gb200", testSelector, bfb),
			},
			want:   selectedDeployment{name: "gb200", flavor: "gb200-flavor", bfb: "bf-bundle"},
			wantOK: true,
		},
		{
			name:        "selector does not match the node",
			deployments: []*unstructured.Unstructured{newDeployment(t, "bf4", otherSelector, bfs)},
		},
		{
			name: "no deployments",
		},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			objs := []client.Object{newNode("node-a", testNodeLabels)}
			for _, d := range tc.deployments {
				objs = append(objs, d)
			}
			r := newReconciler(t, objs...)
			got, ok, err := r.selectDeployment(context.Background(), "node-a")
			if err != nil {
				t.Fatalf("selectDeployment: %v", err)
			}
			if ok != tc.wantOK {
				t.Fatalf("ok = %v, want %v (got %+v)", ok, tc.wantOK, got)
			}
			if got != tc.want {
				t.Fatalf("selected = %+v, want %+v", got, tc.want)
			}
			if !ok {
				// Nothing selected, so nothing was marked reconciled.
				for _, d := range tc.deployments {
					if c := conditionsByType(t, getDeployment(t, r, d.GetName()))["DPUSetsReconciled"]; c != nil {
						t.Fatalf("DPUSetsReconciled set on unselected %s: %v", d.GetName(), c)
					}
				}
				return
			}
			// The selected deployment is reported reconciled at its generation.
			d := getDeployment(t, r, got.name)
			c := conditionsByType(t, d)["DPUSetsReconciled"]
			if c == nil || c["status"] != "True" || c["observedGeneration"] != d.GetGeneration() {
				t.Fatalf("DPUSetsReconciled = %v, want True at generation %d", c, d.GetGeneration())
			}
		})
	}
}

func TestMarkDeploymentReadyPreservesOtherConditions(t *testing.T) {
	d := newDeployment(t, "gb200", testSelector, map[string]interface{}{"bfb": "bf-bundle", "flavor": "gb200-flavor"})
	d.SetGeneration(4)
	ready := map[string]interface{}{
		"type": "Ready", "status": "False", "reason": "Pending", "message": "",
		"lastTransitionTime": "2026-01-01T00:00:00Z",
	}
	stale := map[string]interface{}{
		"type": "DPUSetsReconciled", "status": "True", "reason": "Success", "message": "",
		"lastTransitionTime": "2026-01-01T00:00:00Z", "observedGeneration": int64(3),
	}
	if err := unstructured.SetNestedSlice(d.Object, []interface{}{ready, stale}, "status", "conditions"); err != nil {
		t.Fatal(err)
	}
	r := newReconciler(t, d)
	ctx := context.Background()

	if err := r.markDeploymentReady(ctx, getDeployment(t, r, "gb200")); err != nil {
		t.Fatalf("markDeploymentReady: %v", err)
	}
	after := getDeployment(t, r, "gb200")
	conds := conditionsByType(t, after)
	if len(conds) != 2 {
		t.Fatalf("got %d conditions, want Ready and DPUSetsReconciled: %v", len(conds), conds)
	}
	if got := conds["Ready"]; got == nil || got["status"] != "False" || got["reason"] != "Pending" {
		t.Fatalf("Ready condition changed: %v", got)
	}
	if got := conds["DPUSetsReconciled"]; got == nil || got["status"] != "True" || got["observedGeneration"] != int64(4) {
		t.Fatalf("DPUSetsReconciled = %v, want True at generation 4", got)
	}
	if og, _, _ := unstructured.NestedInt64(after.Object, "status", "observedGeneration"); og != 4 {
		t.Fatalf("status.observedGeneration = %d, want 4", og)
	}

	// A current condition is left untouched.
	if err := r.markDeploymentReady(ctx, after); err != nil {
		t.Fatalf("second markDeploymentReady: %v", err)
	}
	if again := getDeployment(t, r, "gb200"); again.GetResourceVersion() != after.GetResourceVersion() {
		t.Fatalf("second call rewrote the deployment: resourceVersion %s -> %s", after.GetResourceVersion(), again.GetResourceVersion())
	}
}

func newDevice() *provisioningv1.DPUDevice {
	return &provisioningv1.DPUDevice{
		ObjectMeta: metav1.ObjectMeta{
			Namespace: testNamespace,
			Name:      carbide.DPUDeviceName("d1"),
			Labels: map[string]string{
				carbide.LabelDPUMachineID:  "machine-1",
				carbide.LabelHostBMCIP:     "10.0.0.1",
				carbide.LabelControlledDev: "true",
			},
		},
		Spec: provisioningv1.DPUDeviceSpec{SerialNumber: "SN1"},
	}
}

func TestEnsureDPUInheritsDeploymentSpec(t *testing.T) {
	cases := []struct {
		name string
		dpus map[string]interface{}
		want provisioningv1.DPUSpec
		// unstructuredCreate: the DPU is sent unstructured with spec.bfb
		// removed, because the pinned typed DPUSpec cannot omit bfb.
		unstructuredCreate bool
	}{
		{
			name: "bfb deployment",
			dpus: map[string]interface{}{"bfb": "bf-bundle", "flavor": "gb200-flavor"},
			want: provisioningv1.DPUSpec{DPUFlavor: "gb200-flavor", BFB: "bf-bundle"},
		},
		{
			name:               "blueFieldSoftware deployment",
			dpus:               map[string]interface{}{"blueFieldSoftware": "bfs-abc", "flavor": "bf4-flavor"},
			want:               provisioningv1.DPUSpec{DPUFlavor: "bf4-flavor", BlueFieldSoftware: "bfs-abc"},
			unstructuredCreate: true,
		},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			nodeName := carbide.DPUNodeName("a")
			device := newDevice()
			r := newReconciler(t, newNode(nodeName, testNodeLabels), device, newDeployment(t, "dep", testSelector, tc.dpus))
			var created []client.Object
			recordCreates(t, r, &created)
			dpuName := carbide.DPUName("a", "d1")

			dpu, err := r.ensureDPU(context.Background(), device, dpuName, nodeName)
			if err != nil {
				t.Fatalf("ensureDPU: %v", err)
			}
			if len(created) != 1 {
				t.Fatalf("Create called %d times, want 1", len(created))
			}
			switch obj := created[0].(type) {
			case *provisioningv1.DPU:
				if tc.unstructuredCreate {
					t.Fatalf("DPU created typed with bfb %q, want unstructured without spec.bfb", obj.Spec.BFB)
				}
			case *unstructured.Unstructured:
				if !tc.unstructuredCreate {
					t.Fatalf("DPU created unstructured, want typed")
				}
				if obj.GroupVersionKind() != provisioningv1.DPUGroupVersionKind {
					t.Fatalf("created GVK = %v", obj.GroupVersionKind())
				}
				if _, has, _ := unstructured.NestedFieldNoCopy(obj.Object, "spec", "bfb"); has {
					t.Fatalf("spec.bfb sent on a blueFieldSoftware DPU: %v", obj.Object["spec"])
				}
				if got, _, _ := unstructured.NestedString(obj.Object, "spec", "blueFieldSoftware"); got != tc.want.BlueFieldSoftware {
					t.Fatalf("sent spec.blueFieldSoftware = %q, want %q", got, tc.want.BlueFieldSoftware)
				}
			default:
				t.Fatalf("Create received %T", obj)
			}
			if dpu.Spec.DPUFlavor != tc.want.DPUFlavor || dpu.Spec.BFB != tc.want.BFB || dpu.Spec.BlueFieldSoftware != tc.want.BlueFieldSoftware {
				t.Fatalf("spec flavor/bfb/blueFieldSoftware = %q/%q/%q, want %q/%q/%q",
					dpu.Spec.DPUFlavor, dpu.Spec.BFB, dpu.Spec.BlueFieldSoftware,
					tc.want.DPUFlavor, tc.want.BFB, tc.want.BlueFieldSoftware)
			}
			if got := dpu.Labels[carbide.LabelOwnedByDPUDeployment]; got != testNamespace+"_dep" {
				t.Fatalf("owned-by label = %q", got)
			}
			if dpu.Status.Phase != provisioningv1.DPUInitializing {
				t.Fatalf("phase = %q, want Initializing", dpu.Status.Phase)
			}
		})
	}
}

func TestEnsureDPUTerminatingMismatchIsRecreating(t *testing.T) {
	nodeName := carbide.DPUNodeName("a")
	dpuName := carbide.DPUName("a", "d1")
	device := newDevice()
	now := metav1.Now()
	existing := &provisioningv1.DPU{
		ObjectMeta: metav1.ObjectMeta{
			Namespace:         testNamespace,
			Name:              dpuName,
			DeletionTimestamp: &now,
			Finalizers:        []string{"test/keep"},
		},
		Spec: provisioningv1.DPUSpec{
			DPUNodeName: nodeName, DPUDeviceName: device.Name, SerialNumber: "SN1",
			DPUFlavor: "sim", BFB: "sim",
		},
	}
	dep := newDeployment(t, "dep", testSelector, map[string]interface{}{"bfb": "bf-bundle", "flavor": "gb200-flavor"})
	r := newReconciler(t, newNode(nodeName, testNodeLabels), device, dep, existing)
	key := types.NamespacedName{Namespace: testNamespace, Name: dpuName}
	var before provisioningv1.DPU
	if err := r.Get(context.Background(), key, &before); err != nil {
		t.Fatalf("get DPU: %v", err)
	}

	_, err := r.ensureDPU(context.Background(), device, dpuName, nodeName)
	if !errors.Is(err, errDPURecreating) {
		t.Fatalf("err = %v, want errDPURecreating", err)
	}
	// The terminating DPU was neither relabeled nor deleted again.
	var got provisioningv1.DPU
	if err := r.Get(context.Background(), key, &got); err != nil {
		t.Fatalf("get DPU: %v", err)
	}
	if _, labeled := got.Labels[carbide.LabelOwnedByDPUDeployment]; labeled {
		t.Fatalf("terminating DPU was relabeled: %v", got.Labels)
	}
	if got.ResourceVersion != before.ResourceVersion {
		t.Fatalf("terminating DPU was written: resourceVersion %s -> %s", before.ResourceVersion, got.ResourceVersion)
	}
}

// A live DPU whose immutable flavor differs from the deployment selecting its
// node is deleted so the next reconcile recreates it.
func TestEnsureDPUFlavorDriftDeletesDPU(t *testing.T) {
	nodeName := carbide.DPUNodeName("a")
	dpuName := carbide.DPUName("a", "d1")
	device := newDevice()
	existing := &provisioningv1.DPU{
		ObjectMeta: metav1.ObjectMeta{Namespace: testNamespace, Name: dpuName},
		Spec: provisioningv1.DPUSpec{
			DPUNodeName: nodeName, DPUDeviceName: device.Name, SerialNumber: "SN1",
			DPUFlavor: "sim", BFB: "sim",
		},
	}
	dep := newDeployment(t, "dep", testSelector, map[string]interface{}{"bfb": "bf-bundle", "flavor": "gb200-flavor"})
	r := newReconciler(t, newNode(nodeName, testNodeLabels), device, dep, existing)

	_, err := r.ensureDPU(context.Background(), device, dpuName, nodeName)
	if !errors.Is(err, errDPURecreating) {
		t.Fatalf("err = %v, want errDPURecreating", err)
	}
	var got provisioningv1.DPU
	err = r.Get(context.Background(), types.NamespacedName{Namespace: testNamespace, Name: dpuName}, &got)
	if !apierrors.IsNotFound(err) {
		t.Fatalf("get DPU after flavor drift: err = %v, want NotFound", err)
	}
}
