// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package common

import (
	"context"
	"testing"

	"github.com/google/uuid"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	tmocks "go.temporal.io/sdk/mocks"
	tp "go.temporal.io/sdk/temporal"
	"google.golang.org/protobuf/encoding/protojson"
	"google.golang.org/protobuf/proto"

	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/grpcproxy"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	swe "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/error"
)

// The mocked Site verifies the transport boundary. Core ID locking and REST
// PostgreSQL claim serialization require separate executable proof.
func expectDomainFenceCall(t *testing.T, c *tmocks.Client, method string, reply proto.Message, resultErr error, validate func(grpcproxy.Request)) {
	t.Helper()
	run := &tmocks.WorkflowRun{}
	run.On("Get", mock.Anything, mock.Anything).Run(func(args mock.Arguments) {
		if reply != nil {
			b, err := protojson.Marshal(reply)
			require.NoError(t, err)
			args.Get(1).(*grpcproxy.Response).ResponseJSON = b
		}
	}).Return(resultErr).Once()
	c.On("ExecuteWorkflow", mock.Anything, mock.Anything, grpcproxy.Core.WorkflowName,
		mock.MatchedBy(func(req grpcproxy.Request) bool {
			if req.FullMethod != method {
				return false
			}
			if validate != nil {
				validate(req)
			}
			return true
		})).Return(run, nil).Once()
	t.Cleanup(func() { run.AssertExpectations(t) })
}

func TestReservedDomainRejectionFence_NeverAdoptsLegacyIDAndRequiresConfirmedCancellation(t *testing.T) {
	id := uuid.New()
	name := "owned.example.com"
	siteID := uuid.NewString()
	for _, tc := range []struct {
		name             string
		found            *corev1.DomainList
		replayErr        error
		cancelErr        error
		ready, confirmed bool
	}{
		{name: "absent ID cancellation confirmed", found: &corev1.DomainList{}, confirmed: true},
		{name: "absent ID cancellation result lost", found: &corev1.DomainList{}, cancelErr: tp.NewTimeoutError(1, nil)},
		{name: "matching legacy ID replay rejected", found: &corev1.DomainList{Domains: []*corev1.Domain{{Id: &corev1.DomainId{Value: id.String()}, Name: name}}}, replayErr: tp.NewNonRetryableApplicationError("not reserved", swe.ErrTypeNICoFailedPrecondition, nil)},
		{name: "matching reserved ID replay confirmed", found: &corev1.DomainList{Domains: []*corev1.Domain{{Id: &corev1.DomainId{Value: id.String()}, Name: name}}}, ready: true, confirmed: true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			c := &tmocks.Client{}
			var calls []string
			check := func(req grpcproxy.Request) { calls = append(calls, req.FullMethod) }
			expectDomainFenceCall(t, c, corev1.Forge_FindDomain_FullMethodName, tc.found, nil, check)
			if len(tc.found.GetDomains()) != 0 {
				expectDomainFenceCall(t, c, corev1.Forge_CreateDomain_FullMethodName, &corev1.Domain{Id: &corev1.DomainId{Value: id.String()}, Name: name}, tc.replayErr, func(req grpcproxy.Request) {
					check(req)
					var payload corev1.CreateDomainRequest
					require.NoError(t, protojson.Unmarshal(req.RequestJSON, &payload))
					require.Equal(t, id.String(), payload.GetReservedId().GetValue())
					require.Equal(t, name, payload.GetName())
				})
			} else {
				expectDomainFenceCall(t, c, corev1.Forge_DeleteDomain_FullMethodName, nil, tc.cancelErr, func(req grpcproxy.Request) {
					check(req)
					var payload corev1.DomainDeletionRequest
					require.NoError(t, protojson.Unmarshal(req.RequestJSON, &payload))
					require.Equal(t, id.String(), payload.GetId().GetValue())
					require.True(t, payload.GetCancelReservedId())
				})
			}
			ready, confirmed := ReservedDomainRejectionFence(context.Background(), c, id, name, siteID)
			require.Equal(t, tc.ready, ready)
			require.Equal(t, tc.confirmed, confirmed)
			require.Len(t, calls, 2)
			c.AssertExpectations(t)
		})
	}
}
