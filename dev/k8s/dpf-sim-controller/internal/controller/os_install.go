// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package controller

import (
	"context"
	"errors"
	"fmt"
	"strings"
	"sync"
	"time"

	provisioningv1 "github.com/nvidia/doca-platform/api/provisioning/v1alpha1"
	corev1 "k8s.io/api/core/v1"
	"k8s.io/apimachinery/pkg/api/meta"
	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
	"k8s.io/apimachinery/pkg/types"
	ctrl "sigs.k8s.io/controller-runtime"
	"sigs.k8s.io/controller-runtime/pkg/client"
	"sigs.k8s.io/controller-runtime/pkg/log"

	"github.com/nvidia/infra-controller/dev/k8s/dpf-sim-controller/internal/carbide"
	"github.com/nvidia/infra-controller/dev/k8s/dpf-sim-controller/internal/redfish"
)

// Defaults mirrored from the real DPF operator
// (internal/provisioning/controllers/dpu/state/redfish/client/client.go and
// cmd/provisioning/main.go in doca-platform).
const (
	// DefaultBMCSecretName is the Secret NICo writes the shared DPU BMC
	// password to in the DPF namespace (crates/dpf/src/sdk.rs SECRET_NAME).
	DefaultBMCSecretName = "bmc-shared-password"
	// bmcSecretPasswordKey is the key in that Secret holding the password.
	bmcSecretPasswordKey = "password"
	// DefaultBMCUsername is the BlueField-3 BMC account the real operator logs
	// in as.
	DefaultBMCUsername = "root"
	// defaultBMCPort is the DPUDevice.spec.bmcPort CRD default.
	defaultBMCPort = 443

	credentialTTL = time.Minute
)

// OSInstallConfig makes the simulator install the managed BFB through the DPU
// BMC's Redfish during OS Installing, as the real DPF operator does, instead of
// only dwelling. A nil config on the reconciler keeps the dwell-only behavior.
type OSInstallConfig struct {
	// SecretName is the Secret in the DPF namespace holding the BMC password
	// under the key "password". Zero value means DefaultBMCSecretName.
	SecretName string
	// Username is the BMC account. Zero value means DefaultBMCUsername.
	Username string
	// Timeout bounds the whole OS Installing phase. When it passes, the DPU
	// moves to Error, as the real operator's OS install timeout does.
	Timeout time.Duration
	// PollInterval is how often an accepted install task is polled.
	PollInterval time.Duration
	// RequestTimeout bounds each Redfish request.
	RequestTimeout time.Duration
}

// osInstaller carries the runtime state OSInstallConfig needs: a direct
// (uncached) Secret reader, so the simulator's RBAC stays at "get one Secret"
// instead of list/watch on every Secret in the namespace, and a short-lived
// credential cache, so polling thousands of DPUs does not read the Secret on
// every poll.
type osInstaller struct {
	OSInstallConfig
	secrets client.Reader
	ns      string

	mu        sync.Mutex
	password  string
	fetchedAt time.Time
}

func newOSInstaller(cfg OSInstallConfig, secrets client.Reader, namespace string) *osInstaller {
	if cfg.SecretName == "" {
		cfg.SecretName = DefaultBMCSecretName
	}
	if cfg.Username == "" {
		cfg.Username = DefaultBMCUsername
	}
	return &osInstaller{OSInstallConfig: cfg, secrets: secrets, ns: namespace}
}

// bmcPassword returns the shared BMC password. NICo refreshes the Secret when
// the site credential changes, so a cached value is dropped after credentialTTL.
func (o *osInstaller) bmcPassword(ctx context.Context) (string, error) {
	o.mu.Lock()
	defer o.mu.Unlock()
	if o.password != "" && time.Since(o.fetchedAt) < credentialTTL {
		return o.password, nil
	}
	var s corev1.Secret
	if err := o.secrets.Get(ctx, types.NamespacedName{Namespace: o.ns, Name: o.SecretName}, &s); err != nil {
		return "", fmt.Errorf("read BMC secret %s/%s: %w", o.ns, o.SecretName, err)
	}
	pw := string(s.Data[bmcSecretPasswordKey])
	if pw == "" {
		return "", fmt.Errorf("BMC secret %s/%s has no %q key", o.ns, o.SecretName, bmcSecretPasswordKey)
	}
	o.password, o.fetchedAt = pw, time.Now()
	return pw, nil
}

