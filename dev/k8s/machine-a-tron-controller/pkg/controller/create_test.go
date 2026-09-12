// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package controller

// These tests drive the Reconciler against the client-go fake clientset through
// the real K8sServiceClient, so the create, adopt, evict and retry paths are
// exercised with the same error shapes the API server produces.

import (
	"context"
	"fmt"
	"sync/atomic"
	"testing"
	"time"

	"github.com/rs/zerolog"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	corev1 "k8s.io/api/core/v1"
	apierrors "k8s.io/apimachinery/pkg/api/errors"
	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
	"k8s.io/apimachinery/pkg/runtime"
	"k8s.io/apimachinery/pkg/util/validation/field"
	"k8s.io/apimachinery/pkg/util/wait"
	"k8s.io/client-go/kubernetes/fake"
	k8stesting "k8s.io/client-go/testing"

	"github.com/NVIDIA/infra-controller/dev/k8s/machine-a-tron-controller/pkg/matclient"
)

const (
	fakeNamespace = "nico-system"
	fakeInstance  = "https://nico-machine-a-tron-bmc-mock.nico-system.svc:1266"
	fakeDownURL   = "https://nico-machine-a-tron-down-bmc-mock.nico-system.svc:1266"
)

// fastBackoff keeps retry tests quick while still exercising several attempts.
var fastBackoff = wait.Backoff{Steps: 4, Duration: time.Millisecond, Factor: 1.0}

// deviceWithIP returns a status response with one host at the given BMC IP.
func deviceWithIP(matID, ip string) *matclient.MachinesStatusResponse {
	return &matclient.MachinesStatusResponse{
		Machines: []matclient.MachineStatus{{
			MatID:      matID,
			APIState:   "Ready",
			PowerState: "On",
			BMC: matclient.BMCStatus{
				IP:      ptr(ip),
				Redfish: matclient.EndpointStatus{ReachablePort: 443, ListenPort: 1266},
			},
		}},
	}
}

// fakeBuilder matches the builder newFakeReconciler uses, so a Service from
// managedService compares equal to the one the reconciler desires.
func fakeBuilder() *ServiceBuilder {
	return &ServiceBuilder{Namespace: fakeNamespace, BaseSelector: map[string]string{"app": "mat"}}
}

// managedService returns the Service this controller would have created for
// matID at ip, including the BMC IP annotation that marks it as owning ip.
func managedService(matID, ip string) *corev1.Service {
	return fakeBuilder().BuildService(&deviceWithIP(matID, ip).Machines[0], MachineTypeHost, "", "")
}

// newFakeReconciler wires a Reconciler to the fake clientset through the real
// K8sServiceClient. fetchers maps each discovered instance URL to the status
// it serves; an entry with err set fails its fetch, which sets fetchFailed in
// Reconcile and gates deletions as in a fleet with an unreachable pod.
func newFakeReconciler(cs *fake.Clientset, fetchers map[string]*mockStatusFetcher) *Reconciler {
	instances := make([]DiscoveredInstance, 0, len(fetchers))
	for url := range fetchers {
		instances = append(instances, DiscoveredInstance{URL: url, ServiceName: "nico-machine-a-tron-bmc-mock"})
	}
	r := NewReconciler(&mockDiscovery{instances: instances}, fakeBuilder(), NewRealK8sServiceClient(cs), nil, nil, zerolog.Nop())
	r.createBackoff = fastBackoff
	r.statusFetcher = func(url string) (StatusFetcher, error) {
		return fetchers[url], nil
	}
	return r
}

// clusterIPAllocatedError mirrors the API server's rejection of a create whose
// ClusterIP is already held by another Service.
func clusterIPAllocatedError(name, ip string) error {
	return apierrors.NewInvalid(corev1.SchemeGroupVersion.WithKind("Service").GroupKind(), name, field.ErrorList{
		field.Invalid(field.NewPath("spec", "clusterIPs"), []string{ip},
			fmt.Sprintf("failed to allocate IP %s: provided IP is already allocated", ip)),
	})
}

// withClusterIPAllocator makes the fake API server behave like the real
// ClusterIP allocator: a create is rejected while another Service in the
// namespace holds the requested address.
func withClusterIPAllocator(cs *fake.Clientset) {
	cs.PrependReactor("create", "services", func(action k8stesting.Action) (bool, runtime.Object, error) {
		svc := action.(k8stesting.CreateAction).GetObject().(*corev1.Service)
		if svc.Spec.ClusterIP == "" {
			return false, nil, nil
		}
		obj, err := cs.Tracker().List(
			corev1.SchemeGroupVersion.WithResource("services"),
			corev1.SchemeGroupVersion.WithKind("Service"),
			svc.Namespace)
		if err != nil {
			return true, nil, err
		}
		for _, item := range obj.(*corev1.ServiceList).Items {
			if item.Name != svc.Name && item.Spec.ClusterIP == svc.Spec.ClusterIP {
				return true, nil, clusterIPAllocatedError(svc.Name, svc.Spec.ClusterIP)
			}
		}
		return false, nil, nil
	})
}

