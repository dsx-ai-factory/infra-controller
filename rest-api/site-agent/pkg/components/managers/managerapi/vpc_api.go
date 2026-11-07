// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package managerapi

// VPCExpansion - VPC Expansion
type VPCExpansion interface{}

// VPCInterface - interface to VPC
type VPCInterface interface {
	// List all the apis of VPC here
	Init()
	RegisterSubscriber() error
	RegisterPublisher() error
	RegisterCron() error

	GetState() []string
	VPCExpansion
}
