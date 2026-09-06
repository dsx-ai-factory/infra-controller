// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package controller

import (
	"context"
	"fmt"
	"sync"
	"testing"

	"github.com/rs/zerolog"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	corev1 "k8s.io/api/core/v1"
	apierrors "k8s.io/apimachinery/pkg/api/errors"
	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
	"k8s.io/apimachinery/pkg/util/intstr"

	"github.com/NVIDIA/infra-controller/dev/k8s/machine-a-tron-controller/pkg/matclient"
)

func TestBuildServiceName(t *testing.T) {
	tests := []struct {
		machineType string
		matID       string
		want        string
	}{
		{
			machineType: MachineTypeHost,
			matID:       "12345678-1234-1234-1234-123456789abc",
			want:        "mat-bmc-host-12345678-123-0cdd56f0",
		},
		{
			machineType: MachineTypeDPU,
			matID:       "abcdefgh-1234-1234-1234-123456789abc",
			want:        "mat-bmc-dpu-abcdefgh-123-e6e1a630",
		},
		{
			machineType: MachineTypeHost,
			matID:       "short",
			want:        "mat-bmc-host-short",
		},
	}

	for _, tt := range tests {
		t.Run(tt.want, func(t *testing.T) {
			got := BuildServiceName(tt.machineType, tt.matID)
			assert.Equal(t, tt.want, got)
		})
	}
}

func TestBuildServiceName_TruncatedIDsRemainUnique(t *testing.T) {
	first := BuildServiceName(MachineTypeHost, "same-prefix-1234567890")
	second := BuildServiceName(MachineTypeHost, "same-prefix-abcdefghij")

	assert.NotEqual(t, first, second)
	assert.Contains(t, first, "same-prefix-")
	assert.Contains(t, second, "same-prefix-")
}

func TestServiceBuilder_BuildService(t *testing.T) {
	builder := &ServiceBuilder{
		Namespace: "test-ns",
		BaseSelector: map[string]string{
			"app": "machine-a-tron",
		},
	}

	machine := &matclient.MachineStatus{
		MatID:        "host-uuid-12345678",
		MachineID:    ptr("nico-machine-id"),
		HardwareType: ptr("GB200"),
		APIState:     "Ready",
		PowerState:   "On",
		BMC: matclient.BMCStatus{
			IP: ptr("192.168.1.100"),
			Redfish: matclient.EndpointStatus{
				ReachablePort: 443,
				ListenPort:    8443,
			},
		},
	}

	svc := builder.BuildService(machine, MachineTypeHost, "", "")

	// Check basic metadata
	assert.Equal(t, "mat-bmc-host-host-uuid-12-306c5924", svc.Name)
	assert.Equal(t, "test-ns", svc.Namespace)

	// Check labels
	assert.Equal(t, LabelManagedByValue, svc.Labels[LabelManagedBy])
	assert.Equal(t, "host-uuid-12345678", svc.Labels[LabelMatID])
	assert.Equal(t, "nico-machine-id", svc.Labels[LabelMachineID])
	assert.Equal(t, MachineTypeHost, svc.Labels[LabelMachineType])

	// Check annotations
	assert.Equal(t, "192.168.1.100", svc.Annotations[AnnotationBMCIP])
	assert.Equal(t, "Ready", svc.Annotations[AnnotationAPIState])
	assert.Equal(t, "On", svc.Annotations[AnnotationPowerState])
	assert.Equal(t, "GB200", svc.Annotations[AnnotationHardwareType])
	assert.Equal(t, "8443", svc.Annotations[AnnotationRedfishListenPort])

	// Check ports
	require.Len(t, svc.Spec.Ports, 1)
	assert.Equal(t, PortNameRedfish, svc.Spec.Ports[0].Name)
	assert.Equal(t, corev1.ProtocolTCP, svc.Spec.Ports[0].Protocol)
	assert.Equal(t, int32(443), svc.Spec.Ports[0].Port)
	assert.Equal(t, intstr.FromInt32(8443), svc.Spec.Ports[0].TargetPort)

	// Check selector
	assert.Equal(t, builder.BaseSelector, svc.Spec.Selector)
}

