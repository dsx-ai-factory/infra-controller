// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"net/http"
	"testing"

	"github.com/labstack/echo/v4"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"google.golang.org/protobuf/encoding/protojson"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	swe "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/error"
)

func TestAttestationJobAction_Handle(t *testing.T) {
	f := newAttestationFixture(t)
	handlers := []struct {
		name   string
		handle echo.HandlerFunc
		method string
	}{
		{name: "waive", handle: NewWaiveAttestationJobHandler(f.dbSession, f.scp).Handle, method: corev1.Forge_WaiveAttestation_FullMethodName},
		{name: "fail", handle: NewFailAttestationJobHandler(f.dbSession, f.scp).Handle, method: corev1.Forge_FailAttestation_FullMethodName},
	}
	body := `{"siteId":"` + f.siteID + `"}`
	cases := []struct {
		name        string
		jobID       string
		body        string
		replyErr    error
		wantStatus  int
		wantMessage string
		wantStarts  int
	}{
		{name: "accepted", jobID: testHandlerJobID, body: body, wantStatus: http.StatusAccepted, wantStarts: 1},
		{name: "missing job", jobID: testHandlerJobID, body: body, replyErr: coreError(swe.ErrTypeNICoObjectNotFound, "no rows"), wantStatus: http.StatusNotFound, wantStarts: 1},
		{name: "lost reply may have committed", jobID: testHandlerJobID, body: body, replyErr: coreError(swe.ErrTypeNICoUnavailable, "reset"), wantStatus: http.StatusServiceUnavailable, wantMessage: "cancellationTarget", wantStarts: 1},
		{name: "job ID in body is not a member", jobID: testHandlerJobID, body: `{"siteId":"` + f.siteID + `","jobId":"` + testHandlerJobID + `"}`, wantStatus: http.StatusBadRequest},
		{name: "invalid job ID", jobID: "1", body: body, wantStatus: http.StatusBadRequest},
		{name: "foreign site", jobID: testHandlerJobID, body: `{"siteId":"` + f.foreignSite + `"}`, wantStatus: http.StatusForbidden},
	}
	for _, h := range handlers {
		for _, tc := range cases {
			t.Run(h.name+"/"+tc.name, func(t *testing.T) {
				f.reply(t, nil, tc.replyErr)
				if tc.replyErr == nil {
					f.replyRaw(`{}`)
				}
				rec := f.serve(t, h.handle, attestationRequest{method: http.MethodPost, target: "/", contentType: "application/json", body: tc.body, params: map[string]string{"jobId": tc.jobID}})
				assert.Equal(t, tc.wantStatus, rec.Code, rec.Body.String())
				require.Len(t, f.proxied, tc.wantStarts)
				if tc.wantMessage != "" {
					assert.Contains(t, decodeAPIError(t, rec).Message, tc.wantMessage)
				}
				if tc.wantStarts == 0 {
					return
				}
				assert.Equal(t, h.method, f.proxied[0].FullMethod)
				var coreReq corev1.UUID
				require.NoError(t, protojson.Unmarshal(f.proxied[0].RequestJSON, &coreReq))
				assert.Equal(t, testHandlerJobID, coreReq.GetValue())
			})
		}
	}
}
