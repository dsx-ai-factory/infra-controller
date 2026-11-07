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
	authz "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

func TestReprovisionMachineDpuHandlerProxiesRequest(t *testing.T) {
	fixture := common.NewTestSetupProviderMachineHandlerFixture(t, nil)
	handler := NewReprovisionMachineDpuHandler(fixture.DBSession, fixture.SiteClientPool, fixture.Config)

	rec := fixture.Request(t, handler.Handle, http.MethodPatch, "/", model.APIMachineDpuReprovisionRequest{Mode: model.MachineDpuReprovisionModeRestart, UpdateFirmware: true}, "")
	assert.Equal(t, http.StatusAccepted, rec.Code)
	assert.Equal(t, corev1.Forge_TriggerDpuReprovisioning_FullMethodName, fixture.ProxiedReq.FullMethod)
	assert.Empty(t, fixture.ProxiedReq.EncryptedSecrets)

	var coreReq corev1.DpuReprovisioningRequest
	require.NoError(t, protojson.Unmarshal(fixture.ProxiedReq.RequestJSON, &coreReq))
	assert.Equal(t, fixture.MachineID, coreReq.GetMachineId().GetId())
	assert.Equal(t, corev1.DpuReprovisioningRequest_Restart, coreReq.GetMode())
	assert.Equal(t, corev1.UpdateInitiator_AdminCli, coreReq.GetInitiator())
	assert.True(t, coreReq.GetUpdateFirmware())
	var resp model.APIMessageResponse
	require.NoError(t, json.Unmarshal(rec.Body.Bytes(), &resp))
	assert.Equal(t, "DPU reprovisioning request was accepted", resp.Message)
}

func TestReprovisionMachineDpuHandlerAllowsPrivilegedTenant(t *testing.T) {
	fixture := common.NewTestSetupProviderMachineHandlerFixture(t, nil)
	handler := NewReprovisionMachineDpuHandler(fixture.DBSession, fixture.SiteClientPool, fixture.Config)
	ctx := context.Background()

	tenantOrg := "test-tenant-org"
	tenantUser := common.TestBuildUser(t, fixture.DBSession, "test-tenant-starfleet-id", tenantOrg, []string{authz.TenantAdminRole})
	tenant := common.TestBuildTenant(t, fixture.DBSession, "test-tenant", tenantOrg, tenantUser)

	machine, err := cdbm.NewMachineDAO(fixture.DBSession).GetByID(ctx, nil, fixture.MachineID, []string{}, false)
	require.NoError(t, err)
	provider, err := cdbm.NewInfrastructureProviderDAO(fixture.DBSession).GetByID(ctx, nil, machine.InfrastructureProviderID, []string{})
	require.NoError(t, err)
	common.TestBuildTenantAccountWithTargetedInstanceCreation(t, fixture.DBSession, provider, &tenant.ID, tenant.Org, cdbm.TenantAccountStatusReady, tenantUser)

	fixture.Org = tenantOrg
	fixture.User = tenantUser
	rec := fixture.Request(t, handler.Handle, http.MethodPatch, "/", model.APIMachineDpuReprovisionRequest{Mode: model.MachineDpuReprovisionModeRestart, UpdateFirmware: true}, "")
	assert.Equal(t, http.StatusAccepted, rec.Code)
	assert.Equal(t, corev1.Forge_TriggerDpuReprovisioning_FullMethodName, fixture.ProxiedReq.FullMethod)

	var coreReq corev1.DpuReprovisioningRequest
	require.NoError(t, protojson.Unmarshal(fixture.ProxiedReq.RequestJSON, &coreReq))
	assert.Equal(t, fixture.MachineID, coreReq.GetMachineId().GetId())
	assert.Equal(t, corev1.DpuReprovisioningRequest_Restart, coreReq.GetMode())
	assert.True(t, coreReq.GetUpdateFirmware())
}

func TestReprovisionMachineDpuHandlerRejectsMachineWithInstanceNoAcknowledgement(t *testing.T) {
	fixture := common.NewTestSetupProviderMachineHandlerFixture(t, nil)
	handler := NewReprovisionMachineDpuHandler(fixture.DBSession, fixture.SiteClientPool, fixture.Config)
	isAssigned := true
	_, err := cdbm.NewMachineDAO(fixture.DBSession).Update(context.Background(), nil, cdbm.MachineUpdateInput{
		MachineID:  fixture.MachineID,
		IsAssigned: &isAssigned,
	})
	require.NoError(t, err)

	rec := fixture.Request(t, handler.Handle, http.MethodPatch, "/", model.APIMachineDpuReprovisionRequest{Mode: model.MachineDpuReprovisionModeRestart}, "")
	assert.Equal(t, http.StatusBadRequest, rec.Code)
	assert.Empty(t, fixture.ProxiedReq.FullMethod)
}

func TestReprovisionMachineDpuHandlerAllowsMachineWithInstanceAcknowledgement(t *testing.T) {
	fixture := common.NewTestSetupProviderMachineHandlerFixture(t, nil)
	handler := NewReprovisionMachineDpuHandler(fixture.DBSession, fixture.SiteClientPool, fixture.Config)
	isAssigned := true
	_, err := cdbm.NewMachineDAO(fixture.DBSession).Update(context.Background(), nil, cdbm.MachineUpdateInput{
		MachineID:  fixture.MachineID,
		IsAssigned: &isAssigned,
	})
	require.NoError(t, err)
	acknowledgeAttachedInstance := true

	rec := fixture.Request(t, handler.Handle, http.MethodPatch, "/", model.APIMachineDpuReprovisionRequest{
		Mode:                        model.MachineDpuReprovisionModeRestart,
		AcknowledgeAttachedInstance: &acknowledgeAttachedInstance,
	}, "")
	assert.Equal(t, http.StatusAccepted, rec.Code)
	assert.Equal(t, corev1.Forge_TriggerDpuReprovisioning_FullMethodName, fixture.ProxiedReq.FullMethod)
}