func TestServiceBuilder_BuildService_WithIPMI(t *testing.T) {
	builder := &ServiceBuilder{
		Namespace: "test-ns",
		BaseSelector: map[string]string{
			"app": "machine-a-tron",
		},
	}

	machine := &matclient.MachineStatus{
		MatID:      "host-uuid-12345678",
		APIState:   "Ready",
		PowerState: "On",
		BMC: matclient.BMCStatus{
			IP: ptr("192.168.1.100"),
			Redfish: matclient.EndpointStatus{
				ReachablePort: 443,
				ListenPort:    8443,
			},
			IPMI: &matclient.EndpointStatus{
				ReachablePort: 16023,
				ListenPort:    16023,
			},
		},
	}

	svc := builder.BuildService(machine, MachineTypeHost, "", "")

	// Check we have both ports
	require.Len(t, svc.Spec.Ports, 2)

	// Find ports by name
	var redfishPort, ipmiPort *corev1.ServicePort
	for i := range svc.Spec.Ports {
		switch svc.Spec.Ports[i].Name {
		case PortNameRedfish:
			redfishPort = &svc.Spec.Ports[i]
		case PortNameIPMI:
			ipmiPort = &svc.Spec.Ports[i]
		}
	}

	require.NotNil(t, redfishPort)
	assert.Equal(t, corev1.ProtocolTCP, redfishPort.Protocol)
	assert.Equal(t, int32(443), redfishPort.Port)
	assert.Equal(t, intstr.FromInt32(8443), redfishPort.TargetPort)

	require.NotNil(t, ipmiPort)
	assert.Equal(t, corev1.ProtocolUDP, ipmiPort.Protocol)
	assert.Equal(t, int32(16023), ipmiPort.Port)
	assert.Equal(t, intstr.FromInt32(16023), ipmiPort.TargetPort)

	// Check IPMI annotation
	assert.Equal(t, "16023", svc.Annotations[AnnotationIPMIListenPort])
}

func TestServiceBuilder_BuildService_WithSSH(t *testing.T) {
	builder := &ServiceBuilder{
		Namespace: "test-ns",
		BaseSelector: map[string]string{
			"app": "machine-a-tron",
		},
	}

	machine := &matclient.MachineStatus{
		MatID:      "dpu-uuid-12345678",
		APIState:   "Ready",
		PowerState: "On",
		BMC: matclient.BMCStatus{
			IP: ptr("192.168.1.101"),
			Redfish: matclient.EndpointStatus{
				ReachablePort: 443,
				ListenPort:    8444,
			},
			SSH: &matclient.EndpointStatus{
				ReachablePort: 32022,
				ListenPort:    32022,
			},
		},
	}

	svc := builder.BuildService(machine, MachineTypeDPU, "parent-host-uuid", "")

	require.Len(t, svc.Spec.Ports, 2)
	assert.Equal(t, PortNameSSH, svc.Spec.Ports[1].Name)
	assert.Equal(t, corev1.ProtocolTCP, svc.Spec.Ports[1].Protocol)
	assert.Equal(t, int32(32022), svc.Spec.Ports[1].Port)
	assert.Equal(t, intstr.FromInt32(32022), svc.Spec.Ports[1].TargetPort)
	assert.Equal(t, "32022", svc.Annotations[AnnotationSSHListenPort])
}

func TestServiceBuilder_BuildService_DPU(t *testing.T) {
	builder := &ServiceBuilder{
		Namespace: "test-ns",
		BaseSelector: map[string]string{
			"app": "machine-a-tron",
		},
	}

	dpu := &matclient.MachineStatus{
		MatID:      "dpu-uuid-12345678",
		MachineID:  ptr("dpu-machine-id"),
		APIState:   "Ready",
		PowerState: "On",
		BMC: matclient.BMCStatus{
			IP: ptr("192.168.1.101"),
			Redfish: matclient.EndpointStatus{
				ReachablePort: 443,
				ListenPort:    8444,
			},
		},
	}

	svc := builder.BuildService(dpu, MachineTypeDPU, "parent-host-uuid", "")

	// Check DPU-specific labels
	assert.Equal(t, MachineTypeDPU, svc.Labels[LabelMachineType])
	assert.Equal(t, "parent-host-uuid", svc.Labels[LabelParentMatID])
}

func TestServiceBuilder_BuildService_BMCIPAsClusterIP(t *testing.T) {
	builder := &ServiceBuilder{
		Namespace: "test-ns",
		BaseSelector: map[string]string{
			"app": "machine-a-tron",
		},
	}

	machine := &matclient.MachineStatus{
		MatID:      "host-uuid-12345678",
		APIState:   "Ready",
		PowerState: "On",
		BMC: matclient.BMCStatus{
			IP: ptr("10.100.0.20"),
			Redfish: matclient.EndpointStatus{
				ReachablePort: 443,
				ListenPort:    8443,
			},
		},
	}

	svc := builder.BuildService(machine, MachineTypeHost, "", "")

	// Check BMC IP is used directly as ClusterIP
	assert.Equal(t, "10.100.0.20", svc.Spec.ClusterIP)
}

