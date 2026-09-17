// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package activity

import (
	"context"
	"testing"

	"github.com/stretchr/testify/require"
	"go.temporal.io/sdk/temporal"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"

	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/grpcproxy"
	swe "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/error"
)

func TestInvokeGRPCProxyOnSite(t *testing.T) {
	_, err := invokeGRPCProxyOnSite(
		context.Background(),
		grpcproxy.Flow,
		"InvokeFlowGRPCOnSite",
		proxyErrorInvoker{err: status.Error(codes.FailedPrecondition, "task cannot be cancelled")},
		"",
		grpcproxy.Request{FullMethod: "/v1.Flow/CancelTask", RequestJSON: []byte(`{}`)},
	)
	require.Error(t, err)

	// REST receives a deserialized Temporal error, not the original gRPC error.
	converter := temporal.NewDefaultFailureConverter(temporal.DefaultFailureConverterOptions{})
	decoded := converter.FailureToError(converter.ErrorToFailure(err))
	var applicationErr *temporal.ApplicationError
	require.ErrorAs(t, decoded, &applicationErr)
	require.Equal(t, swe.ErrTypeNICoFailedPrecondition, applicationErr.Type())
	require.True(t, applicationErr.NonRetryable())
}

// proxyErrorInvoker supplies a local proxy failure without calling a backend.
type proxyErrorInvoker struct {
	err error
}

// InvokeJSON returns the failure configured by the test.
func (p proxyErrorInvoker) InvokeJSON(context.Context, string, []byte) ([]byte, error) {
	return nil, p.err
}
