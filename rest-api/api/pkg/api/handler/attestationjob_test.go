// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"net/http"
	"testing"
	"time"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"google.golang.org/protobuf/encoding/protojson"
	"google.golang.org/protobuf/proto"
	"google.golang.org/protobuf/types/known/timestamppb"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	swe "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/error"
)

const testHandlerJobID = "4f0c6f0e-7f1d-4a43-9a52-6a1f6f4c0b11"

func TestGetAllAttestationJobHandler_Handle(t *testing.T) {
	f := newAttestationFixture(t)
	handler := NewGetAllAttestationJobHandler(f.dbSession, f.scp)

	cases := []struct {
		name         string
		query        string
		replyMsg     proto.Message
		wantStatus   int
		wantJSON     string
		wantSelector corev1.SpdmListAttestationRequestSelector
		wantSize     uint32
		wantToken    string
	}{
		{name: "empty first page", query: "siteId=" + f.siteID, replyMsg: &corev1.SpdmListAttestationResponse{}, wantStatus: http.StatusOK, wantJSON: `{"jobs":[],"nextPageToken":""}`},
		{name: "continuation with filter", query: "siteId=" + f.siteID + "&status=Passed&pageSize=10&pageToken=abc%2B1", replyMsg: &corev1.SpdmListAttestationResponse{
			Statuses:      []*corev1.SpdmAttestationJobStatus{{JobId: &corev1.UUID{Value: testHandlerJobID}, AttestationStatus: corev1.SpdmAttestationStatus_SPDM_ATT_PASSED, MachineIds: []*corev1.MachineId{{Id: "m1"}}}},
			NextPageToken: "def",
		}, wantStatus: http.StatusOK, wantSelector: corev1.SpdmListAttestationRequestSelector_SPDM_LIST_PASSED, wantSize: 10, wantToken: "abc+1",
			wantJSON: `{"jobs":[{"jobId":"` + testHandlerJobID + `","machineIds":["m1"],"attestationStatus":"Passed"}],"nextPageToken":"def"}`},
		{name: "numbered paging is not supported", query: "siteId=" + f.siteID + "&pageNumber=2", wantStatus: http.StatusBadRequest},
		{name: "zero page size", query: "siteId=" + f.siteID + "&pageSize=0", wantStatus: http.StatusBadRequest},
		{name: "repeated token", query: "siteId=" + f.siteID + "&pageToken=a&pageToken=a", wantStatus: http.StatusBadRequest},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			f.reply(t, tc.replyMsg, nil)
			rec := f.serve(t, handler.Handle, attestationRequest{method: http.MethodGet, target: "/?" + tc.query})
			assert.Equal(t, tc.wantStatus, rec.Code, rec.Body.String())
			if tc.wantStatus != http.StatusOK {
				assert.Empty(t, f.proxied)
				return
			}
			assert.JSONEq(t, tc.wantJSON, rec.Body.String())
			var coreReq corev1.SpdmListAttestationRequest
			require.NoError(t, protojson.Unmarshal(f.proxied[0].RequestJSON, &coreReq))
			assert.Equal(t, tc.wantSelector, coreReq.GetSelector())
			assert.Equal(t, tc.wantSize, coreReq.GetPageSize())
			assert.Equal(t, tc.wantToken, coreReq.GetPageToken())
		})
	}
}