func TestServiceBuilder_BuildServicesFromStatus(t *testing.T) {
	builder := &ServiceBuilder{
		Namespace: "test-ns",
		BaseSelector: map[string]string{
			"app": "machine-a-tron",
		},
	}

	status := &matclient.MachinesStatusResponse{
		Machines: []matclient.MachineStatus{
			{
				MatID:      "host-1",
				APIState:   "Ready",
				PowerState: "On",
				BMC: matclient.BMCStatus{
					IP:      ptr("10.0.0.1"),
					Redfish: matclient.EndpointStatus{ReachablePort: 443, ListenPort: 8443},
				},
				DPUs: []matclient.MachineStatus{
					{
						MatID:      "dpu-1",
						APIState:   "Ready",
						PowerState: "On",
						BMC: matclient.BMCStatus{
							IP:      ptr("10.0.0.2"),
							Redfish: matclient.EndpointStatus{ReachablePort: 443, ListenPort: 8444},
						},
					},
					{
						MatID:      "dpu-2",
						APIState:   "Ready",
						PowerState: "On",
						BMC: matclient.BMCStatus{
							IP:      ptr("10.0.0.3"),
							Redfish: matclient.EndpointStatus{ReachablePort: 443, ListenPort: 8445},
						},
					},
				},
			},
			{
				MatID:      "host-2",
				APIState:   "Ready",
				PowerState: "On",
				BMC: matclient.BMCStatus{
					IP:      ptr("10.0.0.4"),
					Redfish: matclient.EndpointStatus{ReachablePort: 443, ListenPort: 8446},
				},
			},
		},
	}

	services := builder.BuildServicesFromStatus(status, "")

	// Should have 4 services: 2 hosts + 2 DPUs
	assert.Len(t, services, 4)

	// Verify parent links for DPUs
	dpuServices := make([]*corev1.Service, 0)
	for _, svc := range services {
		if svc.Labels[LabelMachineType] == MachineTypeDPU {
			dpuServices = append(dpuServices, svc)
		}
	}

	assert.Len(t, dpuServices, 2)
	for _, svc := range dpuServices {
		assert.Equal(t, "host-1", svc.Labels[LabelParentMatID])
	}
}

func TestServiceBuilder_BuildServicesFromStatus_SkipsMachinesWithoutBMCIP(t *testing.T) {
	builder := &ServiceBuilder{
		Namespace:    "test-ns",
		BaseSelector: map[string]string{"app": "machine-a-tron"},
	}

	status := &matclient.MachinesStatusResponse{
		Machines: []matclient.MachineStatus{
			{
				MatID: "host-no-ip",
				BMC:   matclient.BMCStatus{Redfish: matclient.EndpointStatus{ReachablePort: 443, ListenPort: 8443}},
				DPUs: []matclient.MachineStatus{
					{
						MatID: "dpu-with-ip",
						BMC:   matclient.BMCStatus{IP: ptr("10.0.0.2"), Redfish: matclient.EndpointStatus{ReachablePort: 443, ListenPort: 8444}},
					},
					{
						MatID: "dpu-empty-ip",
						BMC:   matclient.BMCStatus{IP: ptr(""), Redfish: matclient.EndpointStatus{ReachablePort: 443, ListenPort: 8445}},
					},
				},
			},
		},
	}

	services := builder.BuildServicesFromStatus(status, "")

	require.Len(t, services, 1, "only machines with a BMC IP get a Service")
	assert.Equal(t, "dpu-with-ip", services[0].Labels[LabelMatID])
	assert.Equal(t, "10.0.0.2", services[0].Spec.ClusterIP)
	assert.Equal(t, 3, countMachines(status))
}

