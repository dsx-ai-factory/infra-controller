// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package service

import (
	"testing"

	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"

	pb "github.com/dsx-ai-factory/infra-controller/rest-api/flow/pkg/proto/v1"
)

func TestFlowServerImpl_CreateOperationRule(t *testing.T) {
	// A nil store proves the invalid definition is rejected before any
	// persistence call is attempted.
	_, err := (&FlowServerImpl{}).CreateOperationRule(
		t.Context(),
		&pb.CreateOperationRuleRequest{
			RuleDefinitionJson: `{"version":"v999","steps":[]}`,
		},
	)

	require.Error(t, err)
	assert.Equal(t, codes.InvalidArgument, status.Code(err))
	assert.ErrorContains(t, err, "unsupported rule definition version: v999")
}

func TestFlowServerImpl_UpdateOperationRule(t *testing.T) {
	unsupportedRuleDefinition := `{"version":"v999","steps":[]}`

	// A nil store proves the invalid definition is rejected before any
	// persistence call is attempted.
	_, err := (&FlowServerImpl{}).UpdateOperationRule(
		t.Context(),
		&pb.UpdateOperationRuleRequest{
			RuleId:             &pb.UUID{Id: uuid.NewString()},
			RuleDefinitionJson: &unsupportedRuleDefinition,
		},
	)

	require.Error(t, err)
	assert.Equal(t, codes.InvalidArgument, status.Code(err))
	assert.ErrorContains(t, err, "unsupported rule definition version: v999")
}
