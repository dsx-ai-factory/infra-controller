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

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

const testAttestationJobID = "4f0c6f0e-7f1d-4a43-9a52-6a1f6f4c0b11"

func TestAPIAttestationJobListRequest_ToProto(t *testing.T) {
	status := func(s AttestationStatus) *AttestationStatus { return &s }
	cases := []struct {
		name         string
		req          APIAttestationJobListRequest
		wantErr      string
		wantSelector corev1.SpdmListAttestationRequestSelector
	}{
		{name: "default is in progress", req: APIAttestationJobListRequest{SiteID: testAttestationSiteID}, wantSelector: corev1.SpdmListAttestationRequestSelector_SPDM_LIST_IN_PROGRESS},
		{name: "waived", req: APIAttestationJobListRequest{SiteID: testAttestationSiteID, Status: status(AttestationStatusWaived)}, wantSelector: corev1.SpdmListAttestationRequestSelector_SPDM_LIST_WAIVED},
		{name: "failed", req: APIAttestationJobListRequest{SiteID: testAttestationSiteID, Status: status(AttestationStatusFailed)}, wantSelector: corev1.SpdmListAttestationRequestSelector_SPDM_LIST_FAILED},
		{name: "passed selector differs from passed verdict number", req: APIAttestationJobListRequest{SiteID: testAttestationSiteID, Status: status(AttestationStatusPassed)}, wantSelector: corev1.SpdmListAttestationRequestSelector_SPDM_LIST_PASSED},
		{name: "unknown status", req: APIAttestationJobListRequest{SiteID: testAttestationSiteID, Status: status("Cancelled")}, wantErr: "status"},
		{name: "invalid site", req: APIAttestationJobListRequest{SiteID: "x"}, wantErr: "siteId"},
		{name: "other order", req: APIAttestationJobListRequest{SiteID: testAttestationSiteID, OrderBy: "JOB_ID_DESC"}, wantErr: "orderBy"},
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
			assert.Equal(t, tc.wantSelector, tc.req.ToProto().GetSelector())
		})
	}
}

