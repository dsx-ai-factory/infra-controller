// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package nicoapi

import (
	"context"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// GetComponentFirmwareDetails retains the caller's shorter read budget while
// borrowing the existing connection; it never submits or advances an update.
func (c *grpcClient) GetComponentFirmwareDetails(ctx context.Context, request *corev1.GetComponentFirmwareDetailsRequest) (*corev1.GetComponentFirmwareDetailsResponse, error) {
	ctx, cancel := context.WithTimeout(ctx, c.grpcTimeout)
	defer cancel()
	return c.gclient.GetComponentFirmwareDetails(ctx, request)
}
