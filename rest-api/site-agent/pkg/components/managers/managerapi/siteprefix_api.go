// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package managerapi

// SitePrefixInterface publishes inventory without a CRUD subscriber.
type SitePrefixInterface interface {
	Init()
	RegisterPublisher() error
	RegisterCron() error
}