// rejectFirstCreates rejects the first n creates of the named Service with a
// ClusterIP conflict and lets later attempts through. The returned counter
// records every create attempt for that name.
func rejectFirstCreates(cs *fake.Clientset, name string, n int) *int32 {
	var attempts int32
	cs.PrependReactor("create", "services", func(action k8stesting.Action) (bool, runtime.Object, error) {
		svc := action.(k8stesting.CreateAction).GetObject().(*corev1.Service)
		if svc.Name != name {
			return false, nil, nil
		}
		if atomic.AddInt32(&attempts, 1) <= int32(n) {
			return true, nil, clusterIPAllocatedError(svc.Name, svc.Spec.ClusterIP)
		}
		return false, nil, nil
	})
	return &attempts
}

func listServices(t *testing.T, cs *fake.Clientset) []corev1.Service {
	t.Helper()
	list, err := cs.CoreV1().Services(fakeNamespace).List(context.Background(), metav1.ListOptions{})
	require.NoError(t, err)
	return list.Items
}

func countCreateActions(cs *fake.Clientset) int {
	n := 0
	for _, a := range cs.Actions() {
		if a.Matches("create", "services") {
			n++
		}
	}
	return n
}

func TestReconcile_CreatesServicesOnFirstAttempt(t *testing.T) {
	cs := fake.NewClientset()
	withClusterIPAllocator(cs)
	r := newFakeReconciler(cs, map[string]*mockStatusFetcher{
		fakeInstance: {status: &matclient.MachinesStatusResponse{Machines: []matclient.MachineStatus{
			deviceWithIP("host-a", "10.96.64.10").Machines[0],
			deviceWithIP("host-b", "10.96.64.11").Machines[0],
		}}},
	})

	result := r.Reconcile(context.Background())

	require.Empty(t, result.Errors)
	assert.Equal(t, 2, result.Created)
	assert.Equal(t, 0, result.Adopted)
	assert.Equal(t, 2, countCreateActions(cs), "each Service takes a single create call")

	services := listServices(t, cs)
	require.Len(t, services, 2)
	for _, svc := range services {
		assert.Equal(t, LabelManagedByValue, svc.Labels[LabelManagedBy])
		assert.Equal(t, svc.Annotations[AnnotationBMCIP], svc.Spec.ClusterIP)
	}
}

func TestReconcile_RetriesCreateAfterClusterIPConflict(t *testing.T) {
	cs := fake.NewClientset()
	name := BuildServiceName(MachineTypeHost, "host-a")
	attempts := rejectFirstCreates(cs, name, 1)
	r := newFakeReconciler(cs, map[string]*mockStatusFetcher{
		fakeInstance: {status: deviceWithIP("host-a", "10.96.64.10")},
	})

	result := r.Reconcile(context.Background())

	require.Empty(t, result.Errors)
	assert.Equal(t, 1, result.Created)
	assert.Equal(t, int32(2), atomic.LoadInt32(attempts), "rejected once, then created")

	services := listServices(t, cs)
	require.Len(t, services, 1)
	assert.Equal(t, name, services[0].Name)
	assert.Equal(t, "10.96.64.10", services[0].Spec.ClusterIP)
	assert.Equal(t, "10.96.64.10", services[0].Annotations[AnnotationBMCIP])
}

func TestReconcile_ReportsClusterIPConflictAfterRetriesExhausted(t *testing.T) {
	cs := fake.NewClientset()
	name := BuildServiceName(MachineTypeHost, "host-a")
	attempts := rejectFirstCreates(cs, name, 100)
	r := newFakeReconciler(cs, map[string]*mockStatusFetcher{
		fakeInstance: {status: deviceWithIP("host-a", "10.96.64.10")},
	})

	result := r.Reconcile(context.Background())

	require.Len(t, result.Errors, 1)
	assert.Contains(t, result.Errors[0].Error(), "already allocated")
	assert.Equal(t, 0, result.Created)
	assert.Equal(t, int32(fastBackoff.Steps), atomic.LoadInt32(attempts), "one attempt per backoff step")
	assert.Empty(t, listServices(t, cs))
}

