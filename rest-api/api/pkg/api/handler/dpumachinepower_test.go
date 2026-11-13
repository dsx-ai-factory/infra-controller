// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"context"
	"encoding/json"
	"net/http"
	"testing"

	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"
	"google.golang.org/protobuf/encoding/protojson"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	authz "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

func TestUpdateDpuMachinePowerHandler_Handle(t *testing.T) {
	const (
		assignedMessage = "DPU Machine's host Machine is currently in use by an Instance, set acknowledgeAttachedInstance to true to proceed"
		unknownMessage  = "Could not determine whether the DPU Machine's host Machine is in use by an Instance, set acknowledgeAttachedInstance to true to proceed"
		validateMessage = "Error validating DPU Machine power update request data"
		notFoundMessage = "Could not find DPU Machine with specified ID"
	)
	resolveMethods := []string{corev1.Forge_FindMachineIds_FullMethodName, corev1.Forge_FindMachinesByIds_FullMethodName}
	powerMethods := append(append([]string{}, resolveMethods...), corev1.Forge_AdminPowerControl_FullMethodName)

	// dpuOn returns the Core DPU Machine "dpu-1" attached to hostMachineID;
	// an empty hostMachineID leaves the DPU without a host.
	dpuOn := func(hostMachineID string) *corev1.Machine {
		machine := &corev1.Machine{Id: &corev1.MachineId{Id: "dpu-1"}, MachineType: corev1.MachineType_DPU}
		if hostMachineID != "" {
			machine.Status = &corev1.MachineStatus{AssociatedHostMachineId: &corev1.MachineId{Id: hostMachineID}}
		}
		return machine
	}
	assignHost := func(t *testing.T, f *dpuMachineCoreFixture) {
		_, err := cdbm.NewMachineDAO(f.dbSession).Update(context.Background(), nil, cdbm.MachineUpdateInput{
			MachineID:  f.hostMachineID,
			IsAssigned: cutil.GetPtr(true),
		})
		require.NoError(t, err)
	}
	// body renders a request body for the fixture Site with the given extra JSON members.
	body := func(f *dpuMachineCoreFixture, members string) string {
		return `{"siteId":"` + f.siteID + `"` + members + `}`
	}

	tests := []struct {
		name string
		// host selects the host Machine ID Core reports for the DPU; nil uses the fixture host Machine.
		host func(*testing.T, *dpuMachineCoreFixture) string
		// machine overrides the Core Machine returned for the DPU ID.
		machine *corev1.Machine
		// searchMachineIDs overrides the DPU IDs Core lists; nil lists "dpu-1".
		searchMachineIDs []string
		setup            func(*testing.T, *dpuMachineCoreFixture)
		powerError       error
		target           func(*dpuMachineCoreFixture) string
		body             func(*dpuMachineCoreFixture) string
		wantStatus       int
		wantMessage      string
		// wantCoreMessage is the Core status message expected within the proxy's rendering of it.
		wantCoreMessage string
		wantMethods     []string
		wantAction      corev1.AdminPowerControlRequest_SystemPowerControl
	}{
		{
			name:        "proxies the action for a DPU whose host has no Instance",
			body:        func(f *dpuMachineCoreFixture) string { return body(f, `,"action":"ForceRestart"`) },
			wantStatus:  http.StatusAccepted,
			wantMessage: "power control accepted",
			wantMethods: powerMethods,
			wantAction:  corev1.AdminPowerControlRequest_ForceRestart,
		},
		{
			name:        "proxies the action for a DPU that Core reports without a host",
			host:        func(*testing.T, *dpuMachineCoreFixture) string { return "" },
			body:        func(f *dpuMachineCoreFixture) string { return body(f, `,"action":"On"`) },
			wantStatus:  http.StatusAccepted,
			wantMessage: "power control accepted",
			wantMethods: powerMethods,
			wantAction:  corev1.AdminPowerControlRequest_On,
		},
		{
			name:        "rejects a host with an Instance when unacknowledged",
			setup:       assignHost,
			body:        func(f *dpuMachineCoreFixture) string { return body(f, `,"action":"ForceRestart"`) },
			wantStatus:  http.StatusBadRequest,
			wantMessage: assignedMessage,
			wantMethods: resolveMethods,
		},
		{
			name:  "rejects a host with an Instance when acknowledgement is false",
			setup: assignHost,
			body: func(f *dpuMachineCoreFixture) string {
				return body(f, `,"action":"ForceRestart","acknowledgeAttachedInstance":false`)
			},
			wantStatus:  http.StatusBadRequest,
			wantMessage: assignedMessage,
			wantMethods: resolveMethods,
		},
		{
			name:  "proxies the action for a host with an Instance when acknowledged",
			setup: assignHost,
			body: func(f *dpuMachineCoreFixture) string {
				return body(f, `,"action":"ACPowercycle","acknowledgeAttachedInstance":true`)
			},
			wantStatus:  http.StatusAccepted,
			wantMessage: "power control accepted",
			wantMethods: powerMethods,
			wantAction:  corev1.AdminPowerControlRequest_ACPowercycle,
		},
		{
			name:        "rejects a host that REST does not know when unacknowledged",
			host:        func(*testing.T, *dpuMachineCoreFixture) string { return "host-not-in-rest" },
			body:        func(f *dpuMachineCoreFixture) string { return body(f, `,"action":"ForceRestart"`) },
			wantStatus:  http.StatusBadRequest,
			wantMessage: unknownMessage,
			wantMethods: resolveMethods,
		},
		{
			name: "proxies the action for a host that REST does not know when acknowledged",
			host: func(*testing.T, *dpuMachineCoreFixture) string { return "host-not-in-rest" },
			body: func(f *dpuMachineCoreFixture) string {
				return body(f, `,"action":"ForceRestart","acknowledgeAttachedInstance":true`)
			},
			wantStatus:  http.StatusAccepted,
			wantMessage: "power control accepted",
			wantMethods: powerMethods,
			wantAction:  corev1.AdminPowerControlRequest_ForceRestart,
		},
		{
			name: "rejects a host that REST records under another Site when unacknowledged",
			host: func(t *testing.T, f *dpuMachineCoreFixture) string {
				otherOrg := "other-org"
				otherUser := common.TestBuildUser(t, f.dbSession, "other-starfleet-id", otherOrg, []string{authz.ProviderAdminRole})
				otherProvider := common.TestBuildInfrastructureProvider(t, f.dbSession, "Other Infrastructure Provider", otherOrg, otherUser)
				otherSite := common.TestBuildSite(t, f.dbSession, otherProvider, "Other Site", otherUser)
				otherIT := common.TestBuildInstanceType(t, f.dbSession, "other-instance-type", cutil.GetPtr(otherSite.ID), otherSite, nil, otherUser)
				return common.TestBuildMachine(t, f.dbSession, otherProvider, otherSite, &otherIT.ID, cutil.GetPtr("test-controller-machine-type"), cdbm.MachineStatusReady).ID
			},
			body:        func(f *dpuMachineCoreFixture) string { return body(f, `,"action":"ForceRestart"`) },
			wantStatus:  http.StatusBadRequest,
			wantMessage: unknownMessage,
			wantMethods: resolveMethods,
		},
		{
			name:            "surfaces a Core rejection of the power action",
			powerError:      testMachineCoreFailure(status.Error(codes.FailedPrecondition, "DPU BMC is not reachable")),
			body:            func(f *dpuMachineCoreFixture) string { return body(f, `,"action":"ForceRestart"`) },
			wantStatus:      http.StatusPreconditionFailed,
			wantCoreMessage: "DPU BMC is not reachable",
			wantMethods:     powerMethods,
			wantAction:      corev1.AdminPowerControlRequest_ForceRestart,
		},
		{
			name:             "returns not found for an ID Core does not list as a DPU",
			searchMachineIDs: []string{"dpu-2"},
			body:             func(f *dpuMachineCoreFixture) string { return body(f, `,"action":"ForceRestart"`) },
			wantStatus:       http.StatusNotFound,
			wantMessage:      notFoundMessage,
			wantMethods:      resolveMethods[:1],
		},
		{
			name:        "returns not found for a host Machine ID",
			machine:     &corev1.Machine{Id: &corev1.MachineId{Id: "dpu-1"}, MachineType: corev1.MachineType_HOST},
			body:        func(f *dpuMachineCoreFixture) string { return body(f, `,"action":"ForceRestart"`) },
			wantStatus:  http.StatusNotFound,
			wantMessage: notFoundMessage,
			wantMethods: resolveMethods,
		},
		{
			name:        "rejects a body without a Site ID",
			body:        func(*dpuMachineCoreFixture) string { return `{"action":"ForceRestart"}` },
			wantStatus:  http.StatusBadRequest,
			wantMessage: validateMessage,
		},
		{
			name:        "ignores a Site ID supplied as a query parameter",
			target:      func(f *dpuMachineCoreFixture) string { return "/?siteId=" + f.siteID },
			body:        func(*dpuMachineCoreFixture) string { return `{"action":"ForceRestart"}` },
			wantStatus:  http.StatusBadRequest,
			wantMessage: validateMessage,
		},
		{
			name:        "rejects a malformed Site ID",
			body:        func(*dpuMachineCoreFixture) string { return `{"siteId":"not-a-uuid","action":"ForceRestart"}` },
			wantStatus:  http.StatusBadRequest,
			wantMessage: validateMessage,
		},
		{
			name: "rejects a Site that does not exist",
			body: func(*dpuMachineCoreFixture) string {
				return `{"siteId":"` + uuid.NewString() + `","action":"ForceRestart"}`
			},
			wantStatus:  http.StatusBadRequest,
			wantMethods: nil,
		},
		{
			name:        "rejects an unsupported action",
			body:        func(f *dpuMachineCoreFixture) string { return body(f, `,"action":"forcecycle"`) },
			wantStatus:  http.StatusBadRequest,
			wantMessage: validateMessage,
		},
		{
			name:        "rejects a body that is not valid JSON",
			body:        func(*dpuMachineCoreFixture) string { return `{"siteId":` },
			wantStatus:  http.StatusBadRequest,
			wantMessage: "Failed to parse request data, potentially invalid structure",
		},
		{
			name:        "rejects the Tenant Admin role",
			setup:       func(_ *testing.T, f *dpuMachineCoreFixture) { f.setUserRoles(authz.TenantAdminRole) },
			body:        func(f *dpuMachineCoreFixture) string { return body(f, `,"action":"ForceRestart"`) },
			wantStatus:  http.StatusForbidden,
			wantMessage: "User does not have Provider Admin role with org",
		},
		{
			name:        "rejects the Provider Viewer role",
			setup:       func(_ *testing.T, f *dpuMachineCoreFixture) { f.setUserRoles(authz.ProviderViewerRole) },
			body:        func(f *dpuMachineCoreFixture) string { return body(f, `,"action":"ForceRestart"`) },
			wantStatus:  http.StatusForbidden,
			wantMessage: "User does not have Provider Admin role with org",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			fixture := newDpuMachineCoreFixture(t)
			if tt.setup != nil {
				tt.setup(t, fixture)
			}

			// Queue only the Core results the scenario is expected to consume,
			// so an unexpected proxied call fails on the unprepared mock.
			if len(tt.wantMethods) >= 1 {
				searchMachineIDs := tt.searchMachineIDs
				if searchMachineIDs == nil {
					searchMachineIDs = []string{"dpu-1"}
				}
				machineIDs := make([]*corev1.MachineId, 0, len(searchMachineIDs))
				for _, machineID := range searchMachineIDs {
					machineIDs = append(machineIDs, &corev1.MachineId{Id: machineID})
				}
				fixture.expectProxyResponse(t, &corev1.MachineIdList{MachineIds: machineIDs})
			}
			if len(tt.wantMethods) >= 2 {
				machine := tt.machine
				if machine == nil {
					hostMachineID := fixture.hostMachineID
					if tt.host != nil {
						hostMachineID = tt.host(t, fixture)
					}
					machine = dpuOn(hostMachineID)
				}
				fixture.expectProxyResponse(t, &corev1.MachineList{Machines: []*corev1.Machine{machine}})
			}
			if len(tt.wantMethods) >= 3 {
				if tt.powerError != nil {
					fixture.expectProxyError(tt.powerError)
				} else {
					fixture.expectProxyResponse(t, &corev1.AdminPowerControlResponse{Msg: cutil.GetPtr("power control accepted")})
				}
			}

			target := "/"
			if tt.target != nil {
				target = tt.target(fixture)
			}
			handler := NewUpdateDpuMachinePowerHandler(fixture.dbSession, fixture.scp)
			rec := fixture.request(t, handler.Handle, http.MethodPatch, target, tt.body(fixture), "dpu-1")

			assert.Equal(t, tt.wantStatus, rec.Code, rec.Body.String())
			assert.Equal(t, tt.wantMethods, fixture.proxiedMethods())
			if tt.wantMessage != "" {
				var response model.APIMessageResponse
				require.NoError(t, json.Unmarshal(rec.Body.Bytes(), &response))
				assert.Equal(t, tt.wantMessage, response.Message)
			}
			if tt.wantCoreMessage != "" {
				var response model.APIMessageResponse
				require.NoError(t, json.Unmarshal(rec.Body.Bytes(), &response))
				assert.Contains(t, response.Message, tt.wantCoreMessage)
			}
			if len(tt.wantMethods) == len(powerMethods) {
				var powerRequest corev1.AdminPowerControlRequest
				require.NoError(t, protojson.Unmarshal(fixture.proxiedReqs[2].RequestJSON, &powerRequest))
				assert.Equal(t, "dpu-1", powerRequest.GetMachineId())
				assert.Equal(t, tt.wantAction, powerRequest.GetAction())
				assert.Empty(t, fixture.proxiedReqs[2].EncryptedSecrets)
			}
		})
	}
}
