// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package config

import (
	"fmt"
)

// DefaultMetricsNamespace prefixes every metric this worker exposes and matches
// its nico-rest-workflow Helm service name. Operators override it with
// metrics.namespace.
const DefaultMetricsNamespace = "nico_rest_workflow"

// MetricsConfig holds configuration of Metrics
type MetricsConfig struct {
	Enabled   bool
	Port      int
	Namespace string
}

// GetListenAddr returns the local address for listen socket.
func (mcfg *MetricsConfig) GetListenAddr() string {
	return fmt.Sprintf(":%v", mcfg.Port)
}

// NewMetricsConfig initializes and returns a configuration object for managing Metrics
func NewMetricsConfig(enabled bool, port int, namespace string) *MetricsConfig {
	return &MetricsConfig{
		Enabled:   enabled,
		Port:      port,
		Namespace: namespace,
	}
}