func TestAPIAttestationJobList_FromProto(t *testing.T) {
	cases := []struct {
		name     string
		reply    *corev1.SpdmListAttestationResponse
		wantJSON string
		wantErr  string
	}{
		{name: "empty page", reply: &corev1.SpdmListAttestationResponse{}, wantJSON: `{"jobs":[],"nextPageToken":""}`},
		{name: "omitted status is in progress", reply: &corev1.SpdmListAttestationResponse{
			Statuses:      []*corev1.SpdmAttestationJobStatus{{JobId: &corev1.UUID{Value: testAttestationJobID}, MachineIds: []*corev1.MachineId{{Id: "m1"}}}},
			NextPageToken: "next",
		}, wantJSON: `{"jobs":[{"jobId":"` + testAttestationJobID + `","machineIds":["m1"],"attestationStatus":"InProgress"}],"nextPageToken":"next"}`},
		{name: "missing job ID", reply: &corev1.SpdmListAttestationResponse{Statuses: []*corev1.SpdmAttestationJobStatus{{}}}, wantErr: "job ID"},
		{name: "unknown status", reply: &corev1.SpdmListAttestationResponse{Statuses: []*corev1.SpdmAttestationJobStatus{{JobId: &corev1.UUID{Value: testAttestationJobID}, AttestationStatus: 9}}}, wantErr: "unknown attestation status"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			var got APIAttestationJobList
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

func TestAttestationDevicePhase_FromCoreState(t *testing.T) {
	cases := []struct {
		state string
		want  AttestationDevicePhase
	}{
		{state: "FetchMetadata", want: AttestationDevicePhaseFetchMetadata},
		{state: `PollEvidenceCollection { task_id: "secret-task", retry_count: 1 }`, want: AttestationDevicePhasePollEvidenceCollection},
		{state: `Failed("verifier said no")`, want: AttestationDevicePhaseFailed},
		{state: "Waived", want: AttestationDevicePhaseWaived},
		{state: "Cancelled", want: AttestationDevicePhaseUnknown},
		{state: "", want: AttestationDevicePhaseUnknown},
		{state: "failed", want: AttestationDevicePhaseUnknown},
	}
	for _, tc := range cases {
		t.Run(tc.state, func(t *testing.T) {
			var got AttestationDevicePhase
			got.FromCoreState(tc.state)
			assert.Equal(t, tc.want, got)
		})
	}
}

func TestAPIAttestationJob_FromProto(t *testing.T) {
	started := time.Date(2026, 11, 20, 10, 0, 0, 0, time.UTC)
	waive := corev1.SpdmCancellationTarget_SPDM_CANCELLATION_TARGET_WAIVE
	unknownTarget := corev1.SpdmCancellationTarget(7)
	device := func() *corev1.SpdmAttestationDetails {
		return &corev1.SpdmAttestationDetails{
			MachineId: &corev1.MachineId{Id: "m1"},
			DeviceId:  "VERA_CPU_0",
			State:     `Failed("evidence mismatch at 0xdead")`,
			StartedAt: timestamppb.New(started),
		}
	}
	cases := []struct {
		name     string
		reply    *corev1.SpdmGetAttestationResponse
		wantJSON string
		wantErr  string
	}{
		{name: "device without failure text", reply: &corev1.SpdmGetAttestationResponse{AttestationsDetails: []*corev1.SpdmAttestationDetails{device()}},
			wantJSON: `{"jobId":"` + testAttestationJobID + `","devices":[{"machineId":"m1","deviceId":"VERA_CPU_0","phase":"Failed","startedAt":"2026-11-20T10:00:00Z",
				"completedAt":null,"cancellationRequestedAt":null,"cancellationTarget":null}]}`},
		{name: "cancellation request", reply: &corev1.SpdmGetAttestationResponse{AttestationsDetails: []*corev1.SpdmAttestationDetails{func() *corev1.SpdmAttestationDetails {
			d := device()
			d.State = "Waived"
			d.CompletedAt = timestamppb.New(started.Add(time.Minute))
			d.CancelledAt = timestamppb.New(started.Add(30 * time.Second))
			d.CancellationTarget = &waive
			return d
		}()}},
			wantJSON: `{"jobId":"` + testAttestationJobID + `","devices":[{"machineId":"m1","deviceId":"VERA_CPU_0","phase":"Waived","startedAt":"2026-11-20T10:00:00Z",
				"completedAt":"2026-11-20T10:01:00Z","cancellationRequestedAt":"2026-11-20T10:00:30Z","cancellationTarget":"Waive"}]}`},
		{name: "empty reply", reply: &corev1.SpdmGetAttestationResponse{}, wantErr: "no devices"},
		{name: "nil reply", reply: nil, wantErr: "no devices"},
		{name: "missing device ID", reply: &corev1.SpdmGetAttestationResponse{AttestationsDetails: []*corev1.SpdmAttestationDetails{func() *corev1.SpdmAttestationDetails {
			d := device()
			d.DeviceId = ""
			return d
		}()}}, wantErr: "device ID"},
		{name: "missing start", reply: &corev1.SpdmGetAttestationResponse{AttestationsDetails: []*corev1.SpdmAttestationDetails{func() *corev1.SpdmAttestationDetails {
			d := device()
			d.StartedAt = nil
			return d
		}()}}, wantErr: "start time"},
		{name: "unknown target", reply: &corev1.SpdmGetAttestationResponse{AttestationsDetails: []*corev1.SpdmAttestationDetails{func() *corev1.SpdmAttestationDetails {
			d := device()
			d.CancellationTarget = &unknownTarget
			return d
		}()}}, wantErr: "cancellation target"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			var got APIAttestationJob
			err := got.FromProto(testAttestationJobID, tc.reply)
			if tc.wantErr != "" {
				require.Error(t, err)
				assert.Contains(t, err.Error(), tc.wantErr)
				assert.Equal(t, APIAttestationJob{}, got)
				return
			}
			require.NoError(t, err)
			body, err := json.Marshal(got)
			require.NoError(t, err)
			assert.JSONEq(t, tc.wantJSON, string(body))
			assert.NotContains(t, string(body), "0xdead")
		})
	}
}
