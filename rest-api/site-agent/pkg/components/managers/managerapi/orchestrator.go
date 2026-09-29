// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package managerapi

import "context"

// OrchestratorExpansion - Orchestrator Expansion
type OrchestratorExpansion interface{}

// OrchestratorInterface - interface to Orchestrator
type OrchestratorInterface interface {
	// List all the apis of Orchestrator here
	Init()
	Start()
	GetState() []string
	CheckLiveness() error
	CheckReadiness(ctx context.Context) error

	OrchestratorExpansion
}
