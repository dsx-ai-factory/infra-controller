// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"slices"

	"go.temporal.io/sdk/temporal"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	swe "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/error"
)

// skuComponentReplacementRejected recognizes failures that happen before
// `ReplaceSkuComponents` runs. The capability probe and mutation can reach
// different Agent or Core versions during an upgrade, so a positive probe
// alone does not rule out these rejections. Other failures may follow a
// completed mutation and must not trigger another replacement.
// Keep these checks while REST supports sites that lack the new RPC, including
// v2.0 Agents with the Core-only proxy and v2.2.2 Agents with the shared proxy.
func skuComponentReplacementRejected(err error) bool {
	apiErr, ok := err.(*cutil.APIError)
	if ok {
		err = apiErr.Unwrap()
	}
	workflowErr, ok := err.(*temporal.WorkflowExecutionError)
	if ok {
		err = workflowErr.Unwrap()
	}
	activityErr, ok := err.(*temporal.ActivityError)
	if !ok || activityErr.ActivityType().GetName() != "InvokeCoreGRPCOnSite" {
		return false
	}
	appErr, ok := activityErr.Unwrap().(*temporal.ApplicationError)
	if !ok || appErr.HasDetails() {
		return false
	}

	// Temporal records native Go type names: fmt.wrapError becomes "wrapError",
	// gRPC's status.Error becomes "Error", and errors.errorString becomes "".
	if appErr.Type() != "wrapError" {
		if !slices.Contains(swe.UnimplementedOrDeniedErrTypes(), appErr.Type()) {
			return false
		}
		cause, isApplicationError := appErr.Unwrap().(*temporal.ApplicationError)
		if !isApplicationError || !appErr.NonRetryable() || cause.NonRetryable() ||
			cause.HasDetails() || cause.Message() != appErr.Message() {
			return false
		}

		if cause.Type() == "Error" && cause.Unwrap() == nil {
			switch appErr.Type() {
			case swe.ErrTypeNICoUnimplemented, swe.ErrTypeCarbideUnimplemented:
				// Tonic 0.14's unmatched-method response has no status message.
				return cause.Message() == "rpc error: code = Unimplemented desc = "
			case swe.ErrTypeNICoDenied, swe.ErrTypeCarbideDenied:
				// Core's auth middleware returns this raw HTTP 403 before
				// dispatch. It proves rejection, not missing RPC support;
				// the fallback RPC still needs its own authorization.
				return cause.Message() == "rpc error: code = PermissionDenied desc = unexpected HTTP status code received from server: 403 (Forbidden); malformed header: missing HTTP content-type"
			}
		}

		// From v2.3.0-rc.1, InvokeCoreGRPCOnSite adds NICoUnimplemented
		// around the descriptor error that older Agents returned directly.
		if appErr.Type() != swe.ErrTypeNICoUnimplemented {
			return false
		}
		appErr = cause
	}

	// Descriptor lookup fails before the Agent invokes Core. Check the full
	// serialized chain: an HTTP status or matching fragment is not enough.
	if appErr.Type() != "wrapError" || appErr.NonRetryable() || appErr.HasDetails() {
		return false
	}
	cause, ok := appErr.Unwrap().(*temporal.ApplicationError)
	if !ok || cause.Type() != "" || cause.NonRetryable() || cause.HasDetails() || cause.Unwrap() != nil {
		return false
	}
	switch appErr.Message() {
	case `unknown proxied gRPC method: "ReplaceSkuComponents" on "forge.Forge"`:
		return cause.Message() == "unknown proxied gRPC method"
	case `unknown NICo Core method: "ReplaceSkuComponents"`:
		return cause.Message() == "unknown NICo Core method"
	default:
		return false
	}
}
