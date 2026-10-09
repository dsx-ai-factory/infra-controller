// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package sourcelist

import (
	"context"

	"github.com/NVIDIA/infra-controller/dev/k8s/machine-a-tron-controller/pkg/controller"
)

// fromDiscovered converts discovery results into sources. The Service name is
// used as the machine-a-tron identity.
func fromDiscovered(instances []controller.DiscoveredInstance) []Source {
	out := make([]Source, 0, len(instances))
	for _, inst := range instances {
		out = append(out, Source{
			Name:    inst.ServiceName,
			BaseURL: inst.URL,
			Pod:     inst.PodName,
		})
	}
	return out
}

// observingDiscovery forwards successful discovery results to a Registry.
type observingDiscovery struct {
	inner    controller.Discovery
	registry *Registry
}

// WrapDiscovery returns a Discovery that records every successful result in
// the registry before returning it to the caller. Failed discoveries leave
// the registry untouched so the last known good set stays published.
func WrapDiscovery(inner controller.Discovery, registry *Registry) controller.Discovery {
	return &observingDiscovery{inner: inner, registry: registry}
}

// Discover implements controller.Discovery.
func (d *observingDiscovery) Discover(ctx context.Context) ([]controller.DiscoveredInstance, error) {
	instances, err := d.inner.Discover(ctx)
	if err != nil {
		return nil, err
	}
	d.registry.Observe(fromDiscovered(instances))
	return instances, nil
}
