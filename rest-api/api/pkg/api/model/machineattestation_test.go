// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"encoding/json"
	"testing"
	"time"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"google.golang.org/protobuf/types/known/timestamppb"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

const testAttestationMachineID = "fm100htjtiaehv1n5vh67tbmqq4eabcjdmabc2sbrhuqcmo7gjmxgqfbab0"

func TestAPIMachineAttestationTriggerRequest_Validate(t *testing.T) {
	cases := []struct {
		name    string
		req     APIMachineAttestationTriggerRequest
		wantErr string
	}{
		{name: "valid", req: APIMachineAttestationTriggerRequest{MachineID: testAttestationMachineID, RedfishTimeoutSecs: cutil.GetPtr(uint32(10))}},
		{name: "printable synthetic ID", req: APIMachineAttestationTriggerRequest{MachineID: "a b%~", RedfishTimeoutSecs: cutil.GetPtr(uint32(1))}},
		{name: "empty ID", req: APIMachineAttestationTriggerRequest{RedfishTimeoutSecs: cutil.GetPtr(uint32(1))}, wantErr: "machineId"},
		{name: "control character", req: APIMachineAttestationTriggerRequest{MachineID: "a\tb", RedfishTimeoutSecs: cutil.GetPtr(uint32(1))}, wantErr: "machineId"},
		{name: "non-ASCII", req: APIMachineAttestationTriggerRequest{MachineID: "é", RedfishTimeoutSecs: cutil.GetPtr(uint32(1))}, wantErr: "machineId"},
		{name: "invalid UTF-8", req: APIMachineAttestationTriggerRequest{MachineID: "\xff", RedfishTimeoutSecs: cutil.GetPtr(uint32(1))}, wantErr: "machineId"},
		{name: "missing timeout", req: APIMachineAttestationTriggerRequest{MachineID: testAttestationMachineID}, wantErr: "redfishTimeoutSecs"},
		{name: "zero timeout", req: APIMachineAttestationTriggerRequest{MachineID: testAttestationMachineID, RedfishTimeoutSecs: cutil.GetPtr(uint32(0))}, wantErr: "redfishTimeoutSecs"},
		{name: "timeout above cap", req: APIMachineAttestationTriggerRequest{MachineID: testAttestationMachineID, RedfishTimeoutSecs: cutil.GetPtr(uint32(11))}, wantErr: "redfishTimeoutSecs"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			err := tc.req.Validate()
			if tc.wantErr == "" {
				require.NoError(t, err)
				p := tc.req.ToProto()
				assert.Equal(t, tc.req.MachineID, p.GetMachineId().GetId())
				assert.Equal(t, *tc.req.RedfishTimeoutSecs, p.GetRedfishTimeoutSecs())
				assert.Nil(t, p.AttesterIdRegex)
				return
			}
			require.Error(t, err)
			assert.Contains(t, err.Error(), tc.wantErr)
		})
	}
}

