// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"

	"github.com/labstack/echo/v4"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"google.golang.org/protobuf/encoding/protojson"
	"google.golang.org/protobuf/proto"
	"google.golang.org/protobuf/types/known/timestamppb"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	swe "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/error"
)

// buildAttestationMachine creates a machine owned by the fixture's provider.
func (f *attestationFixture) buildAttestationMachine(t *testing.T, update cdbm.MachineUpdateInput) string {
	t.Helper()
	m := common.TestBuildMachine(t, f.dbSession, f.provider, f.site, nil, nil, cdbm.MachineStatusReady)
	update.MachineID = m.ID
	_, err := cdbm.NewMachineDAO(f.dbSession).Update(context.Background(), nil, update)
	require.NoError(t, err)
	return m.ID
}

// serveRouted serves the request through a real Echo router so path
// parameters are captured as in production.
func (f *attestationFixture) serveRouted(t *testing.T, method, route, target, contentType, body string, handle echo.HandlerFunc) *httptest.ResponseRecorder {
	t.Helper()
	e := echo.New()
	e.Add(method, "/org/:orgName"+route, func(c echo.Context) error {
		c.Set("user", f.user)
		return handle(c)
	})
	var req *http.Request
	if body == "" {
		req = httptest.NewRequest(method, "/org/"+f.org+target, nil)
	} else {
		req = httptest.NewRequest(method, "/org/"+f.org+target, strings.NewReader(body))
	}
	if contentType != "" {
		req.Header.Set(echo.HeaderContentType, contentType)
	}
	rec := httptest.NewRecorder()
	e.ServeHTTP(rec, req)
	return rec
}

func TestTriggerMachineAttestationHandler_Handle(t *testing.T) {
	f := newAttestationFixture(t)
	handler := NewTriggerMachineAttestationHandler(f.dbSession, f.scp)
	ready := f.buildAttestationMachine(t, cdbm.MachineUpdateInput{})
	assigned := f.buildAttestationMachine(t, cdbm.MachineUpdateInput{IsAssigned: cutil.GetPtr(true)})
	missing := f.buildAttestationMachine(t, cdbm.MachineUpdateInput{IsMissingOnSite: cutil.GetPtr(true)})
	jobID := "4f0c6f0e-7f1d-4a43-9a52-6a1f6f4c0b11"
	receipt := func(machineID string) *corev1.SpdmMachineAttestationTriggerResponse {
		return &corev1.SpdmMachineAttestationTriggerResponse{
			MachineId:               &corev1.MachineId{Id: machineID},
			DevicesUnderAttestation: 1,
			Outcome:                 corev1.SpdmSchedulingOutcome_SPDM_SCHEDULING_OUTCOME_SCHEDULED,
			ProfileVersion:          cutil.GetPtr("V1"),
			StartedAt:               timestamppb.New(time.Date(2026, 11, 20, 10, 0, 0, 0, time.UTC)),
			JobId:                   &corev1.UUID{Value: jobID},
			AttesterIds:             []string{"VERA_CPU_0"},
		}
	}
	const body = `{"redfishTimeoutSecs":5}`

	cases := []struct {
		name        string
		target      string
		body        string
		replyMsg    proto.Message
		replyErr    error
		wantStatus  int
		wantMessage string
		wantStarts  int
	}{
		{name: "schedules", target: "/machine/" + ready + "/attestation", body: body, replyMsg: receipt(ready), wantStatus: http.StatusOK, wantStarts: 1},
		{name: "assigned machine", target: "/machine/" + assigned + "/attestation", body: body, wantStatus: http.StatusBadRequest, wantMessage: "assigned"},
		{name: "missing on site", target: "/machine/" + missing + "/attestation", body: body, wantStatus: http.StatusBadRequest},
		{name: "unknown machine", target: "/machine/no-such-machine/attestation", body: body, wantStatus: http.StatusNotFound},
		{name: "foreign machine is not disclosed", target: "/machine/" + f.foreignMachs[0] + "/attestation", body: body, wantStatus: http.StatusNotFound},
		{name: "control character in path", target: "/machine/a%09b/attestation", body: body, wantStatus: http.StatusBadRequest, wantMessage: "Error validating"},
		{name: "non-ASCII in path", target: "/machine/%C3%A9/attestation", body: body, wantStatus: http.StatusBadRequest},
		{name: "timeout above cap", target: "/machine/" + ready + "/attestation", body: `{"redfishTimeoutSecs":11}`, wantStatus: http.StatusBadRequest},
		{name: "attester override is not accepted", target: "/machine/" + ready + "/attestation", body: `{"redfishTimeoutSecs":5,"attesterIdRegex":null}`, wantStatus: http.StatusBadRequest, wantMessage: "unknown member"},
		{name: "disabled or lost reply", target: "/machine/" + ready + "/attestation", body: body, replyErr: coreError(swe.ErrTypeNICoUnavailable, "spdm disabled"), wantStatus: http.StatusServiceUnavailable, wantMessage: "SPDM may be disabled", wantStarts: 1},
		{name: "receipt for another machine", target: "/machine/" + ready + "/attestation", body: body, replyMsg: receipt("other"), wantStatus: http.StatusBadGateway, wantMessage: "List the Site's attestation jobs", wantStarts: 1},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			f.reply(t, tc.replyMsg, tc.replyErr)
			rec := f.serveRouted(t, http.MethodPost, "/machine/:id/attestation", tc.target, "application/json", tc.body, handler.Handle)
			assert.Equal(t, tc.wantStatus, rec.Code, rec.Body.String())
			require.Len(t, f.proxied, tc.wantStarts)
			if tc.wantMessage != "" {
				assert.Contains(t, decodeAPIError(t, rec).Message, tc.wantMessage)
			}
			if tc.wantStatus != http.StatusOK {
				return
			}
			var coreReq corev1.SpdmMachineAttestationTriggerRequest
			require.NoError(t, protojson.Unmarshal(f.proxied[0].RequestJSON, &coreReq))
			assert.Equal(t, ready, coreReq.GetMachineId().GetId())
			assert.Equal(t, uint32(5), coreReq.GetRedfishTimeoutSecs())
			assert.Nil(t, coreReq.AttesterIdRegex)
			var resp model.APIMachineAttestationTrigger
			require.NoError(t, json.Unmarshal(rec.Body.Bytes(), &resp))
			assert.Equal(t, jobID, *resp.JobID)
		})
	}
}

