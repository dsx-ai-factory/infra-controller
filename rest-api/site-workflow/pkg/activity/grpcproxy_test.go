// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package activity

import (
	"context"
	"errors"
	"fmt"
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"go.temporal.io/sdk/temporal"

	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/grpcproxy"
	swe "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/error"
	"github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/grpc/client"
)

func TestInvokeGRPCProxyOnSite(t *testing.T) {
	cases := []struct {
		name         string
		err          error
		errType      string
		nonRetryable bool
	}{
		{
			name:         "missing local descriptor remains typed across Temporal",
			err:          fmt.Errorf("%w: FutureMethod", client.ErrUnknownProxyMethod),
			errType:      swe.ErrTypeNICoUnimplemented,
			nonRetryable: true,
		},
		{name: "unrelated local error is not reported as unsupported", err: errors.New("invalid request JSON")},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			_, err := invokeGRPCProxyOnSite(context.Background(), grpcproxy.Core, "InvokeCoreGRPCOnSite",
				proxyErrorInvoker{err: tc.err}, "", grpcproxy.Request{FullMethod: "/forge.Forge/FutureMethod", RequestJSON: []byte(`{}`)})
			require.Error(t, err)

			// REST receives a deserialized Temporal error, not the original sentinel.
			converter := temporal.NewDefaultFailureConverter(temporal.DefaultFailureConverterOptions{})
			decoded := converter.FailureToError(converter.ErrorToFailure(err))
			var applicationErr *temporal.ApplicationError
			require.ErrorAs(t, decoded, &applicationErr)
			assert.Equal(t, tc.errType, applicationErr.Type())
			assert.Equal(t, tc.nonRetryable, applicationErr.NonRetryable())
		})
	}
}

// proxyErrorInvoker supplies a local proxy failure without calling a backend.
type proxyErrorInvoker struct {
	err error
}

// InvokeJSON returns the failure configured by the test.
func (p proxyErrorInvoker) InvokeJSON(context.Context, string, []byte) ([]byte, error) {
	return nil, p.err
}