func TestAPIMachineAttestationTrigger_FromProto(t *testing.T) {
	started := time.Date(2026, 11, 20, 10, 0, 0, 123000, time.UTC)
	jobID := "4f0c6f0e-7f1d-4a43-9a52-6a1f6f4c0b11"
	scheduled := func() *corev1.SpdmMachineAttestationTriggerResponse {
		return &corev1.SpdmMachineAttestationTriggerResponse{
			MachineId:               &corev1.MachineId{Id: testAttestationMachineID},
			DevicesUnderAttestation: 2,
			ResolvedHardwareClass:   "nvidia_gb200",
			Outcome:                 corev1.SpdmSchedulingOutcome_SPDM_SCHEDULING_OUTCOME_SCHEDULED,
			ProfileVersion:          cutil.GetPtr("V1-T1"),
			StartedAt:               timestamppb.New(started),
			JobId:                   &corev1.UUID{Value: jobID},
			AttesterIds:             []string{"A", "B"},
		}
	}
	noWork := func(outcome corev1.SpdmSchedulingOutcome, version *string) *corev1.SpdmMachineAttestationTriggerResponse {
		return &corev1.SpdmMachineAttestationTriggerResponse{
			MachineId: &corev1.MachineId{Id: testAttestationMachineID}, Outcome: outcome, ProfileVersion: version,
		}
	}
	cases := []struct {
		name     string
		reply    *corev1.SpdmMachineAttestationTriggerResponse
		wantJSON string
		wantErr  string
	}{
		{
			name:  "scheduled",
			reply: scheduled(),
			wantJSON: `{"machineId":"` + testAttestationMachineID + `","outcome":"Scheduled","resolvedHardwareClass":"nvidia_gb200","usedAnyFallback":false,
				"profileVersion":"V1-T1","devicesUnderAttestation":2,"jobId":"` + jobID + `","startedAt":"2026-11-20T10:00:00.000123Z","attesterIds":["A","B"]}`,
		},
		{
			name:  "disabled by a None profile carries its version",
			reply: noWork(corev1.SpdmSchedulingOutcome_SPDM_SCHEDULING_OUTCOME_ATTESTATION_DISABLED, cutil.GetPtr("V3-T1")),
			wantJSON: `{"machineId":"` + testAttestationMachineID + `","outcome":"AttestationDisabled","resolvedHardwareClass":"","usedAnyFallback":false,
				"profileVersion":"V3-T1","devicesUnderAttestation":0,"jobId":null,"startedAt":null,"attesterIds":[]}`,
		},
		{
			name:  "no profile",
			reply: noWork(corev1.SpdmSchedulingOutcome_SPDM_SCHEDULING_OUTCOME_NO_PROFILE, nil),
			wantJSON: `{"machineId":"` + testAttestationMachineID + `","outcome":"NoProfile","resolvedHardwareClass":"","usedAnyFallback":false,
				"profileVersion":null,"devicesUnderAttestation":0,"jobId":null,"startedAt":null,"attesterIds":[]}`,
		},
		{name: "other machine", reply: func() *corev1.SpdmMachineAttestationTriggerResponse {
			r := scheduled()
			r.MachineId.Id = "other"
			return r
		}(), wantErr: "different machine"},
		{name: "unspecified outcome", reply: noWork(0, nil), wantErr: "unsupported outcome"},
		{name: "attester override outcome", reply: noWork(corev1.SpdmSchedulingOutcome_SPDM_SCHEDULING_OUTCOME_ATTESTER_OVERRIDE_SCHEDULED, nil), wantErr: "unsupported outcome"},
		{name: "regex echoed", reply: func() *corev1.SpdmMachineAttestationTriggerResponse {
			r := scheduled()
			r.AttesterIdRegex = cutil.GetPtr("")
			return r
		}(), wantErr: "override"},
		{name: "scheduled without version", reply: func() *corev1.SpdmMachineAttestationTriggerResponse {
			r := scheduled()
			r.ProfileVersion = nil
			return r
		}(), wantErr: "profile version"},
		{name: "no profile with version", reply: noWork(corev1.SpdmSchedulingOutcome_SPDM_SCHEDULING_OUTCOME_NO_PROFILE, cutil.GetPtr("V1")), wantErr: "deciding profile"},
		{name: "no work with a job", reply: func() *corev1.SpdmMachineAttestationTriggerResponse {
			r := noWork(corev1.SpdmSchedulingOutcome_SPDM_SCHEDULING_OUTCOME_NO_ATTESTERS_FOUND, cutil.GetPtr("V1"))
			r.JobId = &corev1.UUID{Value: jobID}
			return r
		}(), wantErr: "scheduled work"},
		{name: "count disagrees with attesters", reply: func() *corev1.SpdmMachineAttestationTriggerResponse {
			r := scheduled()
			r.DevicesUnderAttestation = 3
			return r
		}(), wantErr: "device count"},
		{name: "unsorted attesters", reply: func() *corev1.SpdmMachineAttestationTriggerResponse {
			r := scheduled()
			r.AttesterIds = []string{"B", "A"}
			return r
		}(), wantErr: "sorted"},
		{name: "missing start", reply: func() *corev1.SpdmMachineAttestationTriggerResponse {
			r := scheduled()
			r.StartedAt = nil
			return r
		}(), wantErr: "start time"},
		{name: "nil job UUID", reply: func() *corev1.SpdmMachineAttestationTriggerResponse {
			r := scheduled()
			r.JobId = &corev1.UUID{Value: "00000000-0000-0000-0000-000000000000"}
			return r
		}(), wantErr: "job ID"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			var got APIMachineAttestationTrigger
			err := got.FromProto(testAttestationMachineID, tc.reply)
			if tc.wantErr != "" {
				require.Error(t, err)
				assert.Contains(t, err.Error(), tc.wantErr)
				assert.Equal(t, APIMachineAttestationTrigger{}, got)
				return
			}
			require.NoError(t, err)
			body, err := json.Marshal(got)
			require.NoError(t, err)
			assert.JSONEq(t, tc.wantJSON, string(body))
		})
	}
}

