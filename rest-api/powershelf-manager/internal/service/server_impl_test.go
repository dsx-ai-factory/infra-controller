// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package service

import (
	"context"
	"testing"

	pb "github.com/NVIDIA/infra-controller/rest-api/powershelf-manager/internal/proto/v1"
	"github.com/NVIDIA/infra-controller/rest-api/powershelf-manager/pkg/powershelfmanager"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"
)

// newTestServer returns a PowershelfManagerServerImpl with a non-nil but
// zero-value PowershelfManager so handlers can pass their nil-guard. Tests
// that need to exercise the nil-guard path itself (e.g.
// TestUpdateFirmware_NilPowershelfManager) construct an empty
// PowershelfManagerServerImpl directly.
func newTestServer() *PowershelfManagerServerImpl {
	return &PowershelfManagerServerImpl{psm: &powershelfmanager.PowershelfManager{}}
}

func TestPowerTarget_InvalidIP(t *testing.T) {
	tests := map[string]struct {
		ip string
	}{
		"empty":   {ip: ""},
		"garbage": {ip: "pmc-bad-addr"},
		"partial": {ip: "10.20.30"},
	}

	s := newTestServer()

	for name, tc := range tests {
		t.Run(name, func(t *testing.T) {
			target := &pb.PowerTarget{
				PmcIp: tc.ip,
				PmcCredentials: &pb.Credentials{
					Username: "pmcUser",
					Password: "pmcPass",
				},
			}

			resp := s.powerTarget(context.Background(), target, true)

			assert.Equal(t, pb.StatusCode_INVALID_ARGUMENT, resp.Status)
			assert.Equal(t, tc.ip, resp.PmcIp)
			assert.Contains(t, resp.Error, "invalid PMC IP")
		})
	}
}

func TestPowerTarget_NilCredentials(t *testing.T) {
	s := newTestServer()
	target := &pb.PowerTarget{
		PmcIp:          "10.20.30.40",
		PmcCredentials: nil,
	}

	resp := s.powerTarget(context.Background(), target, true)

	assert.Equal(t, pb.StatusCode_INVALID_ARGUMENT, resp.Status)
	assert.Equal(t, "10.20.30.40", resp.PmcIp)
	assert.Contains(t, resp.Error, "credentials are required")
}

func TestPowerTarget_EmptyCredentials(t *testing.T) {
	tests := map[string]struct {
		username string
		password string
	}{
		"empty username": {username: "", password: "pmcPass"},
		"empty password": {username: "pmcUser", password: ""},
		"both empty":     {username: "", password: ""},
	}

	s := newTestServer()

	for name, tc := range tests {
		t.Run(name, func(t *testing.T) {
			target := &pb.PowerTarget{
				PmcIp: "10.20.30.40",
				PmcCredentials: &pb.Credentials{
					Username: tc.username,
					Password: tc.password,
				},
			}

			resp := s.powerTarget(context.Background(), target, true)

			assert.Equal(t, pb.StatusCode_INVALID_ARGUMENT, resp.Status)
			assert.Contains(t, resp.Error, "must not be empty")
		})
	}
}

func validFirmwareTarget() *pb.FirmwareTarget {
	return &pb.FirmwareTarget{
		PmcMacAddress: "00:11:22:33:44:55",
		PmcIpAddress:  "10.20.30.40",
		PmcCredentials: &pb.Credentials{
			Username: "pmcUser",
			Password: "pmcPass",
		},
		PmcVendor: pb.PMCVendor_PMC_TYPE_LITEON,
	}
}

func TestValidateFirmwareTarget_Valid(t *testing.T) {
	err := validateFirmwareTarget(validFirmwareTarget())
	assert.NoError(t, err)
}

func TestValidateFirmwareTarget_NilTarget(t *testing.T) {
	err := validateFirmwareTarget(nil)
	assert.Error(t, err)
	assert.Contains(t, err.Error(), "firmware target is required")
}

func TestValidateFirmwareTarget_InvalidMAC(t *testing.T) {
	tests := map[string]struct {
		mac string
	}{
		"empty":   {mac: ""},
		"garbage": {mac: "not-a-mac"},
		"partial": {mac: "00:11:22"},
	}

	for name, tc := range tests {
		t.Run(name, func(t *testing.T) {
			target := validFirmwareTarget()
			target.PmcMacAddress = tc.mac
			err := validateFirmwareTarget(target)
			assert.Error(t, err)
		})
	}
}

func TestValidateFirmwareTarget_InvalidIP(t *testing.T) {
	tests := map[string]struct {
		ip string
	}{
		"empty":   {ip: ""},
		"garbage": {ip: "pmc-bad-addr"},
		"partial": {ip: "10.20.30"},
	}

	for name, tc := range tests {
		t.Run(name, func(t *testing.T) {
			target := validFirmwareTarget()
			target.PmcIpAddress = tc.ip
			err := validateFirmwareTarget(target)
			assert.Error(t, err)
			assert.Contains(t, err.Error(), "invalid pmc_ip_address")
		})
	}
}

