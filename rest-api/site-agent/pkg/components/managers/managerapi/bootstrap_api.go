// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package managerapi

import "context"

// BootstrapExpansion - Bootstrap Expansion
type BootstrapExpansion interface{}

// BootstrapInterface - interface to Bootstrap
type BootstrapInterface interface {
	// List all the apis of Bootstrap here
	Init()
	Start()
	DownloadAndStoreCreds(ctx context.Context, otpOverride []byte) error
	CheckRegistration() error

	BootstrapExpansion

	// Temporal Workflows - Subscriber
	RegisterSubscriber() error
}
