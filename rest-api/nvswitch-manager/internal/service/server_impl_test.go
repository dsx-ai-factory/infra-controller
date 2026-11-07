// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package service

import (
	"context"
	"testing"

	pb "github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/internal/proto/v1"
	"github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/pkg/firmwaremanager"
	"github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/pkg/redfish"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"
)

func TestResetTarget_InvalidIP(t *testing.T) {
	tests := map[string]struct {
		ip string
	}{
		"empty":   {ip: ""},
		"garbage": {ip: "bmc-bad-addr"},
		"partial": {ip: "172.16.0"},
	}

	for name, tc := range tests {
		t.Run(name, func(t *testing.T) {
			target := &pb.PowerTarget{
				BmcIp: tc.ip,
				BmcCredentials: &pb.Credentials{
					Username: "bmcAdmin",
					Password: "bmcSecret",
				},
			}

			resp := resetTarget(context.Background(), target, redfish.ResetOn)

			assert.Equal(t, pb.StatusCode_INVALID_ARGUMENT, resp.Status)
			assert.Equal(t, tc.ip, resp.BmcIp)
			assert.Contains(t, resp.Error, "invalid BMC IP")
		})
	}
}

func TestResetTarget_NilCredentials(t *testing.T) {
	target := &pb.PowerTarget{
		BmcIp:          "172.16.0.10",
		BmcCredentials: nil,
	}

	resp := resetTarget(context.Background(), target, redfish.ResetOn)

	assert.Equal(t, pb.StatusCode_INVALID_ARGUMENT, resp.Status)
	assert.Equal(t, "172.16.0.10", resp.BmcIp)
	assert.Contains(t, resp.Error, "bmc_credentials are required")
}

func TestResetTarget_EmptyCredentials(t *testing.T) {
	tests := map[string]struct {
		username string
		password string
	}{
		"empty username": {username: "", password: "bmcSecret"},
		"empty password": {username: "bmcAdmin", password: ""},
		"both empty":     {username: "", password: ""},
	}

	for name, tc := range tests {
		t.Run(name, func(t *testing.T) {
			target := &pb.PowerTarget{
				BmcIp: "172.16.0.10",
				BmcCredentials: &pb.Credentials{
					Username: tc.username,
					Password: tc.password,
				},
			}

			resp := resetTarget(context.Background(), target, redfish.ResetOn)

			assert.Equal(t, pb.StatusCode_INVALID_ARGUMENT, resp.Status)
			assert.Contains(t, resp.Error, "must not be empty")
		})
	}
}

func validFirmwareTarget() *pb.FirmwareTarget {
	return &pb.FirmwareTarget{
		BmcIp:           "10.0.0.1",
		BmcCredentials:  &pb.Credentials{Username: "admin", Password: "pass"},
		BmcPort:         443,
		NvosIp:          "10.0.0.2",
		NvosCredentials: &pb.Credentials{Username: "nvos", Password: "nvos_pass"},
		NvosPort:        22,
		BmcMac:          "AA:BB:CC:DD:EE:FF",
		NvosMac:         "11:22:33:44:55:66",
		Vendor:          pb.Vendor_VENDOR_NVIDIA,
	}
}

func TestValidateFirmwareTarget_Valid(t *testing.T) {
	assert.NoError(t, validateFirmwareTarget(validFirmwareTarget()))
}

func TestValidateFirmwareTarget_InvalidBmcIP(t *testing.T) {
	target := validFirmwareTarget()
	target.BmcIp = "not-an-ip"
	err := validateFirmwareTarget(target)
	require.Error(t, err)
	assert.Contains(t, err.Error(), "invalid BMC IP")
}

func TestValidateFirmwareTarget_MissingBmcCredentials(t *testing.T) {
	tests := map[string]struct {
		cred *pb.Credentials
	}{
		"nil credentials": {cred: nil},
		"empty username":  {cred: &pb.Credentials{Username: "", Password: "pass"}},
		"empty password":  {cred: &pb.Credentials{Username: "admin", Password: ""}},
	}

	for name, tc := range tests {
		t.Run(name, func(t *testing.T) {
			target := validFirmwareTarget()
			target.BmcCredentials = tc.cred
			err := validateFirmwareTarget(target)
			require.Error(t, err)
			assert.Contains(t, err.Error(), "bmc_credentials")
		})
	}
}

func TestValidateFirmwareTarget_InvalidNvosIP(t *testing.T) {
	target := validFirmwareTarget()
	target.NvosIp = "bad-ip"
	err := validateFirmwareTarget(target)
	require.Error(t, err)
	assert.Contains(t, err.Error(), "invalid NVOS IP")
}

