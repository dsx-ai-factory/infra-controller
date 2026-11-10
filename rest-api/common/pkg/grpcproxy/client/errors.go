// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package client

import (
	"errors"
	"net/http"

	"go.temporal.io/api/serviceerror"
	tp "go.temporal.io/sdk/temporal"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	swe "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/error"
)

// UnwrapWorkflowError removes Temporal wrappers and maps backend errors to HTTP status codes.
func UnwrapWorkflowError(err error) (code int, unwrappedError error) {
	code = http.StatusInternalServerError

	// Attempt to unwrap our way through Temporal's WorkflowExecutionError
	// and ActivityError layers to reach the underlying cause. These types
	// contain Temporal-internal details (workflow IDs, run IDs, scheduled
	// event IDs, etc) that generally shouldn't be getting exposed to users.
	innerErr := err

	var wfErr *tp.WorkflowExecutionError
	if errors.As(innerErr, &wfErr) {
		if cause := errors.Unwrap(wfErr); cause != nil {
			innerErr = cause
		}
	}

	var actErr *tp.ActivityError
	if errors.As(innerErr, &actErr) {
		if cause := errors.Unwrap(actErr); cause != nil {
			innerErr = cause
		}
	}

	unwrappedError = innerErr

	// if the error chain contains a gRPC error code use it to tune our HTTP response code
	// NOTE: this is duplicating some feature of grpc-gateway and not exhaustive
	s, ok := status.FromError(innerErr)
	if ok {
		// NOTE: this matches WrapErr in site-workflow/pkg/error/error.go
		switch s.Code() {
		case codes.NotFound:
			code = http.StatusNotFound
		case codes.Unimplemented:
			code = http.StatusNotImplemented
		case codes.Unavailable:
			code = http.StatusServiceUnavailable
		case codes.PermissionDenied:
			code = http.StatusForbidden
		case codes.AlreadyExists:
			code = http.StatusConflict
		case codes.FailedPrecondition:
			code = http.StatusPreconditionFailed
		case codes.InvalidArgument:
			code = http.StatusBadRequest
		case codes.ResourceExhausted:
			code = http.StatusTooManyRequests
		}
	}

	// if the error is NOT a Temporal ApplicationError return what we have
	tpError := &tp.ApplicationError{}
	if !errors.As(innerErr, &tpError) {
		return
	}

	// Tune HTTP status code using the application error type coming back from Site.
	switch tpError.Type() {
	case swe.ErrTypeInvalidRequest:
		code = http.StatusBadRequest
	case swe.ErrTypeNICoObjectNotFound, swe.ErrTypeCarbideObjectNotFound:
		code = http.StatusNotFound
	case swe.ErrTypeNICoUnimplemented, swe.ErrTypeCarbideUnimplemented:
		code = http.StatusNotImplemented
	case swe.ErrTypeNICoDenied, swe.ErrTypeCarbideDenied:
		code = http.StatusForbidden
	case swe.ErrTypeNICoUnavailable, swe.ErrTypeCarbideUnavailable:
		code = http.StatusServiceUnavailable
	case swe.ErrTypeNICoAlreadyExists, swe.ErrTypeCarbideAlreadyExists:
		code = http.StatusConflict
	case swe.ErrTypeNICoFailedPrecondition, swe.ErrTypeCarbideFailedPrecondition:
		code = http.StatusPreconditionFailed
	case swe.ErrTypeNICoInvalidArgument, swe.ErrTypeCarbideInvalidArgument:
		code = http.StatusBadRequest
	case swe.ErrTypeNICoResourceExhausted:
		code = http.StatusTooManyRequests
	}

	// if the error is an internal Temporal error it is mostly useless so we unwrap it but we keep
	// the current error if there is no unwrapped error
	if potentialError := errors.Unwrap(tpError); potentialError != nil {
		unwrappedError = potentialError
	}

	return
}

// attemptFailure distinguishes a definitive Core reply from missing or ambiguous history.
func attemptFailure(resultErr error) (*cutil.APIError, bool) {
	code, werr := UnwrapWorkflowError(resultErr)
	coreResultObserved := false
	var activityErr *tp.ActivityError
	var applicationErr *tp.ApplicationError
	if errors.As(resultErr, &activityErr) && activityErr.ActivityType().GetName() == "InvokeCoreGRPCOnSite" && errors.As(resultErr, &applicationErr) {
		switch applicationErr.Type() {
		case swe.ErrTypeNICoObjectNotFound, swe.ErrTypeCarbideObjectNotFound,
			swe.ErrTypeNICoUnimplemented, swe.ErrTypeCarbideUnimplemented,
			swe.ErrTypeNICoDenied, swe.ErrTypeCarbideDenied,
			swe.ErrTypeNICoAlreadyExists, swe.ErrTypeCarbideAlreadyExists,
			swe.ErrTypeNICoFailedPrecondition, swe.ErrTypeCarbideFailedPrecondition,
			swe.ErrTypeNICoInvalidArgument, swe.ErrTypeCarbideInvalidArgument:
			coreResultObserved = true
		}
	}
	var missingWorkflow *serviceerror.NotFound
	var missingNamespace *serviceerror.NamespaceNotFound
	if !coreResultObserved && (code == http.StatusNotFound || errors.As(resultErr, &missingWorkflow) || errors.As(resultErr, &missingNamespace)) {
		// A missing workflow or namespace is not a missing Core resource.
		code = http.StatusServiceUnavailable
	}
	return cutil.NewAPIError(code, werr.Error(), nil).WithCause(resultErr), coreResultObserved
}