func TestGetAttestationJobHandler_Handle(t *testing.T) {
	f := newAttestationFixture(t)
	handler := NewGetAttestationJobHandler(f.dbSession, f.scp)
	detail := &corev1.SpdmGetAttestationResponse{AttestationsDetails: []*corev1.SpdmAttestationDetails{{
		MachineId: &corev1.MachineId{Id: "m1"},
		DeviceId:  "VERA_CPU_0",
		State:     `PollEvidenceCollection { task_id: "TaskService/Tasks/9", retry_count: 0 }`,
		StartedAt: timestamppb.New(time.Date(2026, 11, 20, 10, 0, 0, 0, time.UTC)),
	}}}

	cases := []struct {
		name       string
		jobID      string
		query      string
		replyMsg   proto.Message
		replyRaw   string
		replyErr   error
		wantStatus int
		wantStarts int
	}{
		{name: "populated job", jobID: testHandlerJobID, query: "siteId=" + f.siteID, replyMsg: detail, wantStatus: http.StatusOK, wantStarts: 1},
		{name: "missing job", jobID: testHandlerJobID, query: "siteId=" + f.siteID, replyErr: coreError(swe.ErrTypeNICoObjectNotFound, "no rows"), wantStatus: http.StatusNotFound, wantStarts: 1},
		{name: "empty successful reply", jobID: testHandlerJobID, query: "siteId=" + f.siteID, replyRaw: `{}`, wantStatus: http.StatusBadGateway, wantStarts: 1},
		{name: "empty device list", jobID: testHandlerJobID, query: "siteId=" + f.siteID, replyRaw: `{"attestationsDetails":[]}`, wantStatus: http.StatusBadGateway, wantStarts: 1},
		{name: "invalid job ID", jobID: "not-a-uuid", query: "siteId=" + f.siteID, wantStatus: http.StatusBadRequest},
		{name: "foreign site", jobID: testHandlerJobID, query: "siteId=" + f.foreignSite, wantStatus: http.StatusForbidden},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			if tc.replyRaw != "" {
				f.replyRaw(tc.replyRaw)
			} else {
				f.reply(t, tc.replyMsg, tc.replyErr)
			}
			rec := f.serve(t, handler.Handle, attestationRequest{method: http.MethodGet, target: "/?" + tc.query, params: map[string]string{"jobId": tc.jobID}})
			assert.Equal(t, tc.wantStatus, rec.Code, rec.Body.String())
			require.Len(t, f.proxied, tc.wantStarts)
			assert.NotContains(t, rec.Body.String(), "TaskService")
			if tc.wantStatus != http.StatusOK {
				return
			}
			var coreReq corev1.UUID
			require.NoError(t, protojson.Unmarshal(f.proxied[0].RequestJSON, &coreReq))
			assert.Equal(t, testHandlerJobID, coreReq.GetValue())
			assert.JSONEq(t, `{"jobId":"`+testHandlerJobID+`","devices":[{"machineId":"m1","deviceId":"VERA_CPU_0","phase":"PollEvidenceCollection",
				"startedAt":"2026-11-20T10:00:00Z","completedAt":null,"cancellationRequestedAt":null,"cancellationTarget":null}]}`, rec.Body.String())
		})
	}
}

func TestGetAllMachineAttestationStatusHandler_Handle(t *testing.T) {
	f := newAttestationFixture(t)
	handler := NewGetAllMachineAttestationStatusHandler(f.dbSession, f.scp)
	waived := corev1.SpdmAttestationStatus_SPDM_ATT_WAIVED

	cases := []struct {
		name       string
		query      string
		replyMsg   proto.Message
		wantStatus int
		wantJSON   string
		wantType   *corev1.MachineType
	}{
		{name: "host filter", query: "siteId=" + f.siteID + "&machineType=Host", replyMsg: &corev1.ListMachineAttestationStatusesResponse{
			Statuses: []*corev1.SpdmMachineAttestationStatus{{MachineId: &corev1.MachineId{Id: "m1"}, AttestationStatus: &waived}},
		}, wantStatus: http.StatusOK, wantType: corev1.MachineType_HOST.Enum(),
			wantJSON: `{"statuses":[{"machineId":"m1","attestationStatus":"Waived","attesterSelectionStatus":null}],"nextPageToken":""}`},
		{name: "power shelves are not attestable", query: "siteId=" + f.siteID + "&machineType=PowerShelf", wantStatus: http.StatusBadRequest},
		{name: "unknown query", query: "siteId=" + f.siteID + "&machineId=m1", wantStatus: http.StatusBadRequest},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			f.reply(t, tc.replyMsg, nil)
			rec := f.serve(t, handler.Handle, attestationRequest{method: http.MethodGet, target: "/?" + tc.query})
			assert.Equal(t, tc.wantStatus, rec.Code, rec.Body.String())
			if tc.wantStatus != http.StatusOK {
				assert.Empty(t, f.proxied)
				return
			}
			assert.JSONEq(t, tc.wantJSON, rec.Body.String())
			var coreReq corev1.ListMachineAttestationStatusesRequest
			require.NoError(t, protojson.Unmarshal(f.proxied[0].RequestJSON, &coreReq))
			require.NotNil(t, coreReq.GetFleet())
			assert.Equal(t, tc.wantType, coreReq.GetFleet().MachineType)
		})
	}
}