func TestValidateFirmwareTarget_MissingNvosCredentials(t *testing.T) {
	tests := map[string]struct {
		cred *pb.Credentials
	}{
		"nil credentials": {cred: nil},
		"empty username":  {cred: &pb.Credentials{Username: "", Password: "pass"}},
		"empty password":  {cred: &pb.Credentials{Username: "nvos", Password: ""}},
	}

	for name, tc := range tests {
		t.Run(name, func(t *testing.T) {
			target := validFirmwareTarget()
			target.NvosCredentials = tc.cred
			err := validateFirmwareTarget(target)
			require.Error(t, err)
			assert.Contains(t, err.Error(), "nvos_credentials")
		})
	}
}

func TestValidateFirmwareTarget_MissingMAC(t *testing.T) {
	t.Run("missing bmc_mac", func(t *testing.T) {
		target := validFirmwareTarget()
		target.BmcMac = ""
		err := validateFirmwareTarget(target)
		require.Error(t, err)
		assert.Contains(t, err.Error(), "bmc_mac")
	})

	t.Run("missing nvos_mac", func(t *testing.T) {
		target := validFirmwareTarget()
		target.NvosMac = ""
		err := validateFirmwareTarget(target)
		require.Error(t, err)
		assert.Contains(t, err.Error(), "nvos_mac")
	})
}

func TestFirmwareTargetToRegisterRequest(t *testing.T) {
	target := validFirmwareTarget()
	req := firmwareTargetToRegisterRequest(target)

	assert.Equal(t, pb.Vendor_VENDOR_NVIDIA, req.Vendor)
	assert.Equal(t, "AA:BB:CC:DD:EE:FF", req.Bmc.MacAddress)
	assert.Equal(t, "10.0.0.1", req.Bmc.IpAddress)
	assert.Equal(t, "admin", req.Bmc.Credentials.Username)
	assert.Equal(t, "pass", req.Bmc.Credentials.Password)
	assert.Equal(t, int32(443), req.Bmc.Port)
	assert.Equal(t, "11:22:33:44:55:66", req.Nvos.MacAddress)
	assert.Equal(t, "10.0.0.2", req.Nvos.IpAddress)
	assert.Equal(t, "nvos", req.Nvos.Credentials.Username)
	assert.Equal(t, "nvos_pass", req.Nvos.Credentials.Password)
	assert.Equal(t, int32(22), req.Nvos.Port)
}

// TestQueueUpdates_NilFirmwareManager guards the early-return in QueueUpdates
// that protects against being called before the firmware manager has been
// initialized (e.g. when persistent mode is configured but the firmware
// packages directory is unset). Per-target validation is exercised by the
// TestValidateFirmwareTarget_* tests; driving end-to-end validation through
// the handler would require a non-nil fwm and is left for handler-level
// tests with a mocked FirmwareManager.
func TestQueueUpdates_NilFirmwareManager(t *testing.T) {
	srv := &NVSwitchManagerServerImpl{}

	resp, err := srv.QueueUpdates(context.Background(), &pb.QueueUpdatesRequest{})

	require.Error(t, err)
	assert.Nil(t, resp)
	st, ok := status.FromError(err)
	require.True(t, ok, "expected a gRPC status error, got %T", err)
	assert.Equal(t, codes.Unavailable, st.Code())
	assert.Contains(t, st.Message(), "firmware manager not initialized")
}

// TestQueueUpdates_MutualExclusivity locks in the contract that
// QueueUpdates rejects requests violating the switch_uuids/targets
// mutual-exclusivity constraint. See QueueUpdatesRequest in
// nvswitch-manager.proto for the full contract documentation.
func TestQueueUpdates_MutualExclusivity(t *testing.T) {
	cases := map[string]struct {
		req         *pb.QueueUpdatesRequest
		wantMessage string
	}{
		"empty request rejected (no work to do)": {
			req:         &pb.QueueUpdatesRequest{},
			wantMessage: "either switch_uuids or targets",
		},
		"both populated rejected (would risk duplicate updates)": {
			req: &pb.QueueUpdatesRequest{
				SwitchUuids: []string{"00000000-0000-0000-0000-000000000001"},
				Targets:     []*pb.FirmwareTarget{validFirmwareTarget()},
			},
			wantMessage: "not both",
		},
	}

	for name, tc := range cases {
		t.Run(name, func(t *testing.T) {
			// Non-nil fwm so we get past the Unavailable guard. The
			// mutual-exclusivity validation rejects before any fwm method
			// is invoked, so a zero-value pointer is sufficient.
			srv := &NVSwitchManagerServerImpl{fwm: &firmwaremanager.FirmwareManager{}}

			resp, err := srv.QueueUpdates(context.Background(), tc.req)

			require.Error(t, err)
			assert.Nil(t, resp)
			st, ok := status.FromError(err)
			require.True(t, ok, "expected a gRPC status error, got %T", err)
			assert.Equal(t, codes.InvalidArgument, st.Code())
			assert.Contains(t, st.Message(), tc.wantMessage)
		})
	}
}
