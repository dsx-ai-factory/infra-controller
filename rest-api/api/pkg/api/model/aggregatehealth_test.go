// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"testing"
	"time"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"google.golang.org/protobuf/types/known/timestamppb"

	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	flowv1 "github.com/NVIDIA/infra-controller/rest-api/proto/flow/gen/v1"
)

func TestAPIAggregateHealth_FromFlowProto(t *testing.T) {
	observedAt := time.Date(2026, time.September, 28, 12, 0, 0, 0, time.UTC)
	inAlertSince := observedAt.Add(-time.Minute)
	target := "PSU1"
	tenantMessage := "replace the power supply"
	var got APIAggregateHealth

	got.FromFlowProto(&flowv1.HealthReport{
		Source:     "rack-aggregate-health",
		ObservedAt: timestamppb.New(observedAt),
		Successes:  []*flowv1.HealthProbeSuccess{{Id: "FanSpeed"}},
		Alerts: []*flowv1.HealthProbeAlert{{
			Id:              "PowerSupply",
			Target:          &target,
			InAlertSince:    timestamppb.New(inAlertSince),
			Message:         "fault",
			TenantMessage:   &tenantMessage,
			Classifications: []string{"PreventAllocations"},
		}},
	})

	assert.Equal(t, "rack-aggregate-health", got.Source)
	assert.Equal(t, "2026-09-28T12:00:00Z", *got.ObservedAt)
	assert.Equal(t, "FanSpeed", got.Successes[0].ID)
	assert.Equal(t, "PowerSupply", got.Alerts[0].ID)
	assert.Equal(t, target, *got.Alerts[0].Target)
	assert.Equal(t, "2026-09-28T11:59:00Z", *got.Alerts[0].InAlertSince)
	assert.Equal(t, tenantMessage, *got.Alerts[0].TenantMessage)
}

func TestAPIAggregateHealthConversionsReplaceState(t *testing.T) {
	observedAt := time.Date(2026, time.September, 28, 12, 0, 0, 0, time.UTC)
	inAlertSince := observedAt.Add(-time.Minute)

	flowHealth := APIAggregateHealth{}
	flowHealth.FromFlowProto(&flowv1.HealthReport{
		ObservedAt: timestamppb.New(observedAt),
		Alerts: []*flowv1.HealthProbeAlert{{
			InAlertSince:    timestamppb.New(inAlertSince),
			Classifications: []string{"PreventAllocations"},
		}},
	})
	flowHealth.FromFlowProto(&flowv1.HealthReport{Alerts: []*flowv1.HealthProbeAlert{{}}})
	assert.Nil(t, flowHealth.ObservedAt)
	require.Len(t, flowHealth.Alerts, 1)
	assert.Nil(t, flowHealth.Alerts[0].InAlertSince)
	assert.NotNil(t, flowHealth.Alerts[0].Classifications)
	assert.Empty(t, flowHealth.Alerts[0].Classifications)
	assert.NotNil(t, flowHealth.Successes)
	assert.Empty(t, flowHealth.Successes)

	coreHealth := APIAggregateHealth{}
	coreHealth.FromProto(&corev1.HealthReport{ObservedAt: timestamppb.New(observedAt)})
	coreHealth.FromProto(&corev1.HealthReport{})
	assert.Nil(t, coreHealth.ObservedAt)
	assert.NotNil(t, coreHealth.Alerts)
	assert.NotNil(t, coreHealth.Successes)

	dbHealth := APIAggregateHealth{}
	dbHealth.FromDBModel(&cdbm.MachineHealth{})
	assert.NotNil(t, dbHealth.Alerts)
	assert.NotNil(t, dbHealth.Successes)
}

func TestAPIHealthProbeAlertFromProtoReplacesOptionalFields(t *testing.T) {
	inAlertSince := timestamppb.New(time.Date(2026, time.September, 28, 12, 0, 0, 0, time.UTC))
	alert := APIHealthProbeAlert{}
	alert.FromProto(&corev1.HealthProbeAlert{
		InAlertSince:    inAlertSince,
		Classifications: []string{"PreventAllocations"},
	})
	alert.FromProto(&corev1.HealthProbeAlert{})

	assert.Nil(t, alert.InAlertSince)
	assert.NotNil(t, alert.Classifications)
	assert.Empty(t, alert.Classifications)
}