func TestReconcile_AdoptsExistingServiceWithSameName(t *testing.T) {
	name := BuildServiceName(MachineTypeHost, "host-a")
	// A Service under the desired name without the managed-by label: the
	// managed List does not return it, so the diff schedules a create.
	foreign := &corev1.Service{
		ObjectMeta: metav1.ObjectMeta{
			Name:        name,
			Namespace:   fakeNamespace,
			Labels:      map[string]string{"external.example.com/owner": "operator"},
			Annotations: map[string]string{"external.example.com/note": "keep"},
		},
		Spec: corev1.ServiceSpec{
			Type:      corev1.ServiceTypeClusterIP,
			ClusterIP: "10.96.64.10",
			Ports:     []corev1.ServicePort{{Name: "old", Port: 1, Protocol: corev1.ProtocolTCP}},
		},
	}
	cs := fake.NewClientset(foreign)
	withClusterIPAllocator(cs)
	r := newFakeReconciler(cs, map[string]*mockStatusFetcher{
		fakeInstance: {status: deviceWithIP("host-a", "10.96.64.10")},
	})

	result := r.Reconcile(context.Background())

	require.Empty(t, result.Errors)
	assert.Equal(t, 0, result.Created)
	assert.Equal(t, 1, result.Adopted)

	services := listServices(t, cs)
	require.Len(t, services, 1, "the existing Service is adopted, not duplicated")
	svc := services[0]
	assert.Equal(t, "10.96.64.10", svc.Spec.ClusterIP)
	assert.Equal(t, LabelManagedByValue, svc.Labels[LabelManagedBy])
	assert.Equal(t, "host-a", svc.Labels[LabelMatID])
	assert.Equal(t, "10.96.64.10", svc.Annotations[AnnotationBMCIP])
	assert.Equal(t, "Ready", svc.Annotations[AnnotationAPIState])
	assert.Equal(t, "operator", svc.Labels["external.example.com/owner"], "foreign label kept")
	assert.Equal(t, "keep", svc.Annotations["external.example.com/note"], "foreign annotation kept")
	require.Len(t, svc.Spec.Ports, 1)
	assert.Equal(t, PortNameRedfish, svc.Spec.Ports[0].Name)

	// A second cycle sees the adopted Service in the managed List and is a no-op.
	result = r.Reconcile(context.Background())
	require.Empty(t, result.Errors)
	assert.Equal(t, 0, result.Created+result.Adopted+result.Updated+result.Recreated)
}

func TestReconcile_ReplacesExistingServiceWithDifferentClusterIP(t *testing.T) {
	name := BuildServiceName(MachineTypeHost, "host-a")
	stale := &corev1.Service{
		ObjectMeta: metav1.ObjectMeta{Name: name, Namespace: fakeNamespace},
		Spec: corev1.ServiceSpec{
			Type:      corev1.ServiceTypeClusterIP,
			ClusterIP: "10.96.64.99",
			Ports:     []corev1.ServicePort{{Name: "old", Port: 1, Protocol: corev1.ProtocolTCP}},
		},
	}
	cs := fake.NewClientset(stale)
	withClusterIPAllocator(cs)
	r := newFakeReconciler(cs, map[string]*mockStatusFetcher{
		fakeInstance: {status: deviceWithIP("host-a", "10.96.64.10")},
	})

	result := r.Reconcile(context.Background())

	require.Empty(t, result.Errors)
	assert.Equal(t, 1, result.Adopted)

	services := listServices(t, cs)
	require.Len(t, services, 1)
	assert.Equal(t, name, services[0].Name)
	assert.Equal(t, "10.96.64.10", services[0].Spec.ClusterIP, "ClusterIP is immutable, so the Service was recreated")
	assert.Equal(t, "10.96.64.10", services[0].Annotations[AnnotationBMCIP])
	assert.Equal(t, LabelManagedByValue, services[0].Labels[LabelManagedBy])
}

func TestReconcile_EvictsServiceHoldingClusterIPItDoesNotOwn(t *testing.T) {
	// A Service created for another device before it had a BMC IP: the API
	// server gave it an arbitrary address that NICo DHCP has since leased to
	// host-a. The failing second instance keeps regular deletes gated, as in a
	// fleet where one machine-a-tron pod is unreachable, so the placeholder
	// would otherwise block host-a's address on every cycle.
	placeholder := managedService("host-other", "10.96.64.10")
	delete(placeholder.Annotations, AnnotationBMCIP)
	cs := fake.NewClientset(placeholder)
	withClusterIPAllocator(cs)
	r := newFakeReconciler(cs, map[string]*mockStatusFetcher{
		fakeInstance: {status: deviceWithIP("host-a", "10.96.64.10")},
		fakeDownURL:  {err: fmt.Errorf("connection refused")},
	})

	result := r.Reconcile(context.Background())

	require.Len(t, result.Errors, 1, "only the fetch failure is reported")
	assert.Contains(t, result.Errors[0].Error(), "connection refused")
	assert.Equal(t, 1, result.Created)
	assert.Equal(t, 0, result.Deleted, "regular deletes stay gated while a fetch fails")

	services := listServices(t, cs)
	require.Len(t, services, 1, "placeholder evicted, exactly one Service remains")
	assert.Equal(t, BuildServiceName(MachineTypeHost, "host-a"), services[0].Name)
	assert.Equal(t, "10.96.64.10", services[0].Spec.ClusterIP)
	assert.Equal(t, "10.96.64.10", services[0].Annotations[AnnotationBMCIP])
}

