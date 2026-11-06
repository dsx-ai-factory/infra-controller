// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package nicoapi

import (
	"context"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

func (c *grpcClient) ComponentReset(ctx context.Context, req *corev1.ComponentResetRequest) (*corev1.ComponentResetResponse, error) {
	ctx, cancel := context.WithTimeout(ctx, c.grpcTimeout)
	defer cancel()
	return c.gclient.ComponentReset(ctx, req)
}
func (c *grpcClient) GetComponentResetStatus(ctx context.Context, req *corev1.GetComponentResetStatusRequest) (*corev1.GetComponentResetStatusResponse, error) {
	ctx, cancel := context.WithTimeout(ctx, c.grpcTimeout)
	defer cancel()
	return c.gclient.GetComponentResetStatus(ctx, req)
}

// The shared mock deliberately returns an empty response; reset-specific tests
// inject explicit accepted results instead of treating absence as success.
func (c *mockClient) ComponentReset(context.Context, *corev1.ComponentResetRequest) (*corev1.ComponentResetResponse, error) {
	return &corev1.ComponentResetResponse{}, nil
}
func (c *mockClient) GetComponentResetStatus(context.Context, *corev1.GetComponentResetStatusRequest) (*corev1.GetComponentResetStatusResponse, error) {
	return &corev1.GetComponentResetStatusResponse{}, nil
}
