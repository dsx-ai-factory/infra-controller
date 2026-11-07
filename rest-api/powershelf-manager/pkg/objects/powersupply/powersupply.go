// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package powersupply

import (
	"fmt"
	"strings"

	"github.com/stmcginnis/gofish/common"
	"github.com/stmcginnis/gofish/redfish"
)

// PowerSupply is a snapshot of a Power Supply Unit.
type PowerSupply struct {
	common.Entity
	CapacityWatts string
	// FirmwareVersion shall contain the firmware version as
	// defined by the manufacturer for the associated power supply.
	FirmwareVersion string
	HardwareVersion string
	// Location shall contain location information of the
	// associated power supply.
	Location redfish.PartLocation
	// HotPluggable shall indicate whether the device can be inserted or removed while the underlying equipment
	// otherwise remains in its current operational state. Devices indicated as hot-pluggable shall allow the device to
	// become operable without altering the operational state of the underlying equipment. Devices that cannot be
	// inserted or removed from equipment in operation, or devices that cannot become operable without affecting the
	// operational state of that equipment, shall be indicated as not hot-pluggable.
	HotPluggable   bool
	InputSourceNum int
	// Manufacturer shall be the name of the
	// organization responsible for producing the power supply. This
	// organization might be the entity from whom the power supply is
	// purchased, but this is not necessarily true.
	Manufacturer string
	// Model shall contain the model information as defined
	// by the manufacturer for the associated power supply.
	Model      string
	PowerState bool
	// SerialNumber shall contain the serial number as
	// defined by the manufacturer for the associated power supply.
	SerialNumber string
	Sensors      []*redfish.Sensor
	// Status shall contain any status or health properties
	// of the resource.
	Status common.Status
}

// Summary returns a summary of the PowerSupply as a string.
func (psu *PowerSupply) Summary() string {
	var sb strings.Builder

	fmt.Fprintf(&sb, "%s Summary:\n", psu.Name)
	fmt.Fprintf(&sb, "  ID: %s\n", psu.ID)
	fmt.Fprintf(&sb, "  Name: %s\n", psu.Name)
	fmt.Fprintf(&sb, "  CapacityWatts: %s\n", psu.CapacityWatts)
	fmt.Fprintf(&sb, "  FirmwareVersion: %s\n", psu.FirmwareVersion)
	fmt.Fprintf(&sb, "  HardwareVersion: %s\n", psu.HardwareVersion)
	fmt.Fprintf(&sb, "  HotPluggable: %t\n", psu.HotPluggable)
	fmt.Fprintf(&sb, "  InputSourceNum: %d\n", psu.InputSourceNum)
	fmt.Fprintf(&sb, "  Manufacturer: %s\n", psu.Manufacturer)
	fmt.Fprintf(&sb, "  Model: %s\n", psu.Model)
	fmt.Fprintf(&sb, "  PowerState: %t\n", psu.PowerState)
	fmt.Fprintf(&sb, "  SerialNumber: %s\n", psu.SerialNumber)
	fmt.Fprintf(&sb, "  Status: %s\n", psu.Status.State)
	fmt.Fprintf(&sb, "  Health: %s\n", psu.Status.Health)
	sb.WriteString("  Location:\n")
	fmt.Fprintf(&sb, "    LocationOrdinalValue: %d\n", psu.Location.LocationOrdinalValue)
	fmt.Fprintf(&sb, "    LocationType: %s\n", psu.Location.LocationType)
	fmt.Fprintf(&sb, "    ServiceLabel: %s\n", psu.Location.ServiceLabel)
	sb.WriteString("  Sensors:\n")
	for _, sensor := range psu.Sensors {
		fmt.Fprintf(&sb, "    @odataid: %s\n", sensor.ODataID)
		fmt.Fprintf(&sb, "    Id: %s\n", sensor.ID)
		fmt.Fprintf(&sb, "    Name: %s\n", sensor.Name)
		fmt.Fprintf(&sb, "    Reading: %f\n", sensor.Reading)
		fmt.Fprintf(&sb, "    ReadingRangeMax: %f\n", sensor.ReadingRangeMax)
		fmt.Fprintf(&sb, "    ReadingRangeMin: %f\n", sensor.ReadingRangeMin)
		fmt.Fprintf(&sb, "    ReadingType: %s\n", sensor.ReadingType)
		fmt.Fprintf(&sb, "    ReadingUnits: %s\n", sensor.ReadingUnits)
		sb.WriteString("    Thresholds:\n")
		fmt.Fprintf(&sb, "        LowerCaution: %f\n", sensor.Thresholds.LowerCaution.Reading)
		fmt.Fprintf(&sb, "        LowerCritical: %f\n", sensor.Thresholds.LowerCritical.Reading)
		fmt.Fprintf(&sb, "        UpperCaution: %f\n", sensor.Thresholds.UpperCaution.Reading)
		fmt.Fprintf(&sb, "        UpperCritical: %f\n", sensor.Thresholds.UpperCritical.Reading)
	}

	return sb.String()
}
