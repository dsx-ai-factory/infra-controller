// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package controller

import (
	"context"
	"fmt"
	"reflect"
	"testing"
	"time"

	apierrors "k8s.io/apimachinery/pkg/api/errors"
	"k8s.io/apimachinery/pkg/apis/meta/v1/unstructured"
	"k8s.io/apimachinery/pkg/runtime"
	"k8s.io/apimachinery/pkg/types"
	"sigs.k8s.io/controller-runtime/pkg/client"
	"sigs.k8s.io/controller-runtime/pkg/client/fake"
	"sigs.k8s.io/controller-runtime/pkg/client/interceptor"

	"github.com/nvidia/infra-controller/dev/k8s/dpf-sim-controller/internal/carbide"
)

// newOperatorConfigKeeper builds a keeper on a fake client that, like the API
// server, drops the status of a created DPFOperatorConfig. Every write is
// appended to writes.
func newOperatorConfigKeeper(t *testing.T, writes *[]string, objs ...client.Object) *OperatorConfigKeeper {
	t.Helper()
	cfg := &unstructured.Unstructured{}
	cfg.SetGroupVersionKind(dpfOperatorConfigGVK)
	c := fake.NewClientBuilder().
		WithScheme(runtime.NewScheme()).
		WithObjects(objs...).
		WithStatusSubresource(cfg).
		WithInterceptorFuncs(interceptor.Funcs{
			Create: func(ctx context.Context, c client.WithWatch, obj client.Object, opts ...client.CreateOption) error {
				*writes = append(*writes, "create")
				unstructured.RemoveNestedField(obj.(*unstructured.Unstructured).Object, "status")
				return c.Create(ctx, obj, opts...)
			},
			Update: func(ctx context.Context, c client.WithWatch, obj client.Object, opts ...client.UpdateOption) error {
				*writes = append(*writes, "update")
				return c.Update(ctx, obj, opts...)
			},
			Patch: func(ctx context.Context, c client.WithWatch, obj client.Object, patch client.Patch, opts ...client.PatchOption) error {
				*writes = append(*writes, "patch")
				return c.Patch(ctx, obj, patch, opts...)
			},
			SubResourcePatch: func(ctx context.Context, c client.Client, subResource string, obj client.Object, patch client.Patch, opts ...client.SubResourcePatchOption) error {
				*writes = append(*writes, "patch "+subResource)
				return c.SubResource(subResource).Patch(ctx, obj, patch, opts...)
			},
		}).
		Build()
	return &OperatorConfigKeeper{Client: c, Namespace: testNamespace}
}

// condition is a status condition of the given type and status with the
// fields the CRD requires.
func condition(conditionType, status string) interface{} {
	return map[string]interface{}{
		"type": conditionType, "status": status, "reason": "Test", "message": "",
		"lastTransitionTime": "2026-01-01T00:00:00Z",
	}
}

// existingOperatorConfig is a DPFOperatorConfig as an installer and an
// operator would leave it, with a spec that differs from the simulator's, the
// given version, and the given status conditions.
func existingOperatorConfig(t *testing.T, version string, conditions ...interface{}) *unstructured.Unstructured {
	t.Helper()
	cfg := newOperatorConfig(testNamespace)
	if err := unstructured.SetNestedField(cfg.Object, int64(9000), "spec", "networking", "highSpeedMTU"); err != nil {
		t.Fatal(err)
	}
	if err := unstructured.SetNestedField(cfg.Object, version, "status", "version"); err != nil {
		t.Fatal(err)
	}
	if len(conditions) > 0 {
		if err := unstructured.SetNestedSlice(cfg.Object, conditions, "status", "conditions"); err != nil {
			t.Fatal(err)
		}
	}
	return cfg
}

func getOperatorConfig(ctx context.Context, k *OperatorConfigKeeper) (*unstructured.Unstructured, error) {
	cfg := &unstructured.Unstructured{}
	cfg.SetGroupVersionKind(dpfOperatorConfigGVK)
	err := k.Get(ctx, types.NamespacedName{Namespace: testNamespace, Name: carbide.DPFOperatorConfigName}, cfg)
	return cfg, err
}

