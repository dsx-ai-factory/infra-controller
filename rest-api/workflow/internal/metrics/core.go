// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package metrics

import (
	"github.com/prometheus/client_golang/prometheus"

	"github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/metadata"
)

type coreMetrics struct {
	Info *prometheus.GaugeVec
}

// NewCoreMetrics creates and registers worker information metrics.
func NewCoreMetrics(reg prometheus.Registerer, metricsNamespace, workerNamespace string) *coreMetrics {
	m := &coreMetrics{
		Info: prometheus.NewGaugeVec(prometheus.GaugeOpts{
			Namespace: metricsNamespace,
			Name:      "info",
			Help:      "Information about the Cloud/Site worker",
		}, []string{"version", "namespace"}),
	}

	reg.MustRegister(m.Info)
	m.Info.WithLabelValues(metadata.Version, workerNamespace).Set(1)

	return m
}
