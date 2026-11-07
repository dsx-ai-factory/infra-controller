// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package errors

import (
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"
)

func GRPCErrorAlreadyExists(msg string) error {
	return status.Error(codes.AlreadyExists, msg)
}

func GRPCErrorNotFound(msg string) error {
	return status.Error(codes.NotFound, msg)
}

func GRPCErrorInvalidArgument(msg string) error {
	return status.Error(codes.InvalidArgument, msg)
}

func GRPCErrorInternal(msg string) error {
	return status.Error(codes.Internal, msg)
}

func IsGRPCError(err error) bool {
	_, ok := status.FromError(err)
	return ok
}
