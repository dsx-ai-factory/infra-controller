// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package protobuf

import (
	"testing"

	"github.com/stretchr/testify/require"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/secret"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	pb "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/proto/v1"
)

func TestDeviceCredentialUnsupportedOperationPaths(t *testing.T) {
	upgrade := &pb.UpgradeFirmwareRequest{DeviceCredentialOverrides: &pb.DeviceCredentialOverrides{Bmc: &pb.DeviceLogin{Username: "operator", Password: "device-value"}}}
	cases := []struct {
		name string
		run  func() error
	}{
		{"scheduled template", func() error {
			_, _, _, _, err := ScheduledOperationFrom(&pb.ScheduledOperation{Operation: &pb.ScheduledOperation_UpgradeFirmware{UpgradeFirmware: upgrade}})
			return err
		}},
		{"operation run template", func() error { _, err := upgradeFirmwareOperationFrom(upgrade); return err }},
		{"reusable task operation", func() error {
			_, err := TaskOperationTo(&operations.FirmwareControlTaskInfo{Operation: operations.FirmwareOperationUpgrade, DeviceCredentials: &secret.EncryptedData{Version: 1}})
			return err
		}},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			err := tc.run()
			require.Error(t, err)
			require.NotContains(t, err.Error(), "device-value")
		})
	}
}