// bmcBaseURL is the DPU BMC address NICo registered on the DPUDevice. Real DPF
// copies spec.bmcIp/bmcPort into status on discovery and dials the status
// values; the simulator has no discovery, so it falls back to the spec.
func bmcBaseURL(device *provisioningv1.DPUDevice) (string, error) {
	ip, port := device.Spec.BMCIP, device.Spec.BMCPort
	if device.Status.BMCIP != nil {
		ip = device.Status.BMCIP
	}
	if device.Status.BMCPort != nil {
		port = device.Status.BMCPort
	}
	if ip == nil || *ip == "" {
		return "", fmt.Errorf("DPUDevice %s has no bmcIp", device.Name)
	}
	p := uint32(defaultBMCPort)
	if port != nil {
		p = *port
	}
	if p == defaultBMCPort {
		return "https://" + *ip, nil
	}
	return fmt.Sprintf("https://%s:%d", *ip, p), nil
}

// bfbImageURI is the BFB URL of the BFB CR the DPU references. Real DPF hands
// the BMC a path on its own BFB registry that serves this same file, so the
// BMC fetches the managed image from there. The simulator runs no registry
// and passes the CR's source URL, which names the same managed image.
func (r *DPUDeviceReconciler) bfbImageURI(ctx context.Context, dpu *provisioningv1.DPU) (string, error) {
	var bfb provisioningv1.BFB
	if err := r.Get(ctx, types.NamespacedName{Namespace: r.Namespace, Name: dpu.Spec.BFB}, &bfb); err != nil {
		return "", fmt.Errorf("get BFB %q: %w", dpu.Spec.BFB, err)
	}
	if bfb.Spec.URL == "" {
		return "", fmt.Errorf("BFB %q has no spec.url", dpu.Spec.BFB)
	}
	return bfb.Spec.URL, nil
}

