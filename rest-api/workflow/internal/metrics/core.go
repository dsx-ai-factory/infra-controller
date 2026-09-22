// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package metrics

import (
	"github.com/prometheus/client_golang/prometheus"
)

type coreMetrics struct {
	Info *prometheus.GaugeVec
}

// NewCoreMetrics creates a new coreMetrics struct and registers the metrics with the provided registerer
func NewCoreMetrics(reg prometheus.Registerer, namespace string) *coreMetrics {
	m := &coreMetrics{
		Info: prometheus.NewGaugeVec(prometheus.GaugeOpts{
			Namespace: namespace,
			Name:      "info",
			Help:      "Information about the Cloud/Site worker",
		}, []string{"version", "namespace"}),
	}

	reg.MustRegister(m.Info)

	return m
}
