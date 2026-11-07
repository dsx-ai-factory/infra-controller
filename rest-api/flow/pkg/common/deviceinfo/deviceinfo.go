// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package deviceinfo

import (
	"fmt"

	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/utils"
	"github.com/google/uuid"
)

// SerialInfo represents serial number information of a device which is
// composed of manufacturer and serial number to identify a device.
type SerialInfo struct {
	Manufacturer string `json:"manufacturer"`
	SerialNumber string `json:"serial_number"`
}

func (si *SerialInfo) String() string {
	return fmt.Sprintf("%s-%s", si.Manufacturer, si.SerialNumber)
}

// DeviceInfo represents hardware device information.
type DeviceInfo struct {
	ID           uuid.UUID `json:"id"`            // Unique device identifier
	Name         string    `json:"name"`          // Human-readble device name
	Manufacturer string    `json:"manufacturer"`  // Device manufacturer
	Model        string    `json:"model"`         // Device model
	SerialNumber string    `json:"serial_number"` // Unique serial number
	Description  string    `json:"description"`   // Optional description
}

// NewRandom generates a DeviceInfo with random serial number.
func NewRandom(name string, serialLen int) DeviceInfo {
	return DeviceInfo{
		ID:           uuid.New(),
		Name:         name,
		Manufacturer: "NVIDIA",
		Model:        "Model X",
		SerialNumber: utils.GenerateRandomSerial(serialLen),
		Description:  "randomly generated",
	}
}

// InfoMsg returns a formatted string representation of the device for
// logging/display. If byID is true, uses ID for identification; otherwise
// uses manufacturer and serial
func (di *DeviceInfo) InfoMsg(typ string, byID bool) string {
	if byID {
		return fmt.Sprintf("%s [id: %v]", typ, di.ID)
	}

	return fmt.Sprintf(
		"%s [manufacturer: %s, serial: %s]",
		typ, di.Manufacturer, di.SerialNumber,
	)
}

// GetSerialInfo returns the serial number information of the device.
func (di *DeviceInfo) GetSerialInfo() SerialInfo {
	return SerialInfo{
		Manufacturer: di.Manufacturer,
		SerialNumber: di.SerialNumber,
	}
}

// VerifyIDOrSerial verifies the validity of ID or serial information of
// the device.
func (di *DeviceInfo) VerifyIDOrSerial() bool {
	return di.ID != uuid.Nil || di.SerialNumber != ""
}

// BuildPatchedDeviceInfo builds a patched device info from the current device
// info and the input device info. It goes through the patchable fields and
// builds the patched device info. If there is no change on patchable fields,
// it returns nil.
func (di *DeviceInfo) BuildPatchedDeviceInfo(cur *DeviceInfo) *DeviceInfo {
	if di == nil || cur == nil {
		return nil
	}

	// Make a copy fo the current device info which serves as the base for the
	// patched device info.
	patchedInfo := *cur
	patched := 0

	// Go through the patchable fields which include Name and Description.
	if patchedInfo.Name != di.Name {
		patchedInfo.Name = di.Name
		patched++
	}

	if patchedInfo.Description != di.Description {
		patchedInfo.Description = di.Description
		patched++
	}

	if patched == 0 {
		return nil
	}

	return &patchedInfo
}
