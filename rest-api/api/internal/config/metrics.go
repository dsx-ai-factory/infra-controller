// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package config

import (
	"fmt"
)

// DefaultMetricsNamespace prefixes every metric this server exposes and matches
// its nico-rest-api Helm service name. Operators override it with
// metrics.namespace. Deliberately independent of api.name, which is the URL path
// segment callers route on and has no bearing on how these series are named.
const DefaultMetricsNamespace = "nico_rest_api"

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