func TestValidateFirmwareTarget_MissingCredentials(t *testing.T) {
	tests := map[string]struct {
		creds *pb.Credentials
	}{
		"nil credentials":    {creds: nil},
		"empty username":     {creds: &pb.Credentials{Username: "", Password: "pass"}},
		"empty password":     {creds: &pb.Credentials{Username: "user", Password: ""}},
		"both empty strings": {creds: &pb.Credentials{Username: "", Password: ""}},
	}

	for name, tc := range tests {
		t.Run(name, func(t *testing.T) {
			target := validFirmwareTarget()
			target.PmcCredentials = tc.creds
			err := validateFirmwareTarget(target)
			assert.Error(t, err)
			assert.Contains(t, err.Error(), "pmc_credentials")
		})
	}
}

func TestFirmwareTargetToRegisterRequest(t *testing.T) {
	target := validFirmwareTarget()
	req := firmwareTargetToRegisterRequest(target)

	require.NotNil(t, req)
	assert.Equal(t, target.PmcMacAddress, req.PmcMacAddress)
	assert.Equal(t, target.PmcIpAddress, req.PmcIpAddress)
	assert.Equal(t, target.PmcVendor, req.PmcVendor)
	assert.Equal(t, target.PmcCredentials, req.PmcCredentials)
}

func TestUpdateFirmware_InvalidTarget(t *testing.T) {
	s := newTestServer()

	req := &pb.UpdateFirmwareRequest{
		Targets: []*pb.UpdateFirmwareTargetRequest{
			{
				Target: &pb.FirmwareTarget{
					PmcMacAddress:  "invalid-mac",
					PmcIpAddress:   "10.20.30.40",
					PmcCredentials: &pb.Credentials{Username: "u", Password: "p"},
				},
				Components: []*pb.UpdateComponentFirmwareRequest{
					{Component: pb.PowershelfComponent_PMC, UpgradeTo: &pb.FirmwareVersion{Version: "1.0.0"}},
				},
			},
		},
	}

	resp, err := s.UpdateFirmware(context.Background(), req)
	assert.NoError(t, err)
	require.Len(t, resp.Responses, 1)
	require.Len(t, resp.Responses[0].Components, 1)
	assert.Equal(t, pb.StatusCode_INVALID_ARGUMENT, resp.Responses[0].Components[0].Status)
}

// TestUpdateFirmware_NilPowershelfManager guards the early-return in
// UpdateFirmware that protects against being called before the powershelf
// manager has been initialized. Without this guard the handler would
// nil-dereference inside upgradeComponents → updateFirmware → s.psm.UpgradeFirmware.
func TestUpdateFirmware_NilPowershelfManager(t *testing.T) {
	s := &PowershelfManagerServerImpl{}

	// Use a populated request so we'd otherwise survive validation; the
	// nil-guard must fire before mutual-exclusivity checks.
	req := &pb.UpdateFirmwareRequest{
		Upgrades: []*pb.UpdatePowershelfFirmwareRequest{
			{PmcMacAddress: "00:11:22:33:44:55"},
		},
	}

	resp, err := s.UpdateFirmware(context.Background(), req)

	require.Error(t, err)
	assert.Nil(t, resp)
	st, ok := status.FromError(err)
	require.True(t, ok, "expected a gRPC status error, got %T", err)
	assert.Equal(t, codes.Unavailable, st.Code())
	assert.Contains(t, st.Message(), "powershelf manager not initialized")
}

// TestUpdateFirmware_MutualExclusivity locks in the contract that
// UpdateFirmware rejects requests violating the upgrades/targets
// mutual-exclusivity constraint. See UpdateFirmwareRequest in
// powershelf-manager.proto for the full contract documentation.
func TestUpdateFirmware_MutualExclusivity(t *testing.T) {
	cases := map[string]struct {
		req         *pb.UpdateFirmwareRequest
		wantMessage string
	}{
		"empty request rejected (no work to do)": {
			req:         &pb.UpdateFirmwareRequest{},
			wantMessage: "either upgrades or targets",
		},
		"both populated rejected (would risk duplicate updates)": {
			req: &pb.UpdateFirmwareRequest{
				Upgrades: []*pb.UpdatePowershelfFirmwareRequest{
					{PmcMacAddress: "00:11:22:33:44:55"},
				},
				Targets: []*pb.UpdateFirmwareTargetRequest{
					{Target: validFirmwareTarget()},
				},
			},
			wantMessage: "not both",
		},
	}

	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			s := newTestServer()

			resp, err := s.UpdateFirmware(context.Background(), tc.req)

			require.Error(t, err)
			assert.Nil(t, resp)
			st, ok := status.FromError(err)
			require.True(t, ok, "expected a gRPC status error, got %T", err)
			assert.Equal(t, codes.InvalidArgument, st.Code())
			assert.Contains(t, st.Message(), tc.wantMessage)
		})
	}
}
