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
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

func TestAPICredentialRotationRequestValidate(t *testing.T) {
	siteID := uuid.NewString()
	cases := []struct {
		name    string
		req     APICredentialRotationRequest
		wantErr bool
	}{
		{"bmc ok", APICredentialRotationRequest{SiteID: siteID, CredentialType: CredentialRotationTypeBMC}, false},
		{"nvos ok", APICredentialRotationRequest{SiteID: siteID, CredentialType: CredentialRotationTypeNVOS}, false},
		{"password optional", APICredentialRotationRequest{SiteID: siteID, CredentialType: CredentialRotationTypeBMC, Password: cutil.GetPtr("pw")}, false},
		{"empty password rejected", APICredentialRotationRequest{SiteID: siteID, CredentialType: CredentialRotationTypeBMC, Password: cutil.GetPtr("")}, true},
		{"missing siteId", APICredentialRotationRequest{CredentialType: CredentialRotationTypeBMC}, true},
		{"invalid siteId", APICredentialRotationRequest{SiteID: "not-a-uuid", CredentialType: CredentialRotationTypeBMC}, true},
		{"missing credentialType", APICredentialRotationRequest{SiteID: siteID}, true},
		{"invalid credentialType", APICredentialRotationRequest{SiteID: siteID, CredentialType: CredentialRotationType("nope")}, true},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			err := tc.req.Validate()
			if tc.wantErr {
				assert.Error(t, err)
			} else {
				assert.NoError(t, err)
			}
		})
	}
}

func TestValidateCredentialRotationType(t *testing.T) {
	cases := []struct {
		name    string
		value   CredentialRotationType
		wantErr bool
	}{
		{"bmc", CredentialRotationTypeBMC, false},
		{"host uefi", CredentialRotationTypeHostUEFI, false},
		{"dpu uefi", CredentialRotationTypeDPUUEFI, false},
		{"nvos", CredentialRotationTypeNVOS, false},
		{"lockdown ikm", CredentialRotationTypeLockdownIKM, false},
		{"empty", CredentialRotationType(""), true},
		{"unknown", CredentialRotationType("SomethingElse"), true},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			err := ValidateCredentialRotationType(tc.value)
			if tc.wantErr {
				assert.Error(t, err)
			} else {
				assert.NoError(t, err)
			}
		})
	}
}

func TestCredentialRotationTypeToProto(t *testing.T) {
	cases := []struct {
		apiType CredentialRotationType
		proto   corev1.RotationCredentialType
	}{
		{CredentialRotationTypeBMC, corev1.RotationCredentialType_ROTATION_BMC},
		{CredentialRotationTypeHostUEFI, corev1.RotationCredentialType_ROTATION_HOST_UEFI},
		{CredentialRotationTypeDPUUEFI, corev1.RotationCredentialType_ROTATION_DPU_UEFI},
		{CredentialRotationTypeNVOS, corev1.RotationCredentialType_ROTATION_NVOS},
		{CredentialRotationTypeLockdownIKM, corev1.RotationCredentialType_ROTATION_LOCKDOWN_IKM},
	}
	for _, tc := range cases {
		t.Run(string(tc.apiType), func(t *testing.T) {
			// ToProto and the reverse lookup must round-trip so the two
			// derived views of the mapping cannot drift.
			assert.Equal(t, tc.proto, tc.apiType.ToProto())
			var got CredentialRotationType
			got.FromProto(tc.proto)
			assert.Equal(t, tc.apiType, got)
		})
	}
}

func TestAPICredentialRotationRequestToProto(t *testing.T) {
	req := APICredentialRotationRequest{
		SiteID:         uuid.NewString(),
		CredentialType: CredentialRotationTypeNVOS,
		Password:       cutil.GetPtr("pw"),
		Reason:         cutil.GetPtr("annual rotation"),
	}
	p := req.ToProto()
	assert.Equal(t, corev1.RotationCredentialType_ROTATION_NVOS, p.GetCredentialType())
	assert.Equal(t, "pw", p.GetPassword())
	assert.Equal(t, "annual rotation", p.GetReason())

	// Optional fields omitted stay nil on the proto (presence preserved).
	bare := APICredentialRotationRequest{SiteID: req.SiteID, CredentialType: CredentialRotationTypeBMC}
	bp := bare.ToProto()
	assert.Nil(t, bp.Password)
	assert.Nil(t, bp.Reason)
}

