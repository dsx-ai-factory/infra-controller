// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package compat holds deprecated, source-compatible wrappers around
// standard SDK constructors whose signatures changed as a side effect
// of optional-field changes in the OpenAPI spec (which the generator
// reflects by dropping the corresponding constructor argument).
//
// The wrappers live here rather than under `sdk/standard/helpers`
// because the `standard` package already imports `helpers` (for
// pagination and API-name rewriting), so `helpers` cannot import
// `standard` back. This sibling package can safely depend on
// `standard` without creating an import cycle.

package compat

import standard "github.com/NVIDIA/infra-controller/rest-api/sdk/standard"

// NewInstanceCreateRequestWithInterfaces preserves the pre-`auto`
// `NewInstanceCreateRequest` signature, which took an explicit
// `interfaces` argument before that field became optional on the
// REST API. Callers should migrate to
// `standard.NewInstanceCreateRequest(name, tenantId, vpcId)` followed
// by `SetInterfaces` (or `SetAutoNetwork` for zero-DPU instances).
//
// Deprecated: use `standard.NewInstanceCreateRequest` + `SetInterfaces`.
func NewInstanceCreateRequestWithInterfaces(name, tenantID, vpcID string, interfaces []standard.InterfaceCreateRequest) *standard.InstanceCreateRequest {
	req := standard.NewInstanceCreateRequest(name, tenantID, vpcID)
	req.SetInterfaces(interfaces)
	return req
}

// NewBatchInstanceCreateRequestWithInterfaces preserves the pre-`auto`
// `NewBatchInstanceCreateRequest` signature, which took an explicit
// `interfaces` argument before that field became optional. Callers
// should migrate to `standard.NewBatchInstanceCreateRequest(...)`
// followed by `SetInterfaces` (or `SetAutoNetwork` for zero-DPU batches).
//
// Deprecated: use `standard.NewBatchInstanceCreateRequest` + `SetInterfaces`.
func NewBatchInstanceCreateRequestWithInterfaces(namePrefix string, count int32, tenantID, instanceTypeID, vpcID string, interfaces []standard.InterfaceCreateRequest) *standard.BatchInstanceCreateRequest {
	req := standard.NewBatchInstanceCreateRequest(namePrefix, count, tenantID, instanceTypeID, vpcID)
	req.SetInterfaces(interfaces)
	return req
}