func TestComputeServiceDiff(t *testing.T) {
	tests := []struct {
		name            string
		desired         []*corev1.Service
		existing        []*corev1.Service
		wantCreateCount int
		wantUpdateCount int
		wantDeleteCount int
	}{
		{
			name:            "empty to empty",
			desired:         nil,
			existing:        nil,
			wantCreateCount: 0,
			wantUpdateCount: 0,
			wantDeleteCount: 0,
		},
		{
			name: "create new service",
			desired: []*corev1.Service{
				makeTestService("svc-1", "mat-id-1"),
			},
			existing:        nil,
			wantCreateCount: 1,
		},
		{
			name: "dedupe duplicate desired services",
			desired: []*corev1.Service{
				makeTestService("svc-1", "mat-id-1"),
				makeTestService("svc-1", "mat-id-1-duplicate"),
			},
			existing:        nil,
			wantCreateCount: 1,
		},
		{
			name:    "delete stale service",
			desired: nil,
			existing: []*corev1.Service{
				makeTestService("svc-1", "mat-id-1"),
			},
			wantDeleteCount: 1,
		},
		{
			name: "no changes needed",
			desired: []*corev1.Service{
				makeTestService("svc-1", "mat-id-1"),
			},
			existing: []*corev1.Service{
				makeTestService("svc-1", "mat-id-1"),
			},
			wantCreateCount: 0,
			wantUpdateCount: 0,
			wantDeleteCount: 0,
		},
		{
			name: "update port change",
			desired: []*corev1.Service{
				func() *corev1.Service {
					svc := makeTestService("svc-1", "mat-id-1")
					svc.Spec.Ports[0].TargetPort = intstr.FromInt32(9999)
					return svc
				}(),
			},
			existing: []*corev1.Service{
				makeTestService("svc-1", "mat-id-1"),
			},
			wantUpdateCount: 1,
		},
		{
			name: "update annotation change",
			desired: []*corev1.Service{
				func() *corev1.Service {
					svc := makeTestService("svc-1", "mat-id-1")
					svc.Annotations[AnnotationAPIState] = "NotReady"
					return svc
				}(),
			},
			existing: []*corev1.Service{
				makeTestService("svc-1", "mat-id-1"),
			},
			wantUpdateCount: 1,
		},
		{
			name: "ignore foreign label and annotation changes",
			desired: []*corev1.Service{
				makeTestService("svc-1", "mat-id-1"),
			},
			existing: []*corev1.Service{
				func() *corev1.Service {
					svc := makeTestService("svc-1", "mat-id-1")
					svc.Labels["external.example.com/owner"] = "operator"
					svc.Annotations["external.example.com/note"] = "keep"
					return svc
				}(),
			},
			wantUpdateCount: 0,
		},
		{
			name: "remove stale controller-owned optional metadata",
			desired: []*corev1.Service{
				makeTestService("svc-1", "mat-id-1"),
			},
			existing: []*corev1.Service{
				func() *corev1.Service {
					svc := makeTestService("svc-1", "mat-id-1")
					svc.Labels[LabelMachineID] = "old-machine"
					svc.Annotations[AnnotationBMCIP] = "10.0.0.10"
					return svc
				}(),
			},
			wantUpdateCount: 1,
		},
		{
			name: "update owner reference added",
			desired: []*corev1.Service{
				func() *corev1.Service {
					svc := makeTestService("svc-1", "mat-id-1")
					svc.OwnerReferences = []metav1.OwnerReference{
						{
							APIVersion: "apps/v1",
							Kind:       "Deployment",
							Name:       "nico-machine-a-tron",
							UID:        "owner-uid-123",
						},
					}
					return svc
				}(),
			},
			existing: []*corev1.Service{
				makeTestService("svc-1", "mat-id-1"), // no owner reference
			},
			wantUpdateCount: 1,
		},
		{
			name: "update owner reference changed",
			desired: []*corev1.Service{
				func() *corev1.Service {
					svc := makeTestService("svc-1", "mat-id-1")
					svc.OwnerReferences = []metav1.OwnerReference{
						{
							APIVersion: "apps/v1",
							Kind:       "Deployment",
							Name:       "nico-machine-a-tron",
							UID:        "new-owner-uid",
						},
					}
					return svc
				}(),
			},
			existing: []*corev1.Service{
				func() *corev1.Service {
					svc := makeTestService("svc-1", "mat-id-1")
					svc.OwnerReferences = []metav1.OwnerReference{
						{
							APIVersion: "apps/v1",
							Kind:       "Deployment",
							Name:       "nico-machine-a-tron",
							UID:        "old-owner-uid",
						},
					}
					return svc
				}(),
			},
			wantUpdateCount: 1,
		},
		{
			name: "no update when owner reference matches",
			desired: []*corev1.Service{
				func() *corev1.Service {
					svc := makeTestService("svc-1", "mat-id-1")
					svc.OwnerReferences = []metav1.OwnerReference{
						{
							APIVersion: "apps/v1",
							Kind:       "Deployment",
							Name:       "nico-machine-a-tron",
							UID:        "same-uid",
						},
					}
					return svc
				}(),
			},
			existing: []*corev1.Service{
				func() *corev1.Service {
					svc := makeTestService("svc-1", "mat-id-1")
					svc.OwnerReferences = []metav1.OwnerReference{
						{
							APIVersion: "apps/v1",
							Kind:       "Deployment",
							Name:       "nico-machine-a-tron",
							UID:        "same-uid",
						},
					}
					return svc
				}(),
			},
			wantUpdateCount: 0,
		},
		{
			name: "mixed operations",
			desired: []*corev1.Service{
				makeTestService("svc-1", "mat-id-1"), // keep
				makeTestService("svc-2", "mat-id-2"), // create
			},
			existing: []*corev1.Service{
				makeTestService("svc-1", "mat-id-1"), // keep
				makeTestService("svc-3", "mat-id-3"), // delete
			},
			wantCreateCount: 1,
			wantDeleteCount: 1,
		},
		{
			name:    "skip non-managed services on delete",
			desired: nil,
			existing: []*corev1.Service{
				func() *corev1.Service {
					svc := makeTestService("svc-1", "mat-id-1")
					svc.Labels[LabelManagedBy] = "someone-else"
					return svc
				}(),
			},
			wantDeleteCount: 0, // should not delete services we don't manage
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			diff := ComputeServiceDiff(tt.desired, tt.existing)

			assert.Len(t, diff.Create, tt.wantCreateCount)
			assert.Len(t, diff.Update, tt.wantUpdateCount)
			assert.Len(t, diff.Delete, tt.wantDeleteCount)
		})
	}
}