func TestReconcile_DoesNotEvictServiceOwningClusterIP(t *testing.T) {
	// Two devices reporting the same BMC IP is a data problem: the Service
	// that records the address as its BMC IP is left in place and the
	// conflict is reported instead of being resolved by deleting it.
	owner := managedService("host-other", "10.96.64.10")
	cs := fake.NewClientset(owner)
	withClusterIPAllocator(cs)
	r := newFakeReconciler(cs, map[string]*mockStatusFetcher{
		fakeInstance: {status: deviceWithIP("host-a", "10.96.64.10")},
		fakeDownURL:  {err: fmt.Errorf("connection refused")},
	})

	result := r.Reconcile(context.Background())

	require.Len(t, result.Errors, 2, "fetch failure and create conflict")
	conflict := false
	for _, err := range result.Errors {
		if apierrors.IsInvalid(err) {
			conflict = true
			assert.Contains(t, err.Error(), "already allocated")
		}
	}
	assert.True(t, conflict, "the ClusterIP conflict is reported")
	assert.Equal(t, 0, result.Created)

	services := listServices(t, cs)
	require.Len(t, services, 1)
	assert.Equal(t, owner.Name, services[0].Name, "legitimate holder untouched")
}

func TestReconcile_RecreatesMovedServiceWhenAnotherFetchFails(t *testing.T) {
	// host-a moved from .10 to .11. The recreate is derived from a successful
	// fetch, so it must not be held back by an unrelated instance failing,
	// while the Service of a device that vanished stays until every fetch
	// succeeds.
	moved := managedService("host-a", "10.96.64.10")
	stale := managedService("host-gone", "10.96.64.50")
	cs := fake.NewClientset(moved, stale)
	withClusterIPAllocator(cs)
	r := newFakeReconciler(cs, map[string]*mockStatusFetcher{
		fakeInstance: {status: deviceWithIP("host-a", "10.96.64.11")},
		fakeDownURL:  {err: fmt.Errorf("connection refused")},
	})

	result := r.Reconcile(context.Background())

	require.Len(t, result.Errors, 1, "only the fetch failure is reported")
	assert.Equal(t, 1, result.Recreated)
	assert.Equal(t, 0, result.Deleted)

	byName := map[string]corev1.Service{}
	for _, svc := range listServices(t, cs) {
		byName[svc.Name] = svc
	}
	require.Len(t, byName, 2)
	assert.Equal(t, "10.96.64.11", byName[moved.Name].Spec.ClusterIP)
	assert.Equal(t, "10.96.64.11", byName[moved.Name].Annotations[AnnotationBMCIP])
	assert.Contains(t, byName, stale.Name, "stale Service kept while a fetch fails")
}

func TestIsClusterIPConflict(t *testing.T) {
	kind := corev1.SchemeGroupVersion.WithKind("Service").GroupKind()
	tests := []struct {
		name string
		err  error
		want bool
	}{
		{name: "nil", err: nil, want: false},
		{name: "address already allocated", err: clusterIPAllocatedError("svc", "10.96.64.10"), want: true},
		{name: "wrapped", err: fmt.Errorf("creating: %w", clusterIPAllocatedError("svc", "10.96.64.10")), want: true},
		{
			name: "address outside the service range",
			err: apierrors.NewInvalid(kind, "svc", field.ErrorList{
				field.Invalid(field.NewPath("spec", "clusterIPs"), []string{"192.168.1.1"},
					"failed to allocate IP 192.168.1.1: the provided IP (192.168.1.1) is not in the valid range"),
			}),
			want: false,
		},
		{
			name: "invalid on another field",
			err: apierrors.NewInvalid(kind, "svc", field.ErrorList{
				field.Invalid(field.NewPath("spec", "ports"), nil, "already allocated"),
			}),
			want: false,
		},
		{name: "already exists", err: apierrors.NewAlreadyExists(corev1.Resource("services"), "svc"), want: false},
		{name: "plain error", err: fmt.Errorf("boom"), want: false},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			assert.Equal(t, tt.want, isClusterIPConflict(tt.err))
		})
	}
}
