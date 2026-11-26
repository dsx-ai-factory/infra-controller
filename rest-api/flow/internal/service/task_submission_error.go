// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package service

import (
	"context"
	"errors"

	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"

	taskmanager "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/manager"
)

func taskSubmissionStatusError(err error) error {
	if err == nil {
		return nil
	}
	code := taskSubmissionCode(err)
	existing, ok := status.FromError(err)
	if ok && code == existing.Code() {
		return err
	}
	return status.Error(code, err.Error())
}

// A mixed batch must not inherit the status of whichever rack happened to be
// visited first. Internal callers retain the original joined domain errors.
func taskSubmissionCode(err error) codes.Code {
	joined, ok := err.(interface{ Unwrap() []error })
	if ok {
		failures := joined.Unwrap()
		if len(failures) == 0 {
			return codes.Unknown
		}
		code := taskSubmissionCode(failures[0])
		for _, failure := range failures[1:] {
			if taskSubmissionCode(failure) != code {
				return codes.Unknown
			}
		}
		return code
	}
	wrapped, ok := err.(interface{ Unwrap() error })
	if ok {
		return taskSubmissionCode(wrapped.Unwrap())
	}
	switch {
	case errors.Is(err, context.Canceled):
		return codes.Canceled
	case errors.Is(err, context.DeadlineExceeded):
		return codes.DeadlineExceeded
	case errors.Is(err, taskmanager.ErrRackConflict):
		return codes.FailedPrecondition
	case errors.Is(err, taskmanager.ErrQueueFull):
		return codes.ResourceExhausted
	default:
		return status.Code(err)
	}
}