func TestComputeServiceDiff_PreservesForeignMetadataOnUpdate(t *testing.T) {
	desired := makeTestService("svc-1", "mat-id-1")
	desired.Spec.Ports[0].TargetPort = intstr.FromInt32(9999)
	existing := makeTestService("svc-1", "mat-id-1")
	existing.Labels["external.example.com/owner"] = "operator"
	existing.Annotations["external.example.com/note"] = "keep"

	diff := ComputeServiceDiff([]*corev1.Service{desired}, []*corev1.Service{existing})

	require.Len(t, diff.Update, 1)
	assert.Equal(t, "operator", diff.Update[0].Labels["external.example.com/owner"])
	assert.Equal(t, "keep", diff.Update[0].Annotations["external.example.com/note"])
	assert.Equal(t, "9999", diff.Update[0].Spec.Ports[0].TargetPort.String())
}

func TestReconcileLogic(t *testing.T) {
	ctx := context.Background()

	mockK8s := &trackingK8sClient{
		services: make(map[string]*corev1.Service),
	}

	builder := &ServiceBuilder{
		Namespace: "test-ns",
		BaseSelector: map[string]string{
			"app": "machine-a-tron",
		},
	}

	status := &matclient.MachinesStatusResponse{
		Machines: []matclient.MachineStatus{
			{
				MatID:      "host-1234",
				APIState:   "Ready",
				PowerState: "On",
				BMC: matclient.BMCStatus{
					IP: ptr("192.168.1.100"),
					Redfish: matclient.EndpointStatus{
						ReachablePort: 443,
						ListenPort:    8443,
					},
				},
			},
		},
	}

	// First reconcile: should create service
	result := runTestReconcile(ctx, builder, mockK8s, status)
	assert.Equal(t, 1, result.Created)
	assert.Equal(t, 0, result.Updated)
	assert.Equal(t, 0, result.Deleted)
	assert.Empty(t, result.Errors)
	assert.Len(t, mockK8s.services, 1)

	// Second reconcile with same state: no changes
	result = runTestReconcile(ctx, builder, mockK8s, status)
	assert.Equal(t, 0, result.Created)
	assert.Equal(t, 0, result.Updated)
	assert.Equal(t, 0, result.Deleted)

	// Third reconcile with empty status: should delete
	result = runTestReconcile(ctx, builder, mockK8s, &matclient.MachinesStatusResponse{})
	assert.Equal(t, 0, result.Created)
	assert.Equal(t, 0, result.Updated)
	assert.Equal(t, 1, result.Deleted)
	assert.Empty(t, mockK8s.services)
}

// runTestReconcile tests the reconciliation logic without discovery.
func runTestReconcile(ctx context.Context, builder *ServiceBuilder, k8s *trackingK8sClient, status *matclient.MachinesStatusResponse) ReconcileResult {
	result := ReconcileResult{}

	desired := builder.BuildServicesFromStatus(status, "")

	selector := LabelManagedBy + "=" + LabelManagedByValue
	existing, _ := k8s.List(ctx, builder.Namespace, selector)

	diff := ComputeServiceDiff(desired, existing)

	for _, svc := range diff.Create {
		if err := k8s.Create(ctx, svc); err != nil {
			result.Errors = append(result.Errors, err)
		} else {
			result.Created++
		}
	}

	for _, svc := range diff.Update {
		if err := k8s.Update(ctx, svc); err != nil {
			result.Errors = append(result.Errors, err)
		} else {
			result.Updated++
		}
	}

	for _, name := range diff.Delete {
		if err := k8s.Delete(ctx, builder.Namespace, name); err != nil {
			result.Errors = append(result.Errors, err)
		} else {
			result.Deleted++
		}
	}

	return result
}

// makeTestService creates a test service with standard labels.
func makeTestService(name, matID string) *corev1.Service {
	return &corev1.Service{
		ObjectMeta: metav1.ObjectMeta{
			Name:      name,
			Namespace: "test-ns",
			Labels: map[string]string{
				LabelManagedBy:   LabelManagedByValue,
				LabelMatID:       matID,
				LabelMachineType: MachineTypeHost,
			},
			Annotations: map[string]string{
				AnnotationAPIState:   "Ready",
				AnnotationPowerState: "On",
			},
		},
		Spec: corev1.ServiceSpec{
			Ports: []corev1.ServicePort{
				{
					Name:       PortNameRedfish,
					Protocol:   corev1.ProtocolTCP,
					Port:       443,
					TargetPort: intstr.FromInt32(8443),
				},
			},
		},
	}
}

// Mock implementations

// trackingK8sClient is an in-memory K8sServiceClient. Like the API server it
// rejects a create for an existing name and an update or delete for a missing
// one, so the reconciler's adoption and recreate paths are exercised.
type trackingK8sClient struct {
	mu        sync.Mutex
	services  map[string]*corev1.Service
	createErr error
	deleteErr error
}

func (m *trackingK8sClient) List(ctx context.Context, namespace string, labelSelector string) ([]*corev1.Service, error) {
	m.mu.Lock()
	defer m.mu.Unlock()
	result := make([]*corev1.Service, 0)
	for _, svc := range m.services {
		if svc.Labels[LabelManagedBy] == LabelManagedByValue {
			result = append(result, svc)
		}
	}
	return result, nil
}

