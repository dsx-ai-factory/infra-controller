// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package config

import (
	"fmt"
)

// HealthzConfig holds configuration of Healthz
type HealthzConfig struct {
	Enabled bool
	Port    int
}

// GetListenAddr returns the local address for listen socket.
func (hccfg *HealthzConfig) GetListenAddr() string {
	return fmt.Sprintf(":%v", hccfg.Port)
}

// NewHealthzConfig initializes and returns a configuration object for managing Healthz
func NewHealthzConfig(enabled bool, port int) *HealthzConfig {
	return &HealthzConfig{
		Enabled: enabled,
		Port:    port,
	}
}
