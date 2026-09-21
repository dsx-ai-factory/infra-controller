// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package component

import (
	"net"
	"testing"

	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/deviceinfo"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/bmc"
	"github.com/stretchr/testify/assert"
)

func TestNewComponent(t *testing.T) {
	typ := devicetypes.ComponentTypeCompute
	info := deviceinfo.NewRandom("for- testing", 8)
	fw := "1.0.0"
	pos := InRackPosition{SlotID: 1, TrayIndex: 2, HostID: 3}

	c1 := New(typ, nil, fw, nil)
	assert.Equal(t, typ, c1.Type)
	assert.Equal(t, deviceinfo.DeviceInfo{}, c1.Info)
	assert.Equal(t, fw, c1.FirmwareVersion)
	assert.Equal(t, InRackPosition{}, c1.Position)
	assert.NotNil(t, c1.BmcsByType)
	assert.NotNil(t, c1.bmcMacToID)

	c2 := New(typ, &info, fw, &pos)
	assert.Equal(t, typ, c2.Type)
	assert.Equal(t, info, c2.Info)
	assert.Equal(t, fw, c2.FirmwareVersion)
	assert.Equal(t, pos, c2.Position)
	assert.NotNil(t, c2.BmcsByType)
	assert.NotNil(t, c2.bmcMacToID)
}

func TestAddBMC(t *testing.T) {
	bmcTypes := []devicetypes.BMCType{devicetypes.BMCTypeHost, devicetypes.BMCTypeDPU}
	bmcMacs := []string{"00:11:22:33:44:55", "aa:bb:cc:dd:ee:ff"}
	assert.Equal(t, 2, len(bmcTypes))
	assert.Equal(t, len(bmcTypes), len(bmcMacs))

	bmcs := make([]bmc.BMC, 0, len(bmcMacs))
	for _, m := range bmcMacs {
		parsed, err := net.ParseMAC(m)
		assert.NoError(t, err)
		bmcs = append(bmcs, bmc.BMC{MAC: bmc.MACAddress{HardwareAddr: parsed}})
	}

	comp := New(devicetypes.ComponentTypeCompute, nil, "", nil)

	// Add the first one
	added := comp.AddBMC(bmcTypes[0], bmcs[0])
	assert.True(t, added)
	assert.Equal(t, 1, len(comp.BmcsByType[bmcTypes[0]]))
	assert.Equal(t, bmcs[0], comp.BmcsByType[bmcTypes[0]][0])
	assert.Equal(t, 1, len(comp.bmcMacToID))
	assert.Equal(t, bmcTypes[0], comp.bmcMacToID[bmcs[0].MAC.String()].typ)
	assert.Equal(t, 0, comp.bmcMacToID[bmcs[0].MAC.String()].index)

	// Add the first one again
	added = comp.AddBMC(bmcTypes[0], bmcs[0])
	assert.False(t, added)
	assert.Equal(t, 1, len(comp.BmcsByType[bmcTypes[0]]))
	assert.Equal(t, 1, len(comp.bmcMacToID))

	// Add the second one
	added = comp.AddBMC(bmcTypes[1], bmcs[1])
	assert.True(t, added)
	assert.Equal(t, 1, len(comp.BmcsByType[bmcTypes[1]]))
	assert.Equal(t, bmcs[1], comp.BmcsByType[bmcTypes[1]][0])
	assert.Equal(t, 2, len(comp.bmcMacToID))
	assert.Equal(t, bmcTypes[1], comp.bmcMacToID[bmcs[1].MAC.String()].typ)
	assert.Equal(t, 0, comp.bmcMacToID[bmcs[1].MAC.String()].index)
}

func TestComponent_ManagementMAC(t *testing.T) {
	hostMAC, err := net.ParseMAC("aa:bb:cc:dd:ee:02")
	assert.NoError(t, err)
	secondHostMAC, err := net.ParseMAC("aa:bb:cc:dd:ee:01")
	assert.NoError(t, err)
	dpuMAC, err := net.ParseMAC("00:11:22:33:44:55")
	assert.NoError(t, err)

	comp := New(devicetypes.ComponentTypeCompute, nil, "", nil)
	comp.AddBMC(devicetypes.BMCTypeDPU, bmc.BMC{MAC: bmc.MACAddress{HardwareAddr: dpuMAC}})
	comp.AddBMC(devicetypes.BMCTypeHost, bmc.BMC{MAC: bmc.MACAddress{HardwareAddr: hostMAC}})
	comp.AddBMC(devicetypes.BMCTypeHost, bmc.BMC{MAC: bmc.MACAddress{HardwareAddr: secondHostMAC}})

	assert.Equal(t, "aa:bb:cc:dd:ee:01", comp.ManagementMAC())
}

func TestIsCompute(t *testing.T) {
	comp := New(devicetypes.ComponentTypeCompute, nil, "", nil)
	assert.True(t, comp.IsCompute())

	comp = New(devicetypes.ComponentTypeNVSwitch, nil, "", nil)
	assert.False(t, comp.IsCompute())
}

func TestPatch(t *testing.T) {
	comp := New(devicetypes.ComponentTypeCompute, nil, "1.0.0", nil)
	newComp := New(devicetypes.ComponentTypeCompute, nil, "1.0.1", nil)

	patched := comp.Patch(newComp, false)
	assert.True(t, patched)
	assert.Equal(t, newComp.FirmwareVersion, comp.FirmwareVersion)
}

func TestInRackPositionCompare(t *testing.T) {
	pos1 := InRackPosition{SlotID: 2}
	pos2 := InRackPosition{SlotID: 1}

	assert.True(t, pos1.Compare(pos2, true))
	assert.False(t, pos1.Compare(pos2, false))
	assert.False(t, pos2.Compare(pos1, true))
	assert.False(t, pos2.Compare(pos1, false))
}