func TestAPIMachineAttestationStatusRequest_Validate(t *testing.T) {
	cases := []struct {
		name    string
		req     APIMachineAttestationStatusRequest
		wantErr string
	}{
		{name: "default timeout", req: APIMachineAttestationStatusRequest{MachineID: testAttestationMachineID}},
		{name: "maximum timeout", req: APIMachineAttestationStatusRequest{MachineID: testAttestationMachineID, RedfishTimeoutSecs: cutil.GetPtr(uint32(35))}},
		{name: "zero timeout", req: APIMachineAttestationStatusRequest{MachineID: testAttestationMachineID, RedfishTimeoutSecs: cutil.GetPtr(uint32(0))}, wantErr: "redfishTimeoutSecs"},
		{name: "timeout above cap", req: APIMachineAttestationStatusRequest{MachineID: testAttestationMachineID, RedfishTimeoutSecs: cutil.GetPtr(uint32(36))}, wantErr: "redfishTimeoutSecs"},
		{name: "empty ID", req: APIMachineAttestationStatusRequest{}, wantErr: "machineId"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			err := tc.req.Validate()
			if tc.wantErr == "" {
				require.NoError(t, err)
				machine := tc.req.ToProto().GetMachine()
				require.NotNil(t, machine)
				assert.Equal(t, tc.req.MachineID, machine.GetMachineId().GetId())
				assert.Equal(t, tc.req.RedfishTimeoutSecs, machine.RedfishTimeoutSecs)
				return
			}
			require.Error(t, err)
			assert.Contains(t, err.Error(), tc.wantErr)
		})
	}
}

func TestAPIMachineAttestationStatus_FromProto(t *testing.T) {
	passed := corev1.SpdmAttestationStatus_SPDM_ATT_PASSED
	selected := corev1.SpdmAttesterSelectionStatus_SPDM_ATTESTER_SELECTION_STATUS_SELECTED
	noProfile := corev1.SpdmAttesterSelectionStatus_SPDM_ATTESTER_SELECTION_STATUS_NO_PROFILE
	unknown := corev1.SpdmAttesterSelectionStatus(42)
	row := func(status *corev1.SpdmAttestationStatus, selection *corev1.SpdmAttesterSelectionStatus) *corev1.SpdmMachineAttestationStatus {
		return &corev1.SpdmMachineAttestationStatus{MachineId: &corev1.MachineId{Id: testAttestationMachineID}, AttestationStatus: status, AttesterSelectionStatus: selection}
	}
	cases := []struct {
		name     string
		reply    *corev1.ListMachineAttestationStatusesResponse
		wantJSON string
		wantErr  string
	}{
		{name: "passed", reply: &corev1.ListMachineAttestationStatusesResponse{Statuses: []*corev1.SpdmMachineAttestationStatus{row(&passed, &selected)}},
			wantJSON: `{"machineId":"` + testAttestationMachineID + `","attestationStatus":"Passed","attesterSelectionStatus":"Selected"}`},
		{name: "empty selection has null status", reply: &corev1.ListMachineAttestationStatusesResponse{Statuses: []*corev1.SpdmMachineAttestationStatus{row(nil, &noProfile)}},
			wantJSON: `{"machineId":"` + testAttestationMachineID + `","attestationStatus":null,"attesterSelectionStatus":"NoProfile"}`},
		{name: "no rows", reply: &corev1.ListMachineAttestationStatusesResponse{}, wantErr: "exactly one"},
		{name: "continuation", reply: &corev1.ListMachineAttestationStatusesResponse{Statuses: []*corev1.SpdmMachineAttestationStatus{row(&passed, &selected)}, NextPageToken: "x"}, wantErr: "exactly one"},
		{name: "missing selection", reply: &corev1.ListMachineAttestationStatusesResponse{Statuses: []*corev1.SpdmMachineAttestationStatus{row(&passed, nil)}}, wantErr: "selection"},
		{name: "unknown selection", reply: &corev1.ListMachineAttestationStatusesResponse{Statuses: []*corev1.SpdmMachineAttestationStatus{row(&passed, &unknown)}}, wantErr: "selection"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			var got APIMachineAttestationStatus
			err := got.FromProto(testAttestationMachineID, tc.reply)
			if tc.wantErr != "" {
				require.Error(t, err)
				assert.Contains(t, err.Error(), tc.wantErr)
				return
			}
			require.NoError(t, err)
			body, err := json.Marshal(got)
			require.NoError(t, err)
			assert.JSONEq(t, tc.wantJSON, string(body))
		})
	}
}