func TestOperatorConfigKeeperEnsure(t *testing.T) {
	for _, tc := range []struct {
		name string
		// existing is the object on the cluster before the check, nil for none.
		existing   *unstructured.Unstructured
		wantWrites []string
		// wantConditions is status.conditions after the check, nil for no field.
		wantConditions []interface{}
	}{
		{
			name:       "absent: created, then versioned through the status subresource",
			wantWrites: []string{"create", "patch status"},
		},
		{
			name:       "present with another version and only a Ready condition: version replaced, conditions field removed",
			existing:   existingOperatorConfig(t, "v26.1.0", condition("Ready", "False")),
			wantWrites: []string{"patch status"},
		},
		{
			name:           "present with the simulator's version, Ready, and another condition: Ready removed, the other kept",
			existing:       existingOperatorConfig(t, simulatedOperatorVersion, condition("Ready", "True"), condition("SystemComponentsReady", "True")),
			wantWrites:     []string{"patch status"},
			wantConditions: []interface{}{condition("SystemComponentsReady", "True")},
		},
		{
			name:     "present with the simulator's version: nothing written",
			existing: existingOperatorConfig(t, simulatedOperatorVersion),
		},
	} {
		t.Run(tc.name, func(t *testing.T) {
			var writes []string
			var objs []client.Object
			wantSpec := newOperatorConfig(testNamespace).Object["spec"]
			if tc.existing != nil {
				objs = append(objs, tc.existing)
				wantSpec = tc.existing.DeepCopy().Object["spec"]
			}
			k := newOperatorConfigKeeper(t, &writes, objs...)
			ctx := context.Background()
			if err := k.ensure(ctx); err != nil {
				t.Fatalf("ensure: %v", err)
			}
			cfg, err := getOperatorConfig(ctx, k)
			if err != nil {
				t.Fatalf("get DPFOperatorConfig: %v", err)
			}
			if got := cfg.Object["spec"]; !reflect.DeepEqual(got, wantSpec) {
				t.Errorf("spec = %v, want %v", got, wantSpec)
			}
			version, _, _ := unstructured.NestedString(cfg.Object, "status", "version")
			if version != simulatedOperatorVersion {
				t.Errorf("status.version = %q, want %q", version, simulatedOperatorVersion)
			}
			if got, _, _ := unstructured.NestedSlice(cfg.Object, "status", "conditions"); !reflect.DeepEqual(got, tc.wantConditions) {
				t.Errorf("status.conditions = %#v, want %#v", got, tc.wantConditions)
			}
			// nico-api reads this object by these literals and parses the
			// version as semver after a leading "v". Releases from 26.8.0
			// select per-DPU BMC credentials, which the simulator lacks.
			if got, want := cfg.GetAPIVersion()+" "+cfg.GetKind()+" "+cfg.GetName(), "operator.dpu.nvidia.com/v1alpha1 DPFOperatorConfig dpfoperatorconfig"; got != want {
				t.Errorf("object = %q, want %q", got, want)
			}
			var major, minor, patch int
			if _, err := fmt.Sscanf(version, "v%d.%d.%d", &major, &minor, &patch); err != nil || fmt.Sprintf("v%d.%d.%d", major, minor, patch) != version {
				t.Errorf("status.version = %q, want vMAJOR.MINOR.PATCH", version)
			} else if major > 26 || (major == 26 && minor >= 8) {
				t.Errorf("status.version = %q, want a release below 26.8.0", version)
			}
			if !reflect.DeepEqual(writes, tc.wantWrites) {
				t.Errorf("writes = %v, want %v", writes, tc.wantWrites)
			}
			// A second check finds nothing to change.
			if err := k.ensure(ctx); err != nil {
				t.Fatalf("second ensure: %v", err)
			}
			if !reflect.DeepEqual(writes, tc.wantWrites) {
				t.Errorf("writes after a second check = %v, want %v", writes, tc.wantWrites)
			}
		})
	}
}

// A deleted DPFOperatorConfig comes back while the keeper runs, and Start
// returns nil when its context ends, so the manager shuts down cleanly.
func TestOperatorConfigKeeperStartRestoresDeletedObject(t *testing.T) {
	var writes []string
	k := newOperatorConfigKeeper(t, &writes)
	k.interval = 10 * time.Millisecond
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	done := make(chan error, 1)
	go func() { done <- k.Start(ctx) }()

	waitForVersion := func() *unstructured.Unstructured {
		t.Helper()
		deadline := time.Now().Add(10 * time.Second)
		for {
			cfg, err := getOperatorConfig(ctx, k)
			if err == nil {
				if v, _, _ := unstructured.NestedString(cfg.Object, "status", "version"); v == simulatedOperatorVersion {
					return cfg
				}
			} else if !apierrors.IsNotFound(err) {
				t.Fatalf("get DPFOperatorConfig: %v", err)
			}
			if time.Now().After(deadline) {
				t.Fatalf("no DPFOperatorConfig with status.version %s after 10s (last error: %v)", simulatedOperatorVersion, err)
			}
			time.Sleep(5 * time.Millisecond)
		}
	}

	if err := k.Delete(ctx, waitForVersion()); err != nil {
		t.Fatalf("delete DPFOperatorConfig: %v", err)
	}
	waitForVersion()

	cancel()
	select {
	case err := <-done:
		if err != nil {
			t.Fatalf("Start returned %v, want nil", err)
		}
	case <-time.After(10 * time.Second):
		t.Fatal("Start did not return after its context was cancelled")
	}
}