func (m *trackingK8sClient) Get(ctx context.Context, namespace, name string) (*corev1.Service, error) {
	m.mu.Lock()
	defer m.mu.Unlock()
	svc, ok := m.services[name]
	if !ok {
		return nil, apierrors.NewNotFound(corev1.Resource("services"), name)
	}
	return svc.DeepCopy(), nil
}

func (m *trackingK8sClient) Create(ctx context.Context, svc *corev1.Service) error {
	m.mu.Lock()
	defer m.mu.Unlock()
	if m.createErr != nil {
		return m.createErr
	}
	if _, exists := m.services[svc.Name]; exists {
		return apierrors.NewAlreadyExists(corev1.Resource("services"), svc.Name)
	}
	m.services[svc.Name] = svc.DeepCopy()
	return nil
}

func (m *trackingK8sClient) Update(ctx context.Context, svc *corev1.Service) error {
	m.mu.Lock()
	defer m.mu.Unlock()
	if _, exists := m.services[svc.Name]; !exists {
		return apierrors.NewNotFound(corev1.Resource("services"), svc.Name)
	}
	m.services[svc.Name] = svc.DeepCopy()
	return nil
}

func (m *trackingK8sClient) Delete(ctx context.Context, namespace, name string) error {
	m.mu.Lock()
	defer m.mu.Unlock()
	if m.deleteErr != nil {
		return m.deleteErr
	}
	if _, exists := m.services[name]; !exists {
		return apierrors.NewNotFound(corev1.Resource("services"), name)
	}
	delete(m.services, name)
	return nil
}

func ptr[T any](v T) *T {
	return &v
}

func testMachineStatus() *matclient.MachinesStatusResponse {
	return &matclient.MachinesStatusResponse{
		Machines: []matclient.MachineStatus{{
			MatID:    "mat-id-1",
			APIState: "Ready",
			BMC:      matclient.BMCStatus{IP: ptr("10.0.0.1"), Redfish: matclient.EndpointStatus{ReachablePort: 443, ListenPort: 1266}},
		}},
	}
}

func TestProcessRecreatesConcurrently_RecordsErrors(t *testing.T) {
	tests := []struct {
		name      string
		createErr error
		deleteErr error
		wantErr   string
	}{
		{
			name:      "delete failure",
			deleteErr: fmt.Errorf("delete failed"),
			wantErr:   "deleting service svc-1 for recreate",
		},
		{
			name:      "create failure",
			createErr: fmt.Errorf("create failed"),
			wantErr:   "creating service svc-1 after recreate delete",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			result := ReconcileResult{}
			reconciler := &Reconciler{
				serviceBuilder: &ServiceBuilder{Namespace: "test-ns"},
				k8sClient: &trackingK8sClient{
					services:  map[string]*corev1.Service{"svc-1": makeTestService("svc-1", "mat-id-1")},
					createErr: tt.createErr,
					deleteErr: tt.deleteErr,
				},
				logger:      zerolog.Nop(),
				concurrency: 1,
			}

			recreated := reconciler.processRecreatesConcurrently(context.Background(), []*corev1.Service{makeTestService("svc-1", "mat-id-1")}, &result)

			assert.Equal(t, 0, recreated)
			require.Len(t, result.Errors, 1)
			assert.Contains(t, result.Errors[0].Error(), tt.wantErr)
		})
	}
}

// mockDiscovery implements Discovery for testing.
type mockDiscovery struct {
	instances []DiscoveredInstance
	err       error
}

func (m *mockDiscovery) Discover(ctx context.Context) ([]DiscoveredInstance, error) {
	return m.instances, m.err
}

// mockDeploymentClient implements DeploymentClient for testing.
type mockDeploymentClient struct {
	deployments   map[string]*metav1.OwnerReference
	failFor       map[string]bool
	requestedKeys []string // records deployment names that were looked up
}

func (m *mockDeploymentClient) Get(ctx context.Context, namespace, name string) (*metav1.OwnerReference, error) {
	m.requestedKeys = append(m.requestedKeys, name)
	if m.failFor != nil && m.failFor[name] {
		return nil, fmt.Errorf("deployment %s not found", name)
	}
	if ref, ok := m.deployments[name]; ok {
		return ref, nil
	}
	return nil, fmt.Errorf("deployment %s not found", name)
}

// mockStatusFetcher implements StatusFetcher for testing.
type mockStatusFetcher struct {
	status *matclient.MachinesStatusResponse
	err    error
}

func (m *mockStatusFetcher) GetMachinesStatus(ctx context.Context) (*matclient.MachinesStatusResponse, error) {
	return m.status, m.err
}

