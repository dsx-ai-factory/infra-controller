// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package managerapi

// SSHKeyGroupExpansion - SSHKeyGroup Expansion
type SSHKeyGroupExpansion interface{}

// SSHKeyGroupInterface - interface to SSHKeyGroup
type SSHKeyGroupInterface interface {
	// List all the apis of SSHKeyGroup here
	Init()
	RegisterSubscriber() error
	RegisterPublisher() error
	RegisterCron() error

	GetState() []string
	SSHKeyGroupExpansion
}
