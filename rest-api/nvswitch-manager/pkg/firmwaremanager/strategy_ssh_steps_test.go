// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package firmwaremanager

import (
	"testing"

	"github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/pkg/objects/nvswitch"

	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
)

func TestSSHStrategy_Steps(t *testing.T) {
	bundleID := uuid.MustParse("d1f1b2db-ec20-424a-996d-7cc29c5a9d93")
	tests := []struct {
		name   string
		update FirmwareUpdate
		want   []UpdateState
	}{
		{
			name:   "standalone NVOS checks reachability without power cycle",
			update: FirmwareUpdate{Component: nvswitch.NVOS},
			want: []UpdateState{
				StateWaitReachable,
				StateCopy,
				StateUpload,
				StateInstall,
				StateVerify,
				StateCleanup,
			},
		},
		{
			name: "first bundle NVOS checks reachability without power cycle",
			update: FirmwareUpdate{
				Component:      nvswitch.NVOS,
				BundleUpdateID: &bundleID,
				SequenceOrder:  1,
			},
			want: []UpdateState{
				StateWaitReachable,
				StateCopy,
				StateUpload,
				StateInstall,
				StateVerify,
				StateCleanup,
			},
		},
		{
			name: "later bundle NVOS power cycles before checking reachability",
			update: FirmwareUpdate{
				Component:      nvswitch.NVOS,
				BundleUpdateID: &bundleID,
				SequenceOrder:  2,
			},
			want: []UpdateState{
				StatePowerCycle,
				StateWaitReachable,
				StateCopy,
				StateUpload,
				StateInstall,
				StateVerify,
				StateCleanup,
			},
		},
		{
			name: "later bundle CPLD checks reachability without power cycle",
			update: FirmwareUpdate{
				Component:      nvswitch.CPLD,
				BundleUpdateID: &bundleID,
				SequenceOrder:  2,
			},
			want: []UpdateState{
				StateWaitReachable,
				StateCopy,
				StateUpload,
				StateInstall,
				StateVerify,
				StateCleanup,
			},
		},
		{
			name:   "default component uses standard SSH phases",
			update: FirmwareUpdate{Component: nvswitch.BMC},
			want: []UpdateState{
				StateCopy,
				StateUpload,
				StateInstall,
				StateVerify,
				StateCleanup,
			},
		},
	}

	strategy := &SSHStrategy{}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			assert.Equal(t, tt.want, strategy.Steps(&tt.update))
		})
	}
}
