// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package controller

import (
	"context"
	"fmt"

	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
	"k8s.io/client-go/kubernetes"
)

// DefaultDiscoverySelector is the default label selector for discovering
// machine-a-tron bmc-mock Services.
const DefaultDiscoverySelector = "nvidia-infra-controller/mat-service=true"

// DefaultBMCMockPort is the default port for bmc-mock if not found in Service spec.
const DefaultBMCMockPort = 1266

// DiscoveredInstance represents a discovered machine-a-tron instance.
type DiscoveredInstance struct {
	URL         string
	PodName     string
	ServiceName string // Used to derive the Deployment name for owner references
}

// MatPodDiscovery discovers machine-a-tron pods via their bmc-mock Services.
type MatPodDiscovery struct {
	clientset     kubernetes.Interface
	namespace     string
	labelSelector string
}

// NewMatPodDiscovery creates a new MatPodDiscovery.
// If labelSelector is empty, uses DefaultDiscoverySelector.
func NewMatPodDiscovery(clientset kubernetes.Interface, namespace string, labelSelector string) *MatPodDiscovery {
	if labelSelector == "" {
		labelSelector = DefaultDiscoverySelector
	}
	return &MatPodDiscovery{
		clientset:     clientset,
		namespace:     namespace,
		labelSelector: labelSelector,
	}
}

// Discover finds all machine-a-tron bmc-mock services and returns their URLs with pod names.
// The port is derived from the Service's "redfish" port, falling back to DefaultBMCMockPort.
func (d *MatPodDiscovery) Discover(ctx context.Context) ([]DiscoveredInstance, error) {
	services, err := d.clientset.CoreV1().Services(d.namespace).List(ctx, metav1.ListOptions{
		LabelSelector: d.labelSelector,
	})
	if err != nil {
		return nil, fmt.Errorf("listing services with selector %q: %w", d.labelSelector, err)
	}

	var instances []DiscoveredInstance
	for _, svc := range services.Items {
		// Find the port from Service spec (look for "redfish" or use first port)
		port := DefaultBMCMockPort
		foundNamedPort := false
		for _, p := range svc.Spec.Ports {
			if p.Name == "redfish" || p.Name == "bmc-mock" {
				port = int(p.Port)
				foundNamedPort = true
				break
			}
		}
		// Fallback to first port if named port not found
		if !foundNamedPort && len(svc.Spec.Ports) > 0 {
			port = int(svc.Spec.Ports[0].Port)
		}

		url := fmt.Sprintf("https://%s.%s.svc.cluster.local:%d", svc.Name, d.namespace, port)
		podName := svc.Labels[LabelPodName]
		instances = append(instances, DiscoveredInstance{
			URL:         url,
			PodName:     podName,
			ServiceName: svc.Name,
		})
	}

	return instances, nil
}