// osInstallStep drives the Redfish BFB install for a DPU in OS Installing.
// It reports advance=true once the BMC task completed, and the phase should
// move on. Otherwise the returned result says when to look again.
//
// The accepted task's path is kept in AnnSimBFBInstallTask so a reconcile never
// submits a second install while one is running. A SimpleUpdate the BMC
// refuses, a task in Exception, and the phase timeout all move the DPU to Error,
// as in the real operator. A lost task (404) also fails immediately because its
// outcome is unknown; transport failures are retried until the phase timeout.
func (r *DPUDeviceReconciler) osInstallStep(
	ctx context.Context, device *provisioningv1.DPUDevice, dpu *provisioningv1.DPU,
) (advance bool, res ctrl.Result, err error) {
	l := log.FromContext(ctx)
	inst := r.osInstaller

	// A DPU provisioned from a BlueFieldSoftware CR has no BFB to install; the
	// real operator takes a different path for it, which is not simulated.
	if dpu.Spec.BFB == "" {
		l.V(1).Info("DPU has no spec.bfb; skipping Redfish install", "dpu", dpu.Name)
		return true, ctrl.Result{}, nil
	}

	entered, perr := time.Parse(time.RFC3339, dpu.Annotations[carbide.AnnSimPhaseEnteredAt])
	if perr != nil {
		// Missing/garbled stamp: stamp now so the timeout has a start.
		if err := r.setDPUAnnotation(ctx, dpu, carbide.AnnSimPhaseEnteredAt, time.Now().UTC().Format(time.RFC3339)); err != nil {
			return false, ctrl.Result{}, err
		}
		return false, ctrl.Result{RequeueAfter: inst.PollInterval}, nil
	}
	if elapsed := time.Since(entered); elapsed > inst.Timeout {
		msg := fmt.Sprintf("OS install timeout exceeded: %s > %s", elapsed.Round(time.Second), inst.Timeout)
		return false, ctrl.Result{}, r.failOSInstall(ctx, dpu, "InstallationTimeout", msg)
	}

	password, err := inst.bmcPassword(ctx)
	if err != nil {
		l.Info("BMC credentials not available; retrying", "dpu", dpu.Name, "err", err.Error())
		return false, ctrl.Result{RequeueAfter: inst.PollInterval}, nil
	}
	baseURL, err := bmcBaseURL(device)
	if err != nil {
		return false, ctrl.Result{}, r.failOSInstall(ctx, dpu, "NoBMCAddress", err.Error())
	}
	bmc := redfish.New(baseURL, inst.Username, password, inst.RequestTimeout)

	task := dpu.Annotations[carbide.AnnSimBFBInstallTask]
	if task == "" {
		uri, err := r.bfbImageURI(ctx, dpu)
		if err != nil {
			// The BFB CR may not exist yet; retry until the timeout.
			l.Info("managed BFB not resolvable; retrying", "dpu", dpu.Name, "err", err.Error())
			return false, ctrl.Result{RequeueAfter: inst.PollInterval}, nil
		}
		task, err = bmc.SimpleUpdate(ctx, uri)
		if errors.Is(err, redfish.ErrUpdateRejected) {
			return false, ctrl.Result{}, r.failOSInstall(ctx, dpu, "FailToInstall", err.Error())
		}
		if err != nil {
			l.Info("BFB install request failed; retrying", "dpu", dpu.Name, "bmc", baseURL, "err", err.Error())
			return false, ctrl.Result{RequeueAfter: inst.PollInterval}, nil
		}
		l.Info("BFB install submitted", "dpu", dpu.Name, "bmc", baseURL, "task", task, "image", uri)
		if err := r.setDPUAnnotation(ctx, dpu, carbide.AnnSimBFBInstallTask, task); err != nil {
			return false, ctrl.Result{}, err
		}
		return false, ctrl.Result{RequeueAfter: inst.PollInterval}, nil
	}

	t, err := bmc.GetTask(ctx, task)
	if errors.Is(err, redfish.ErrTaskNotFound) {
		msg := fmt.Sprintf("install task %s was lost: %v", task, err)
		return false, ctrl.Result{}, r.failOSInstall(ctx, dpu, "TaskLost", msg)
	}
	if err != nil {
		l.Info("BFB install task poll failed; retrying", "dpu", dpu.Name, "task", task, "err", err.Error())
		return false, ctrl.Result{RequeueAfter: inst.PollInterval}, nil
	}
	switch t.State {
	case redfish.TaskStateCompleted:
		l.Info("BFB install completed", "dpu", dpu.Name, "task", task)
		return true, ctrl.Result{}, nil
	case redfish.TaskStateException:
		msg := fmt.Sprintf("install task %s is in Exception state: %s", task, strings.Join(t.Messages, "; "))
		return false, ctrl.Result{}, r.failOSInstall(ctx, dpu, "FailToInstall", msg)
	}
	l.V(1).Info("BFB install in progress", "dpu", dpu.Name, "task", task, "state", t.State, "percent", t.PercentComplete)
	return false, ctrl.Result{RequeueAfter: inst.PollInterval}, nil
}

// failOSInstall moves the DPU to Error with an OSInstalled=False condition, as
// the real operator's Installing handler does.
func (r *DPUDeviceReconciler) failOSInstall(ctx context.Context, dpu *provisioningv1.DPU, reason, message string) error {
	log.FromContext(ctx).Info("BFB install failed; moving DPU to Error", "dpu", dpu.Name, "reason", reason, "message", message)
	meta.SetStatusCondition(&dpu.Status.Conditions, metav1.Condition{
		Type:    string(provisioningv1.DPUCondOSInstalled),
		Status:  metav1.ConditionFalse,
		Reason:  reason,
		Message: message,
	})
	dpu.Status.Phase = provisioningv1.DPUError
	return r.Status().Update(ctx, dpu)
}
