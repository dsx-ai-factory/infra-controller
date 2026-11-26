// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"encoding/json"
	"net/http"
	"testing"
	"time"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"
	"google.golang.org/protobuf/encoding/protojson"
	"google.golang.org/protobuf/types/known/timestamppb"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	authz "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

func TestGetAllDpuMachineHealthReportHandler_Handle(t *testing.T) {
	const (
		validateMessage = "Error validating DPU Machine health report retrieval request data"
		notFoundMessage = "Could not find DPU Machine with specified ID"
	)
	resolveMethods := []string{corev1.Forge_FindMachineIds_FullMethodName, corev1.Forge_FindMachinesByIds_FullMethodName}
	reportMethods := append(append([]string{}, resolveMethods...), corev1.Forge_ListMachineHealthReports_FullMethodName)
	observedAt := time.Date(2026, 6, 24, 12, 0, 0, 0, time.UTC)
	dpuMachine := &corev1.Machine{Id: &corev1.MachineId{Id: "dpu-1"}, MachineType: corev1.MachineType_DPU}

	tests := []struct {
		name string
		// machine overrides the Core Machine returned for the DPU ID.
		machine *corev1.Machine
		// searchMachineIDs overrides the DPU IDs Core lists; nil lists "dpu-1".
		searchMachineIDs []string
		setup            func(*testing.T, *dpuMachineCoreFixture)
		reports          *corev1.ListHealthReportResponse
		reportError      error
		target           func(*dpuMachineCoreFixture) string
		wantStatus       int
		wantMessage      string
		// wantCoreMessage is the Core status message expected within the proxy's rendering of it.
		wantCoreMessage string
		wantMethods     []string
		wantBody        string
	}{
		{
			name: "returns the per source health reports of the DPU",
			reports: &corev1.ListHealthReportResponse{HealthReportEntries: []*corev1.HealthReportEntry{
				{
					Mode: corev1.HealthReportApplyMode_Replace,
					Report: &corev1.HealthReport{
						Source:      "dpu-agent",
						TriggeredBy: nil,
						ObservedAt:  timestamppb.New(observedAt),
						Successes:   []*corev1.HealthProbeSuccess{{Id: "DpuDiskUtilizationCheck"}},
						Alerts: []*corev1.HealthProbeAlert{{
							Id:              "BgpPeeringTor",
							Message:         "BGP session with the top-of-rack switch is down",
							Classifications: []string{"PreventAllocations"},
						}},
					},
				},
				{
					Mode:   corev1.HealthReportApplyMode_Merge,
					Report: &corev1.HealthReport{Source: "maintenance.dpu-reprovision"},
				},
			}},
			wantStatus:  http.StatusOK,
			wantMethods: reportMethods,
		},
		{
			name:        "returns an empty list for a DPU without health reports",
			reports:     &corev1.ListHealthReportResponse{},
			wantStatus:  http.StatusOK,
			wantMethods: reportMethods,
			wantBody:    `[]`,
		},
		{
			name:            "surfaces a Core rejection of the health report listing",
			reportError:     testMachineCoreFailure(status.Error(codes.FailedPrecondition, "health reports are not available")),
			wantStatus:      http.StatusPreconditionFailed,
			wantCoreMessage: "health reports are not available",
			wantMethods:     reportMethods,
		},
		{
			name:             "returns not found for an ID Core does not list as a DPU",
			searchMachineIDs: []string{"dpu-2"},
			wantStatus:       http.StatusNotFound,
			wantMessage:      notFoundMessage,
			wantMethods:      resolveMethods[:1],
		},
		{
			name:        "returns not found for a host Machine ID",
			machine:     &corev1.Machine{Id: &corev1.MachineId{Id: "dpu-1"}, MachineType: corev1.MachineType_HOST},
			wantStatus:  http.StatusNotFound,
			wantMessage: notFoundMessage,
			wantMethods: resolveMethods,
		},
		{
			name:        "rejects a request without a Site ID",
			target:      func(*dpuMachineCoreFixture) string { return "/" },
			wantStatus:  http.StatusBadRequest,
			wantMessage: validateMessage,
		},
		{
			name:        "rejects a malformed Site ID",
			target:      func(*dpuMachineCoreFixture) string { return "/?siteId=not-a-uuid" },
			wantStatus:  http.StatusBadRequest,
			wantMessage: validateMessage,
		},
		{
			name:        "rejects unknown query parameters",
			target:      func(f *dpuMachineCoreFixture) string { return "/?siteId=" + f.siteID + "&unknown=true" },
			wantStatus:  http.StatusBadRequest,
			wantMessage: "unknown query parameter specified in request: unknown",
		},
		{
			name:        "rejects the Tenant Admin role",
			setup:       func(_ *testing.T, f *dpuMachineCoreFixture) { f.setUserRoles(authz.TenantAdminRole) },
			wantStatus:  http.StatusForbidden,
			wantMessage: "User does not have Provider Admin role with org",
		},
		{
			name:        "rejects the Provider Viewer role",
			setup:       func(_ *testing.T, f *dpuMachineCoreFixture) { f.setUserRoles(authz.ProviderViewerRole) },
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
					machine = dpuMachine
				}
				fixture.expectProxyResponse(t, &corev1.MachineList{Machines: []*corev1.Machine{machine}})
			}
			if len(tt.wantMethods) >= 3 {
				if tt.reportError != nil {
					fixture.expectProxyError(tt.reportError)
				} else {
					fixture.expectProxyResponse(t, tt.reports)
				}
			}

			target := "/?siteId=" + fixture.siteID
			if tt.target != nil {
				target = tt.target(fixture)
			}
			handler := NewGetAllDpuMachineHealthReportHandler(fixture.dbSession, fixture.scp)
			rec := fixture.request(t, handler.Handle, http.MethodGet, target, "", "dpu-1")

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
			if tt.wantBody != "" {
				assert.JSONEq(t, tt.wantBody, rec.Body.String())
			}
			if len(tt.wantMethods) == len(reportMethods) {
				var machineID corev1.MachineId
				require.NoError(t, protojson.Unmarshal(fixture.proxiedReqs[2].RequestJSON, &machineID))
				assert.Equal(t, "dpu-1", machineID.GetId())
			}
			if tt.wantStatus != http.StatusOK || tt.wantBody != "" {
				return
			}

			var response []model.APIMachineHealthReportEntry
			require.NoError(t, json.Unmarshal(rec.Body.Bytes(), &response))
			require.Len(t, response, 2)
			assert.Equal(t, "dpu-agent", response[0].Source)
			assert.Equal(t, model.MachineHealthReportModeReplace, response[0].Mode)
			require.NotNil(t, response[0].ObservedAt)
			assert.Equal(t, observedAt.Format(time.RFC3339), *response[0].ObservedAt)
			require.Len(t, response[0].Successes, 1)
			assert.Equal(t, "DpuDiskUtilizationCheck", response[0].Successes[0].ID)
			require.Len(t, response[0].Alerts, 1)
			assert.Equal(t, "BgpPeeringTor", response[0].Alerts[0].ID)
			assert.Equal(t, []string{"PreventAllocations"}, response[0].Alerts[0].Classifications)
			assert.Equal(t, "maintenance.dpu-reprovision", response[1].Source)
			assert.Equal(t, model.MachineHealthReportModeMerge, response[1].Mode)
		})
	}
}
