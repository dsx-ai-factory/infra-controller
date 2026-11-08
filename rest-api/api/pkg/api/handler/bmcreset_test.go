// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"context"
	"encoding/json"
	"net/http"
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"google.golang.org/protobuf/encoding/protojson"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

func checkResetMachineBMCHandlerProxiesRequest(t *testing.T) {
	t.Helper()
	fixture := common.NewTestSetupProviderMachineHandlerFixture(t, &corev1.AdminBmcResetResponse{})
	handler := NewResetMachineBMCHandler(fixture.DBSession, fixture.SiteClientPool, fixture.Config)

	rec := fixture.Request(t, handler.Handle, http.MethodPatch, "/", model.APIMachineBMCResetRequest{UseIpmiTool: true}, "")
	assert.Equal(t, http.StatusAccepted, rec.Code)
	assert.Equal(t, corev1.Forge_AdminBmcReset_FullMethodName, fixture.ProxiedReq.FullMethod)
	assert.Empty(t, fixture.ProxiedReq.EncryptedSecrets)

	var coreReq corev1.AdminBmcResetRequest
	require.NoError(t, protojson.Unmarshal(fixture.ProxiedReq.RequestJSON, &coreReq))
	assert.Equal(t, fixture.MachineID, coreReq.GetMachineId())
	assert.True(t, coreReq.GetUseIpmitool())
	var resp model.APIMessageResponse
	require.NoError(t, json.Unmarshal(rec.Body.Bytes(), &resp))
	assert.Equal(t, "Machine BMC reset request was accepted", resp.Message)
	assert.NotContains(t, rec.Body.String(), "password")
}

func checkResetMachineBMCHandlerDefaultsUseIpmiTool(t *testing.T) {
	t.Helper()
	fixture := common.NewTestSetupProviderMachineHandlerFixture(t, nil)
	handler := NewResetMachineBMCHandler(fixture.DBSession, fixture.SiteClientPool, fixture.Config)

	rec := fixture.Request(t, handler.Handle, http.MethodPatch, "/", nil, "")
	assert.Equal(t, http.StatusAccepted, rec.Code)
	assert.Equal(t, corev1.Forge_AdminBmcReset_FullMethodName, fixture.ProxiedReq.FullMethod)

	var coreReq corev1.AdminBmcResetRequest
	require.NoError(t, protojson.Unmarshal(fixture.ProxiedReq.RequestJSON, &coreReq))
	assert.False(t, coreReq.GetUseIpmitool())
}

func checkResetMachineBMCHandlerRejectsMachineWithInstanceNoAcknowledgement(t *testing.T) {
	t.Helper()
	fixture := common.NewTestSetupProviderMachineHandlerFixture(t, nil)
	handler := NewResetMachineBMCHandler(fixture.DBSession, fixture.SiteClientPool, fixture.Config)
	isAssigned := true
	_, err := cdbm.NewMachineDAO(fixture.DBSession).Update(context.Background(), nil, cdbm.MachineUpdateInput{
		MachineID:  fixture.MachineID,
		IsAssigned: &isAssigned,
	})
	require.NoError(t, err)

	rec := fixture.Request(t, handler.Handle, http.MethodPatch, "/", model.APIMachineBMCResetRequest{UseIpmiTool: true}, "")
	assert.Equal(t, http.StatusBadRequest, rec.Code)
	assert.Empty(t, fixture.ProxiedReq.FullMethod)
}

func checkResetMachineBMCHandlerAllowsMachineWithInstanceAcknowledgement(t *testing.T) {
	t.Helper()
	fixture := common.NewTestSetupProviderMachineHandlerFixture(t, nil)
	handler := NewResetMachineBMCHandler(fixture.DBSession, fixture.SiteClientPool, fixture.Config)
	isAssigned := true
	_, err := cdbm.NewMachineDAO(fixture.DBSession).Update(context.Background(), nil, cdbm.MachineUpdateInput{
		MachineID:  fixture.MachineID,
		IsAssigned: &isAssigned,
	})
	require.NoError(t, err)
	acknowledgeAttachedInstance := true

	rec := fixture.Request(t, handler.Handle, http.MethodPatch, "/", model.APIMachineBMCResetRequest{
		UseIpmiTool:                 true,
		AcknowledgeAttachedInstance: &acknowledgeAttachedInstance,
	}, "")
	assert.Equal(t, http.StatusAccepted, rec.Code)
	assert.Equal(t, corev1.Forge_AdminBmcReset_FullMethodName, fixture.ProxiedReq.FullMethod)
}

func TestResetMachineBMCHandler_Handle(t *testing.T) {
	for _, tc := range []struct {
		name string
		run  func(*testing.T)
	}{
		{name: "ProxiesRequest", run: checkResetMachineBMCHandlerProxiesRequest},
		{name: "DefaultsUseIpmiTool", run: checkResetMachineBMCHandlerDefaultsUseIpmiTool},
		{name: "RejectsMachineWithInstanceNoAcknowledgement", run: checkResetMachineBMCHandlerRejectsMachineWithInstanceNoAcknowledgement},
		{name: "AllowsMachineWithInstanceAcknowledgement", run: checkResetMachineBMCHandlerAllowsMachineWithInstanceAcknowledgement},
		{name: "membership precedes request and resource validation", run: func(t *testing.T) {
			checkMachineMembershipGate(t, (ResetMachineBMCHandler{}).Handle)
		}},
	} {
		t.Run(tc.name, tc.run)
	}
}
