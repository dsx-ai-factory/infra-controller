// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package controller

import (
	"context"
	"time"

	apierrors "k8s.io/apimachinery/pkg/api/errors"
	"k8s.io/apimachinery/pkg/apis/meta/v1/unstructured"
	"k8s.io/apimachinery/pkg/runtime/schema"
	"k8s.io/apimachinery/pkg/types"
	"sigs.k8s.io/controller-runtime/pkg/client"
	"sigs.k8s.io/controller-runtime/pkg/log"

	"github.com/nvidia/infra-controller/dev/k8s/dpf-sim-controller/internal/carbide"
)

// simulatedOperatorVersion is the DPF release the simulator reproduces, the
// doca-platform version pinned in go.mod. NICo treats versions from 26.8.0 as
// supporting per-DPU BMC credential Secrets (crates/dpf/src/sdk/version.rs).
// The simulator reads only the shared Secret.
const simulatedOperatorVersion = "v26.4.0"

// operatorConfigInterval is the default time between checks of the
// DPFOperatorConfig and the deadline of each check.
const operatorConfigInterval = 30 * time.Second

// dpfOperatorConfigGVK addresses DPFOperatorConfig as unstructured: the CRD in
// crates/dpf/crds requires spec.deploymentMode, which the pinned
// doca-platform types do not have.
var dpfOperatorConfigGVK = schema.GroupVersionKind{
	Group: "operator.dpu.nvidia.com", Version: "v1alpha1", Kind: "DPFOperatorConfig",
}

// OperatorConfigKeeper keeps the DPFOperatorConfig that NICo reads the
// installed operator version from when it starts. NICo fails startup when the
// object or its status.version is missing. On a real site the installer
// creates the object and the operator writes its status. A simulated site has
// neither.
//
// The keeper creates the object when it is absent and keeps status.version at
// the release the simulator reproduces. It removes any Ready condition from
// the status, including one a real operator left, which keeps NICo's scan for
// outdated DPUs off. It never changes the spec or the other conditions of an
// existing object.
type OperatorConfigKeeper struct {
	client.Client

	// Namespace is the DPF namespace, where NICo reads the object.
	Namespace string
	// interval is the time between checks and the deadline of each check.
	// Zero means operatorConfigInterval.
	interval time.Duration
}

// Start implements manager.Runnable. It checks the object at once and then
// every 30 seconds until ctx is done. A failed check is logged and retried at
// the next interval, so the DPU walk does not depend on it.
func (k *OperatorConfigKeeper) Start(ctx context.Context) error {
	interval := k.interval
	if interval <= 0 {
		interval = operatorConfigInterval
	}
	l := log.FromContext(ctx).WithName("dpfoperatorconfig")
	for {
		attempt, cancel := context.WithTimeout(ctx, interval)
		err := k.ensure(attempt)
		cancel()
		if err != nil && ctx.Err() == nil {
			l.Error(err, "DPFOperatorConfig check failed; retrying",
				"name", carbide.DPFOperatorConfigName, "namespace", k.Namespace, "retryIn", interval)
		}
		select {
		case <-ctx.Done():
			return nil
		case <-time.After(interval):
		}
	}
}

// ensure creates the DPFOperatorConfig when it is absent. One status patch
// sets status.version when it differs from simulatedOperatorVersion and
// removes every Ready condition. Nothing is written when neither applies.
func (k *OperatorConfigKeeper) ensure(ctx context.Context) error {
	l := log.FromContext(ctx).WithName("dpfoperatorconfig")
	cfg := &unstructured.Unstructured{}
	cfg.SetGroupVersionKind(dpfOperatorConfigGVK)
	err := k.Get(ctx, types.NamespacedName{Namespace: k.Namespace, Name: carbide.DPFOperatorConfigName}, cfg)
	if apierrors.IsNotFound(err) {
		cfg = newOperatorConfig(k.Namespace)
		if err = k.Create(ctx, cfg); err == nil {
			l.Info("created DPFOperatorConfig", "name", cfg.GetName(), "namespace", k.Namespace)
		}
	}
	if err != nil {
		return err
	}
	base := cfg.DeepCopy()
	readyRemoved := removeReadyConditions(cfg)
	if version, _, _ := unstructured.NestedString(cfg.Object, "status", "version"); version == simulatedOperatorVersion && !readyRemoved {
		return nil
	}
	if err := unstructured.SetNestedField(cfg.Object, simulatedOperatorVersion, "status", "version"); err != nil {
		return err
	}
	// The CRD has a status subresource, so the API server drops a status sent
	// with the create.
	if err := k.Status().Patch(ctx, cfg, client.MergeFrom(base)); err != nil {
		return err
	}
	l.Info("patched DPFOperatorConfig status", "name", cfg.GetName(), "version", simulatedOperatorVersion, "readyConditionRemoved", readyRemoved)
	return nil
}

// removeReadyConditions removes every condition of type Ready from
// status.conditions, and the field itself when it removed the last condition.
// It reports whether it removed a condition. A conditions value that is not a
// list and an item that is not an object stay as they are.
func removeReadyConditions(cfg *unstructured.Unstructured) bool {
	status, _ := cfg.Object["status"].(map[string]interface{})
	conditions, _ := status["conditions"].([]interface{})
	kept := make([]interface{}, 0, len(conditions))
	for _, condition := range conditions {
		if c, ok := condition.(map[string]interface{}); ok && c["type"] == "Ready" {
			continue
		}
		kept = append(kept, condition)
	}
	if len(kept) == len(conditions) {
		return false
	}
	if len(kept) == 0 {
		delete(status, "conditions")
	} else {
		status["conditions"] = kept
	}
	return true
}

// newOperatorConfig builds the object created when none exists. The spec holds
// only the fields the CRD requires, with the Redfish install interface of
// helm-prereqs/operators/dpf/dpfoperatorconfig.yaml.tmpl, which the CRD allows
// in zero-trust mode only. Nothing reads the spec on a simulated site.
func newOperatorConfig(namespace string) *unstructured.Unstructured {
	cfg := &unstructured.Unstructured{Object: map[string]interface{}{
		"spec": map[string]interface{}{
			"deploymentMode": "zero-trust",
			"provisioningController": map[string]interface{}{
				"installInterface": map[string]interface{}{
					"installViaRedfish": map[string]interface{}{
						"skipDPUNodeDiscovery": true,
					},
				},
			},
		},
	}}
	cfg.SetGroupVersionKind(dpfOperatorConfigGVK)
	cfg.SetNamespace(namespace)
	cfg.SetName(carbide.DPFOperatorConfigName)
	return cfg
}
