// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package server

import (
	"context"
	"testing"

	"github.com/gogo/status"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"google.golang.org/grpc/codes"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

func TestInvokeInstancePower(t *testing.T) {
	const instanceID = "12345678-1234-5678-90ab-cdef01234567"

	tests := []struct {
		name          string
		request       *corev1.InstancePowerRequest
		wantCode      codes.Code
		wantMessage   string
		wantResultNil bool
	}{
		{
			name:          "nil request",
			wantCode:      codes.InvalidArgument,
			wantMessage:   "Invalid request argument",
			wantResultNil: true,
		},
		{
			name:          "missing instance ID",
			request:       &corev1.InstancePowerRequest{},
			wantCode:      codes.InvalidArgument,
			wantMessage:   "Invalid request argument",
			wantResultNil: true,
		},
		{
			name: "empty instance ID",
			request: &corev1.InstancePowerRequest{
				InstanceId: &corev1.InstanceId{},
			},
			wantCode:      codes.InvalidArgument,
			wantMessage:   "Invalid request argument",
			wantResultNil: true,
		},
		{
			name: "reset existing instance",
			request: &corev1.InstancePowerRequest{
				InstanceId: &corev1.InstanceId{Value: instanceID},
				Operation:  corev1.InstancePowerRequest_POWER_RESET,
			},
			wantCode: codes.OK,
		},
		{
			name: "invalid operation for existing instance",
			request: &corev1.InstancePowerRequest{
				InstanceId: &corev1.InstanceId{Value: instanceID},
				Operation:  corev1.InstancePowerRequest_Operation(1),
			},
			wantCode:    codes.InvalidArgument,
			wantMessage: "Invalid operation in request",
		},
		{
			name: "unknown instance",
			request: &corev1.InstancePowerRequest{
				InstanceId: &corev1.InstanceId{Value: "87654321-4321-8765-09ba-fedcba987654"},
				Operation:  corev1.InstancePowerRequest_POWER_RESET,
			},
			wantCode:      codes.NotFound,
			wantMessage:   `Instance with ID "87654321-4321-8765-09ba-fedcba987654" not found`,
			wantResultNil: true,
		},
	}

	server := &NICoServerImpl{
		ins: map[string]*corev1.Instance{
			instanceID: {Id: &corev1.InstanceId{Value: instanceID}},
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			result, err := server.InvokeInstancePower(context.Background(), test.request)
			require.Equal(t, test.wantCode, status.Code(err))
			if test.wantMessage != "" {
				assert.Equal(t, test.wantMessage, status.Convert(err).Message())
			}
			assert.Equal(t, test.wantResultNil, result == nil)
		})
	}
}

func TestReencryptTenantIdentitySecrets(t *testing.T) {
	const orgID = "test-org"
	server := &NICoServerImpl{
		identityState: map[string]*identityOrgState{
			orgID: {slot1: &identityKeyMaterial{}, slot2: &identityKeyMaterial{}, currentSlot: 1},
		},
		tokenDelegations:       map[string]*corev1.TokenDelegationResponse{orgID: {}},
		currentEncryptionKeyID: mockCurrentEncryptionKeyID,
		reencryptedOrgs:        make(map[string]string),
	}

	t.Run("nil request", func(t *testing.T) {
		_, err := server.ReencryptTenantIdentitySecrets(context.Background(), nil)
		require.Equal(t, codes.InvalidArgument, status.Code(err))
	})

	t.Run("dry run reports would-change without mutating state", func(t *testing.T) {
		resp, err := server.ReencryptTenantIdentitySecrets(context.Background(),
			&corev1.ReencryptTenantIdentitySecretsRequest{DryRun: true})
		require.NoError(t, err)
		assert.Equal(t, uint32(1), resp.GetRowsExamined())
		assert.Equal(t, uint32(1), resp.GetRowsUpdated())
		assert.Equal(t, uint32(3), resp.GetFieldsReencrypted()) // two signing slots + delegation
		assert.Equal(t, uint32(0), resp.GetRowsSkippedAllOnTarget())
		assert.Equal(t, mockCurrentEncryptionKeyID, resp.GetCurrentEncryptionKeyId())
		assert.Empty(t, server.reencryptedOrgs, "dry run must not stamp org state")
	})

	t.Run("apply re-wraps and stamps state", func(t *testing.T) {
		resp, err := server.ReencryptTenantIdentitySecrets(context.Background(),
			&corev1.ReencryptTenantIdentitySecretsRequest{})
		require.NoError(t, err)
		assert.Equal(t, uint32(1), resp.GetRowsUpdated())
		assert.Equal(t, uint32(3), resp.GetFieldsReencrypted())
		assert.Equal(t, mockCurrentEncryptionKeyID, server.reencryptedOrgs[orgID])
	})

	t.Run("second apply is idempotent", func(t *testing.T) {
		resp, err := server.ReencryptTenantIdentitySecrets(context.Background(),
			&corev1.ReencryptTenantIdentitySecretsRequest{})
		require.NoError(t, err)
		assert.Equal(t, uint32(0), resp.GetRowsUpdated())
		assert.Equal(t, uint32(1), resp.GetRowsSkippedAllOnTarget())
		assert.Equal(t, uint32(3), resp.GetFieldsSkippedOnTarget())
	})

	t.Run("missing org filter returns not found", func(t *testing.T) {
		resp, err := server.ReencryptTenantIdentitySecrets(context.Background(),
			&corev1.ReencryptTenantIdentitySecretsRequest{OrganizationId: getStrPtr("other-org")})
		assert.Nil(t, resp)
		require.Equal(t, codes.NotFound, status.Code(err))
		assert.Equal(t, `Identity configuration not found for org "other-org"`, status.Convert(err).Message())
	})
}
