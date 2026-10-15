// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"encoding/json"
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"google.golang.org/protobuf/types/known/timestamppb"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// The deprecated observed_at alias is returned only until its date gate passes.
// The gate is moved around the current time so both outcomes are exercised on
// both conversion paths regardless of the date the test runs.
func TestAPIMachineHealthObservedAtDeprecationGate(t *testing.T) {
	saved := machineHealthObservedAtDeprecationTime
	t.Cleanup(func() { machineHealthObservedAtDeprecationTime = saved })

	observed := time.Date(2026, time.September, 28, 12, 0, 0, 0, time.UTC)
	observedAt := observed.Format(time.RFC3339)
	fromDB := func() APIMachineHealth {
		var health APIMachineHealth
		health.FromDBModel(&cdbm.MachineHealth{ObservedAt: &observedAt})
		return health
	}
	fromProto := func() APIMachineHealth {
		var health APIMachineHealth
		health.FromProto(&corev1.HealthReport{ObservedAt: timestamppb.New(observed)})
		return health
	}

	tests := []struct {
		name      string
		gate      time.Time
		wantAlias bool
	}{
		{name: "before cutoff keeps observed_at", gate: time.Now().Add(24 * time.Hour), wantAlias: true},
		{name: "after cutoff drops observed_at", gate: time.Now().Add(-24 * time.Hour), wantAlias: false},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			machineHealthObservedAtDeprecationTime = tt.gate
			for name, build := range map[string]func() APIMachineHealth{"FromDBModel": fromDB, "FromProto": fromProto} {
				encoded, err := json.Marshal(build())
				require.NoError(t, err, name)
				var fields map[string]any
				require.NoError(t, json.Unmarshal(encoded, &fields), name)
				assert.Equal(t, observedAt, fields["observedAt"], name)
				_, hasAlias := fields["observed_at"]
				assert.Equal(t, tt.wantAlias, hasAlias, name)
				if tt.wantAlias {
					assert.Equal(t, observedAt, fields["observed_at"], name)
				}
			}
		})
	}
}

func TestAPIMachineHealthReportEntryRequestValidateAndToProto(t *testing.T) {
	inAlertSince := "2026-06-24T11:00:00Z"
	req := APIMachineHealthReportEntryRequest{
		Source: "overrides.sre",
		Mode:   MachineHealthReportModeReplace,
		Successes: []APIHealthProbeSuccess{
			{ID: "probe.ok", Target: cutil.GetPtr("host")},
		},
		Alerts: []APIHealthProbeAlert{
			{
				ID:              "probe.alert",
				Target:          cutil.GetPtr("gpu0"),
				InAlertSince:    &inAlertSince,
				Message:         "forced unhealthy",
				TenantMessage:   cutil.GetPtr("maintenance"),
				Classifications: []string{"maintenance"},
			},
		},
	}
	require.NoError(t, req.Validate())

	user := &cdbm.User{ID: uuid.New()}
	protoReq := req.ToProto("machine-1", user)
	assert.Equal(t, "machine-1", protoReq.GetMachineId().GetId())
	entry := protoReq.GetHealthReportEntry()
	require.NotNil(t, entry)
	assert.Equal(t, corev1.HealthReportApplyMode_Replace, entry.GetMode())
	report := entry.GetReport()
	require.NotNil(t, report)
	assert.Equal(t, "overrides.sre", report.GetSource())
	assert.Equal(t, user.ID.String(), report.GetTriggeredBy())
	assert.WithinDuration(t, time.Now(), report.GetObservedAt().AsTime(), time.Minute)
	require.Len(t, report.GetSuccesses(), 1)
	assert.Equal(t, "probe.ok", report.GetSuccesses()[0].GetId())
	require.Len(t, report.GetAlerts(), 1)
	assert.Equal(t, "probe.alert", report.GetAlerts()[0].GetId())
	assert.Equal(t, inAlertSince, report.GetAlerts()[0].GetInAlertSince().AsTime().Format(time.RFC3339))

	assert.Error(t, (&APIMachineHealthReportEntryRequest{Mode: MachineHealthReportModeMerge}).Validate())
	assert.Error(t, (&APIMachineHealthReportEntryRequest{Source: "source", Mode: MachineHealthReportMode("merge")}).Validate())
	assert.Error(t, (&APIMachineHealthReportEntryRequest{Source: "source", Mode: MachineHealthReportModeMerge, Successes: []APIHealthProbeSuccess{{}}}).Validate())
	assert.Error(t, (&APIMachineHealthReportEntryRequest{Source: "source", Mode: MachineHealthReportModeMerge, Alerts: []APIHealthProbeAlert{{ID: "alert"}}}).Validate())
}

func TestAPIRackHealthReportEntryRequest_Validate(t *testing.T) {
	validReport := APIMachineHealthReportEntryRequest{Source: "overrides.sre", Mode: MachineHealthReportModeMerge}
	tests := []struct {
		name    string
		request APIRackHealthReportEntryRequest
		wantErr bool
	}{
		{name: "valid", request: APIRackHealthReportEntryRequest{SiteID: uuid.NewString(), APIMachineHealthReportEntryRequest: validReport}},
		{name: "invalid site ID", request: APIRackHealthReportEntryRequest{SiteID: "not-a-uuid", APIMachineHealthReportEntryRequest: validReport}, wantErr: true},
		{name: "missing report source", request: APIRackHealthReportEntryRequest{SiteID: uuid.NewString(), APIMachineHealthReportEntryRequest: APIMachineHealthReportEntryRequest{Mode: MachineHealthReportModeMerge}}, wantErr: true},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			if test.wantErr {
				assert.Error(t, test.request.Validate())
				return
			}
			assert.NoError(t, test.request.Validate())
		})
	}
}

func TestAPITrayHealthReportEntryRequest_Validate(t *testing.T) {
	validReport := APIMachineHealthReportEntryRequest{Source: "overrides.sre", Mode: MachineHealthReportModeMerge}
	tests := []struct {
		name    string
		request APITrayHealthReportEntryRequest
		wantErr bool
	}{
		{name: "valid", request: APITrayHealthReportEntryRequest{SiteID: uuid.NewString(), Type: "PowerShelf", APIMachineHealthReportEntryRequest: validReport}},
		{name: "missing type", request: APITrayHealthReportEntryRequest{SiteID: uuid.NewString(), APIMachineHealthReportEntryRequest: validReport}, wantErr: true},
		{name: "unsupported type", request: APITrayHealthReportEntryRequest{SiteID: uuid.NewString(), Type: "CDU", APIMachineHealthReportEntryRequest: validReport}, wantErr: true},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			if test.wantErr {
				assert.Error(t, test.request.Validate())
				return
			}
			assert.NoError(t, test.request.Validate())
		})
	}
}
