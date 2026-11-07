// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package managerapi

// SiteInterface is the Site manager interface.
type SiteInterface interface {
	Init()
	RegisterPublisher() error
	RegisterCron() error
	GetState() []string
}
