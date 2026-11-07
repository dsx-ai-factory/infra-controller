// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package queue

const (
	// CloudTaskQueue handles all tasks triggered by Cloud API and
	// are meant to be consumed by Cloud system worker
	CloudTaskQueue = "cloud"
	// SiteTaskQueue handles tasks submitted by Site agents running on Site management clusters
	SiteTaskQueue = "site"
)