func TestGetMachineAttestationStatusHandler_Handle(t *testing.T) {
	f := newAttestationFixture(t)
	handler := NewGetMachineAttestationStatusHandler(f.dbSession, f.scp)
	assigned := f.buildAttestationMachine(t, cdbm.MachineUpdateInput{IsAssigned: cutil.GetPtr(true)})
	selected := corev1.SpdmAttesterSelectionStatus_SPDM_ATTESTER_SELECTION_STATUS_SELECTED
	failed := corev1.SpdmAttestationStatus_SPDM_ATT_FAILED
	status := &corev1.ListMachineAttestationStatusesResponse{Statuses: []*corev1.SpdmMachineAttestationStatus{
		{MachineId: &corev1.MachineId{Id: assigned}, AttestationStatus: &failed, AttesterSelectionStatus: &selected},
	}}

	cases := []struct {
		name        string
		target      string
		replyMsg    proto.Message
		replyErr    error
		wantStatus  int
		wantTimeout *uint32
		wantStarts  int
	}{
		{name: "assigned machines can be read", target: "/machine/" + assigned + "/attestation/status", replyMsg: status, wantStatus: http.StatusOK, wantStarts: 1},
		{name: "explicit timeout", target: "/machine/" + assigned + "/attestation/status?redfishTimeoutSecs=35", replyMsg: status, wantStatus: http.StatusOK, wantTimeout: cutil.GetPtr(uint32(35)), wantStarts: 1},
		{name: "timeout above cap", target: "/machine/" + assigned + "/attestation/status?redfishTimeoutSecs=36", wantStatus: http.StatusBadRequest},
		{name: "non-numeric timeout", target: "/machine/" + assigned + "/attestation/status?redfishTimeoutSecs=x", wantStatus: http.StatusBadRequest},
		{name: "deadline exceeded", target: "/machine/" + assigned + "/attestation/status", replyErr: coreError(swe.ErrTypeNICoDeadlineExceeded, "slow BMC"), wantStatus: http.StatusGatewayTimeout, wantStarts: 1},
		{name: "fleet-shaped reply", target: "/machine/" + assigned + "/attestation/status", replyMsg: &corev1.ListMachineAttestationStatusesResponse{}, wantStatus: http.StatusBadGateway, wantStarts: 1},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			f.reply(t, tc.replyMsg, tc.replyErr)
			rec := f.serveRouted(t, http.MethodGet, "/machine/:id/attestation/status", tc.target, "", "", handler.Handle)
			assert.Equal(t, tc.wantStatus, rec.Code, rec.Body.String())
			require.Len(t, f.proxied, tc.wantStarts)
			if tc.wantStatus != http.StatusOK {
				return
			}
			var coreReq corev1.ListMachineAttestationStatusesRequest
			require.NoError(t, protojson.Unmarshal(f.proxied[0].RequestJSON, &coreReq))
			assert.Equal(t, assigned, coreReq.GetMachine().GetMachineId().GetId())
			assert.Equal(t, tc.wantTimeout, coreReq.GetMachine().RedfishTimeoutSecs)
			assert.JSONEq(t, `{"machineId":"`+assigned+`","attestationStatus":"Failed","attesterSelectionStatus":"Selected"}`, rec.Body.String())
		})
	}
}
