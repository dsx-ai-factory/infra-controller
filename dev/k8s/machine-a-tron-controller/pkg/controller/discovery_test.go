// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package controller

import (
	"context"
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	corev1 "k8s.io/api/core/v1"
	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
	"k8s.io/apimachinery/pkg/runtime"
	"k8s.io/client-go/kubernetes/fake"
)

func TestMatPodDiscovery_Discover(t *testing.T) {
	tests := []struct {
		name          string
		services      []runtime.Object
		labelSelector string
		wantInstances []DiscoveredInstance
		wantErr       bool
	}{
		{
			name:          "no services found",
			services:      nil,
			wantInstances: nil,
		},
		{
			name: "discovers service with redfish port 8443",
			services: []runtime.Object{
				&corev1.Service{
					ObjectMeta: metav1.ObjectMeta{
						Name:      "nico-machine-a-tron-bmc-mock",
						Namespace: "nico-system",
						Labels: map[string]string{
							"nvidia-infra-controller/mat-service": "true",
							LabelPodName:                          "mat-0",
						},
					},
					Spec: corev1.ServiceSpec{
						Ports: []corev1.ServicePort{
							{
								Name: "redfish",
								Port: 8443,
							},
						},
					},
				},
			},
			wantInstances: []DiscoveredInstance{
				{
					URL:         "https://nico-machine-a-tron-bmc-mock.nico-system.svc.cluster.local:8443",
					PodName:     "mat-0",
					ServiceName: "nico-machine-a-tron-bmc-mock",
				},
			},
		},
		{
			name: "discovers service with bmc-mock port name",
			services: []runtime.Object{
				&corev1.Service{
					ObjectMeta: metav1.ObjectMeta{
						Name:      "mat-service",
						Namespace: "nico-system",
						Labels: map[string]string{
							"nvidia-infra-controller/mat-service": "true",
						},
					},
					Spec: corev1.ServiceSpec{
						Ports: []corev1.ServicePort{
							{
								Name: "bmc-mock",
								Port: 9000,
							},
						},
					},
				},
			},
			wantInstances: []DiscoveredInstance{
				{
					URL:     "https://mat-service.nico-system.svc.cluster.local:9000",
					PodName: "", ServiceName: "mat-service",
				},
			},
		},
		{
			name: "falls back to first port when named port not found",
			services: []runtime.Object{
				&corev1.Service{
					ObjectMeta: metav1.ObjectMeta{
						Name:      "mat-service",
						Namespace: "nico-system",
						Labels: map[string]string{
							"nvidia-infra-controller/mat-service": "true",
						},
					},
					Spec: corev1.ServiceSpec{
						Ports: []corev1.ServicePort{
							{
								Name: "https",
								Port: 443,
							},
						},
					},
				},
			},
			wantInstances: []DiscoveredInstance{
				{
					URL:     "https://mat-service.nico-system.svc.cluster.local:443",
					PodName: "", ServiceName: "mat-service",
				},
			},
		},
		{
			name: "uses named redfish port even when it matches default",
			services: []runtime.Object{
				&corev1.Service{
					ObjectMeta: metav1.ObjectMeta{
						Name:      "mat-service",
						Namespace: "nico-system",
						Labels: map[string]string{
							"nvidia-infra-controller/mat-service": "true",
						},
					},
					Spec: corev1.ServiceSpec{
						Ports: []corev1.ServicePort{
							{
								Name: "metrics",
								Port: 9090,
							},
							{
								Name: "redfish",
								Port: 1266,
							},
						},
					},
				},
			},
			wantInstances: []DiscoveredInstance{
				{
					URL:     "https://mat-service.nico-system.svc.cluster.local:1266",
					PodName: "", ServiceName: "mat-service",
				},
			},
		},
		{
			name: "uses default port when service has no ports",
			services: []runtime.Object{
				&corev1.Service{
					ObjectMeta: metav1.ObjectMeta{
						Name:      "mat-service",
						Namespace: "nico-system",
						Labels: map[string]string{
							"nvidia-infra-controller/mat-service": "true",
						},
					},
					Spec: corev1.ServiceSpec{
						Ports: []corev1.ServicePort{},
					},
				},
			},
			wantInstances: []DiscoveredInstance{
				{
					URL:     "https://mat-service.nico-system.svc.cluster.local:1266",
					PodName: "", ServiceName: "mat-service",
				},
			},
		},
		{
			name: "discovers multiple services",
			services: []runtime.Object{
				&corev1.Service{
					ObjectMeta: metav1.ObjectMeta{
						Name:      "mat-0-bmc-mock",
						Namespace: "nico-system",
						Labels: map[string]string{
							"nvidia-infra-controller/mat-service": "true",
							LabelPodName:                          "mat-0",
						},
					},
					Spec: corev1.ServiceSpec{
						Ports: []corev1.ServicePort{
							{Name: "redfish", Port: 8443},
						},
					},
				},
				&corev1.Service{
					ObjectMeta: metav1.ObjectMeta{
						Name:      "mat-1-bmc-mock",
						Namespace: "nico-system",
						Labels: map[string]string{
							"nvidia-infra-controller/mat-service": "true",
							LabelPodName:                          "mat-1",
						},
					},
					Spec: corev1.ServiceSpec{
						Ports: []corev1.ServicePort{
							{Name: "redfish", Port: 8443},
						},
					},
				},
			},
			wantInstances: []DiscoveredInstance{
				{
					URL:     "https://mat-0-bmc-mock.nico-system.svc.cluster.local:8443",
					PodName: "mat-0", ServiceName: "mat-0-bmc-mock",
				},
				{
					URL:     "https://mat-1-bmc-mock.nico-system.svc.cluster.local:8443",
					PodName: "mat-1", ServiceName: "mat-1-bmc-mock",
				},
			},
		},
		{
			name: "filters by label selector",
			services: []runtime.Object{
				&corev1.Service{
					ObjectMeta: metav1.ObjectMeta{
						Name:      "mat-service",
						Namespace: "nico-system",
						Labels: map[string]string{
							"nvidia-infra-controller/mat-service": "true",
						},
					},
					Spec: corev1.ServiceSpec{
						Ports: []corev1.ServicePort{
							{Name: "redfish", Port: 8443},
						},
					},
				},
				&corev1.Service{
					ObjectMeta: metav1.ObjectMeta{
						Name:      "other-service",
						Namespace: "nico-system",
						Labels: map[string]string{
							"app": "something-else",
						},
					},
					Spec: corev1.ServiceSpec{
						Ports: []corev1.ServicePort{
							{Name: "http", Port: 80},
						},
					},
				},
			},
			wantInstances: []DiscoveredInstance{
				{
					URL:     "https://mat-service.nico-system.svc.cluster.local:8443",
					PodName: "", ServiceName: "mat-service",
				},
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			clientset := fake.NewSimpleClientset(tt.services...)

			labelSelector := tt.labelSelector
			if labelSelector == "" {
				labelSelector = DefaultDiscoverySelector
			}

			discovery := NewMatPodDiscovery(clientset, "nico-system", labelSelector)
			instances, err := discovery.Discover(context.Background())

			if tt.wantErr {
				require.Error(t, err)
				return
			}
			require.NoError(t, err)
			assert.Equal(t, tt.wantInstances, instances)
		})
	}
}

func TestNewMatPodDiscovery_DefaultSelector(t *testing.T) {
	clientset := fake.NewSimpleClientset()
	discovery := NewMatPodDiscovery(clientset, "test-ns", "")

	assert.Equal(t, DefaultDiscoverySelector, discovery.labelSelector)
}

func TestNewMatPodDiscovery_CustomSelector(t *testing.T) {
	clientset := fake.NewSimpleClientset()
	customSelector := "app=my-app"
	discovery := NewMatPodDiscovery(clientset, "test-ns", customSelector)

	assert.Equal(t, customSelector, discovery.labelSelector)
}
