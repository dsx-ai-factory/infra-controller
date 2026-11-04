// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"testing"

	"github.com/stretchr/testify/assert"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

func TestNewAPIMachineDisks(t *testing.T) {
	size := uint32(3662830)
	pci := "/devices/pci0000:00/0000:00:0a.0/0000:01:00.0/nvme/nvme2"
	info := &corev1.DiscoveryInfo{
		NvmeDevices: []*corev1.NvmeDevice{
			{Model: "Dell BOSS-N1", Serial: "CN0WW56VFCP0042T04MQ", PciPath: &pci},
			{Model: "SAMSUNG MZWLJ3T8HBLS-00007", Serial: "S4YPNE0NA02595", SizeMb: &size},
		},
		BlockDevices: []*corev1.BlockDevice{
			{Model: "SAMSUNG MZWLJ3T8HBLS-00007", Serial: "S4YPNE0NA02595", DeviceType: "disk"},
			{Model: "SATA SSD", Serial: "SATA1", DeviceType: "disk"},
			{Model: "Virtual CD", Serial: "CD1", DeviceType: "cdrom"},
		},
	}

	disks := newAPIMachineDisks(info)

	assert.Len(t, disks, 3, "duplicate serial and non-disk devices are dropped")
	assert.Equal(t, "Dell BOSS-N1", disks[0].Model)
	assert.Nil(t, disks[0].SizeMb)
	assert.Equal(t, pci, *disks[0].PciPath)
	assert.Equal(t, size, *disks[1].SizeMb)
	assert.Equal(t, "SATA1", disks[2].Serial)
	assert.Empty(t, newAPIMachineDisks(nil))
}