func TestReconciler_OwnerReferences(t *testing.T) {
	ip := "10.0.0.1"
	machineStatus := &matclient.MachinesStatusResponse{
		Machines: []matclient.MachineStatus{
			{
				MatID:    "mat-id-12345",
				APIState: "Ready",
				BMC: matclient.BMCStatus{
					IP:      &ip,
					Redfish: matclient.EndpointStatus{ReachablePort: 443, ListenPort: 1266},
				},
			},
		},
	}

	tests := []struct {
		name              string
		instances         []DiscoveredInstance
		deployments       map[string]*metav1.OwnerReference
		failDeployments   map[string]bool
		wantOwnerName     string // expected owner name on created service, empty if no owner
		wantServiceCount  int
		wantDeployLookups []string // expected deployment names to be looked up
	}{
		{
			name: "single-pod mode derives owner from service name",
			instances: []DiscoveredInstance{
				{URL: "https://nico-machine-a-tron-bmc-mock.ns.svc:8443", PodName: "", ServiceName: "nico-machine-a-tron-bmc-mock"},
			},
			deployments: map[string]*metav1.OwnerReference{
				"nico-machine-a-tron": {APIVersion: "apps/v1", Kind: "Deployment", Name: "nico-machine-a-tron", UID: "uid-single"},
			},
			wantOwnerName:     "nico-machine-a-tron",
			wantServiceCount:  1,
			wantDeployLookups: []string{"nico-machine-a-tron"},
		},
		{
			name: "multi-pod mode maps pod to its deployment",
			instances: []DiscoveredInstance{
				{URL: "https://nico-machine-a-tron-mat-0-bmc-mock.ns.svc:8443", PodName: "mat-0", ServiceName: "nico-machine-a-tron-mat-0-bmc-mock"},
			},
			deployments: map[string]*metav1.OwnerReference{
				"nico-machine-a-tron-mat-0": {APIVersion: "apps/v1", Kind: "Deployment", Name: "nico-machine-a-tron-mat-0", UID: "uid-mat-0"},
			},
			wantOwnerName:     "nico-machine-a-tron-mat-0",
			wantServiceCount:  1,
			wantDeployLookups: []string{"nico-machine-a-tron-mat-0"},
		},
		{
			name: "multi-pod mode looks up all deployment names",
			instances: []DiscoveredInstance{
				{URL: "https://nico-machine-a-tron-mat-0-bmc-mock.ns.svc:8443", PodName: "mat-0", ServiceName: "nico-machine-a-tron-mat-0-bmc-mock"},
				{URL: "https://nico-machine-a-tron-mat-1-bmc-mock.ns.svc:8443", PodName: "mat-1", ServiceName: "nico-machine-a-tron-mat-1-bmc-mock"},
			},
			deployments: map[string]*metav1.OwnerReference{
				"nico-machine-a-tron-mat-0": {APIVersion: "apps/v1", Kind: "Deployment", Name: "nico-machine-a-tron-mat-0", UID: "uid-mat-0"},
				"nico-machine-a-tron-mat-1": {APIVersion: "apps/v1", Kind: "Deployment", Name: "nico-machine-a-tron-mat-1", UID: "uid-mat-1"},
			},
			// Both instances return the same machine, so only 1 service is created.
			// The key assertion is that both deployments were looked up.
			wantOwnerName:     "nico-machine-a-tron-mat-0",
			wantServiceCount:  1,
			wantDeployLookups: []string{"nico-machine-a-tron-mat-0", "nico-machine-a-tron-mat-1"},
		},
		{
			name: "failed deployment lookup creates services without owner",
			instances: []DiscoveredInstance{
				{URL: "https://nico-machine-a-tron-bmc-mock.ns.svc:8443", PodName: "", ServiceName: "nico-machine-a-tron-bmc-mock"},
			},
			deployments:       map[string]*metav1.OwnerReference{},
			failDeployments:   map[string]bool{"nico-machine-a-tron": true},
			wantOwnerName:     "", // no owner
			wantServiceCount:  1,
			wantDeployLookups: []string{"nico-machine-a-tron"},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			discovery := &mockDiscovery{instances: tt.instances}
			deployClient := &mockDeploymentClient{
				deployments: tt.deployments,
				failFor:     tt.failDeployments,
			}
			// Use existing mockK8sClient - services map tracks created services
			k8sClient := &trackingK8sClient{services: make(map[string]*corev1.Service)}

			builder := &ServiceBuilder{
				Namespace:    "nico-system",
				BaseSelector: map[string]string{"app.kubernetes.io/name": "nico-machine-a-tron"},
			}

			logger := zerolog.New(zerolog.NewTestWriter(t)).Level(zerolog.Disabled)

			reconciler := NewReconciler(discovery, builder, k8sClient, deployClient, nil, logger)
			reconciler.statusFetcher = func(url string) (StatusFetcher, error) {
				return &mockStatusFetcher{status: machineStatus}, nil
			}

			result := reconciler.Reconcile(context.Background())

			require.Empty(t, result.Errors, "unexpected errors: %v", result.Errors)
			require.Len(t, k8sClient.services, tt.wantServiceCount, "expected %d services created", tt.wantServiceCount)

			// Assert the expected deployment lookups occurred
			assert.ElementsMatch(t, tt.wantDeployLookups, deployClient.requestedKeys, "deployment lookup mismatch")

			if tt.wantServiceCount > 0 {
				// Get first service from map
				var svc *corev1.Service
				for _, s := range k8sClient.services {
					svc = s
					break
				}
				if tt.wantOwnerName != "" {
					require.Len(t, svc.OwnerReferences, 1, "expected owner reference")
					assert.Equal(t, tt.wantOwnerName, svc.OwnerReferences[0].Name)
					assert.Equal(t, "apps/v1", svc.OwnerReferences[0].APIVersion)
					assert.Equal(t, "Deployment", svc.OwnerReferences[0].Kind)
				} else {
					assert.Empty(t, svc.OwnerReferences, "expected no owner reference")
				}
			}
		})
	}
}