func TestAPIAttestationFleetRequest_ToProto(t *testing.T) {
	failed := AttestationStatusFailed
	passed := AttestationStatusPassed
	dpu := AttestationMachineTypeDpu
	cases := []struct {
		name        string
		req         APIAttestationFleetRequest
		wantErr     string
		wantStatus  *corev1.SpdmAttestationStatus
		wantType    *corev1.MachineType
		wantSize    uint32
		wantToken   string
		statusIsSet bool
	}{
		{name: "defaults", req: APIAttestationFleetRequest{SiteID: testAttestationSiteID}},
		{name: "filters and cursor", req: APIAttestationFleetRequest{SiteID: testAttestationSiteID, Status: &failed, MachineType: &dpu, PageSize: cutil.GetPtr(uint32(5)), PageToken: "tok", OrderBy: AttestationStatusOrderByMachineIDAsc},
			wantStatus: cutil.GetPtr(corev1.SpdmAttestationStatus_SPDM_ATT_FAILED), wantType: cutil.GetPtr(corev1.MachineType_DPU), wantSize: 5, wantToken: "tok"},
		{name: "passed keeps its own number", req: APIAttestationFleetRequest{SiteID: testAttestationSiteID, Status: &passed},
			wantStatus: cutil.GetPtr(corev1.SpdmAttestationStatus_SPDM_ATT_PASSED)},
		{name: "unknown machine type", req: APIAttestationFleetRequest{SiteID: testAttestationSiteID, MachineType: (*AttestationMachineType)(cutil.GetPtr("PowerShelf"))}, wantErr: "machineType"},
		{name: "zero page size", req: APIAttestationFleetRequest{SiteID: testAttestationSiteID, PageSize: cutil.GetPtr(uint32(0))}, wantErr: "pageSize"},
		{name: "page size above cap", req: APIAttestationFleetRequest{SiteID: testAttestationSiteID, PageSize: cutil.GetPtr(uint32(101))}, wantErr: "pageSize"},
		{name: "other order", req: APIAttestationFleetRequest{SiteID: testAttestationSiteID, OrderBy: "MACHINE_ID_DESC"}, wantErr: "orderBy"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			err := tc.req.Validate()
			if tc.wantErr != "" {
				require.Error(t, err)
				assert.Contains(t, err.Error(), tc.wantErr)
				return
			}
			require.NoError(t, err)
			fleet := tc.req.ToProto().GetFleet()
			require.NotNil(t, fleet)
			assert.Equal(t, tc.wantStatus, fleet.AttestationStatus)
			assert.Equal(t, tc.wantType, fleet.MachineType)
			assert.Equal(t, tc.wantSize, fleet.GetPageSize())
			assert.Equal(t, tc.wantToken, fleet.GetPageToken())
		})
	}
}

func TestAPIAttestationFleetPage_FromProto(t *testing.T) {
	inProgress := corev1.SpdmAttestationStatus_SPDM_ATT_IN_PROGRESS
	selected := corev1.SpdmAttesterSelectionStatus_SPDM_ATTESTER_SELECTION_STATUS_SELECTED
	cases := []struct {
		name     string
		reply    *corev1.ListMachineAttestationStatusesResponse
		wantJSON string
		wantErr  string
	}{
		{name: "empty page", reply: &corev1.ListMachineAttestationStatusesResponse{}, wantJSON: `{"statuses":[],"nextPageToken":""}`},
		{name: "present zero is InProgress and absent is null", reply: &corev1.ListMachineAttestationStatusesResponse{
			Statuses: []*corev1.SpdmMachineAttestationStatus{
				{MachineId: &corev1.MachineId{Id: "m1"}, AttestationStatus: &inProgress},
				{MachineId: &corev1.MachineId{Id: "m2"}},
			},
			NextPageToken: "next",
		}, wantJSON: `{"statuses":[
			{"machineId":"m1","attestationStatus":"InProgress","attesterSelectionStatus":null},
			{"machineId":"m2","attestationStatus":null,"attesterSelectionStatus":null}],"nextPageToken":"next"}`},
		{name: "live selection on a fleet row", reply: &corev1.ListMachineAttestationStatusesResponse{
			Statuses: []*corev1.SpdmMachineAttestationStatus{{MachineId: &corev1.MachineId{Id: "m1"}, AttesterSelectionStatus: &selected}},
		}, wantErr: "live selection"},
		{name: "unknown status", reply: &corev1.ListMachineAttestationStatusesResponse{
			Statuses: []*corev1.SpdmMachineAttestationStatus{{MachineId: &corev1.MachineId{Id: "m1"}, AttestationStatus: (*corev1.SpdmAttestationStatus)(cutil.GetPtr(int32(7)))}},
		}, wantErr: "unknown attestation status"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			var got APIAttestationFleetPage
			err := got.FromProto(tc.reply)
			if tc.wantErr != "" {
				require.Error(t, err)
				assert.Contains(t, err.Error(), tc.wantErr)
				return
			}
			require.NoError(t, err)
			body, err := json.Marshal(got)
			require.NoError(t, err)
			assert.JSONEq(t, tc.wantJSON, string(body))
		})
	}
}