func TestAPICredentialRotationResultFromProto(t *testing.T) {
	started := time.Now().Add(-time.Hour)
	result := &APICredentialRotationResult{}
	result.FromProto(&corev1.RotateCredentialResult{
		CredentialType: corev1.RotationCredentialType_ROTATION_BMC,
		TargetVersion:  7,
		StartedAt:      timestamppb.New(started),
	})
	assert.Equal(t, CredentialRotationTypeBMC, result.CredentialType)
	assert.Equal(t, uint32(7), result.TargetVersion)
	require.NotNil(t, result.Started)
	assert.WithinDuration(t, started, *result.Started, time.Second)

	// A response body must never surface a password field.
	body, err := json.Marshal(result)
	require.NoError(t, err)
	assert.NotContains(t, string(body), "password")
}

func TestAPICredentialRotationStatusFromProto(t *testing.T) {
	quarantinedUntil := time.Now().Add(15 * time.Minute)
	lastAttempt := time.Now().Add(-5 * time.Minute)
	status := &APICredentialRotationStatus{}
	status.FromProto(&corev1.CredentialRotationStatusResult{
		TargetVersion:         3,
		Converged:             5,
		Pending:               2,
		Quarantined:           1,
		QuarantinedDeviceMacs: []string{"aa:bb:cc:dd:ee:ff"},
		StartedAt:             timestamppb.New(time.Now().Add(-2 * time.Hour)),
		Complete:              false,
		Device: &corev1.DeviceCredentialRotationStatus{
			DeviceMac:        "aa:bb:cc:dd:ee:ff",
			CurrentVersion:   cutil.GetPtr(uint32(2)),
			Converged:        false,
			Quarantined:      true,
			QuarantinedUntil: timestamppb.New(quarantinedUntil),
			RotateAttempts:   4,
			LastAttemptAt:    timestamppb.New(lastAttempt),
			LastError:        cutil.GetPtr("redacted login failure"),
		},
	})

	assert.Equal(t, uint32(3), status.TargetVersion)
	assert.Equal(t, uint64(5), status.Converged)
	assert.Equal(t, uint64(2), status.Pending)
	assert.Equal(t, uint64(1), status.Quarantined)
	assert.Equal(t, []string{"aa:bb:cc:dd:ee:ff"}, status.QuarantinedDeviceMacs)
	require.NotNil(t, status.Started)
	assert.False(t, status.Complete)

	require.NotNil(t, status.Device)
	assert.Equal(t, "aa:bb:cc:dd:ee:ff", status.Device.DeviceMac)
	require.NotNil(t, status.Device.CurrentVersion)
	assert.Equal(t, uint32(2), *status.Device.CurrentVersion)
	assert.Nil(t, status.Device.RotatingToVersion)
	assert.True(t, status.Device.Quarantined)
	require.NotNil(t, status.Device.QuarantinedUntil)
	assert.Equal(t, uint32(4), status.Device.RotateAttempts)
	require.NotNil(t, status.Device.LastAttempted)
	require.NotNil(t, status.Device.LastError)
	assert.Equal(t, "redacted login failure", *status.Device.LastError)
}

func TestAPICredentialRotationStatusFromProtoSiteWide(t *testing.T) {
	// A site-wide (non-device) query omits the per-device block.
	status := &APICredentialRotationStatus{}
	status.FromProto(&corev1.CredentialRotationStatusResult{
		TargetVersion: 1,
		Converged:     10,
		Complete:      true,
	})
	assert.True(t, status.Complete)
	assert.Nil(t, status.Device)
	assert.Nil(t, status.Started)
}