// trackingStatusFetcher implements StatusFetcher and tracks calls for testing.
type trackingStatusFetcher struct {
	status     *matclient.MachinesStatusResponse
	err        error
	closed     bool
	fetchCount int
}

func (m *trackingStatusFetcher) GetMachinesStatus(ctx context.Context) (*matclient.MachinesStatusResponse, error) {
	m.fetchCount++
	return m.status, m.err
}

func (m *trackingStatusFetcher) Close() error {
	m.closed = true
	return nil
}

func TestReconciler_ClientCaching(t *testing.T) {
	const url1, url2 = "https://mat-1.ns.svc:8443", "https://mat-2.ns.svc:8443"

	setup := func(urls ...string) (*Reconciler, *mockDiscovery, map[string]*trackingStatusFetcher, *int) {
		instances := make([]DiscoveredInstance, len(urls))
		for i, u := range urls {
			instances[i] = DiscoveredInstance{URL: u, ServiceName: "mat-bmc-mock"}
		}
		discovery := &mockDiscovery{instances: instances}
		fetchers := make(map[string]*trackingStatusFetcher)
		createCount := 0

		r := NewReconciler(discovery, &ServiceBuilder{
			Namespace:    "ns",
			BaseSelector: map[string]string{"app": "mat"},
		}, &trackingK8sClient{services: make(map[string]*corev1.Service)}, nil, nil, zerolog.Nop())

		r.statusFetcher = func(url string) (StatusFetcher, error) {
			createCount++
			f := &trackingStatusFetcher{status: testMachineStatus()}
			fetchers[url] = f
			return f, nil
		}
		return r, discovery, fetchers, &createCount
	}

	t.Run("reuses cached client", func(t *testing.T) {
		r, _, fetchers, createCount := setup(url1)

		r.Reconcile(context.Background())
		assert.Equal(t, 1, *createCount)
		assert.Equal(t, 1, fetchers[url1].fetchCount)

		r.Reconcile(context.Background())
		assert.Equal(t, 1, *createCount, "should reuse cached client")
		assert.Equal(t, 2, fetchers[url1].fetchCount)
	})

	t.Run("evicts and closes stale client", func(t *testing.T) {
		r, discovery, fetchers, _ := setup(url1, url2)

		r.Reconcile(context.Background())
		require.False(t, fetchers[url2].closed)

		discovery.instances = discovery.instances[:1] // remove url2
		r.Reconcile(context.Background())

		assert.False(t, fetchers[url1].closed)
		assert.True(t, fetchers[url2].closed, "evicted client should be closed")
		assert.NotContains(t, r.clientCache, url2)
	})

	t.Run("evicts all clients when discovery returns zero instances", func(t *testing.T) {
		r, discovery, fetchers, _ := setup(url1)

		r.Reconcile(context.Background())
		require.Contains(t, r.clientCache, url1)
		require.False(t, fetchers[url1].closed)

		discovery.instances = nil // all instances gone
		r.Reconcile(context.Background())

		assert.True(t, fetchers[url1].closed, "client should be closed when all instances disappear")
		assert.Empty(t, r.clientCache)
	})
}

func TestProcessDeletesConcurrently_PropagatesErrors(t *testing.T) {
	deleteErr := fmt.Errorf("delete failed")

	reconciler := &Reconciler{
		serviceBuilder: &ServiceBuilder{Namespace: "test-ns"},
		k8sClient: &trackingK8sClient{
			services:  map[string]*corev1.Service{"svc-1": makeTestService("svc-1", "mat-id-1")},
			deleteErr: deleteErr,
		},
		logger:      zerolog.Nop(),
		concurrency: 1,
	}

	result := ReconcileResult{}
	deleted := reconciler.processDeletesConcurrently(context.Background(), []string{"svc-1"}, &result)

	assert.Equal(t, 0, deleted, "should not count failed deletes as deleted")
	require.Len(t, result.Errors, 1, "should propagate delete error to result")
	assert.Contains(t, result.Errors[0].Error(), "deleting service svc-1")
}
