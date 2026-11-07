// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package powersupply

import (
	"testing"

	rfcommon "github.com/stmcginnis/gofish/common"
	redfish "github.com/stmcginnis/gofish/redfish"
	"github.com/stretchr/testify/assert"
)

func TestPowerSupplySummary(t *testing.T) {
	// Helper: build a sensor
	makeSensor := func(odataID, id, name string, reading float32, rmax float64, rmin float32, rtype, runits string,
		lc, lcr, uc, ucr float32) *redfish.Sensor {
		return &redfish.Sensor{
			Entity: rfcommon.Entity{
				ODataID: odataID,
				ID:      id,
				Name:    name,
			},
			Reading:         reading,
			ReadingRangeMax: rmax,
			ReadingRangeMin: rmin,
			ReadingType:     redfish.ReadingType(rtype),
			ReadingUnits:    runits,
			Thresholds: redfish.Thresholds{
				LowerCaution:  redfish.Threshold{Reading: lc},
				LowerCritical: redfish.Threshold{Reading: lcr},
				UpperCaution:  redfish.Threshold{Reading: uc},
				UpperCritical: redfish.Threshold{Reading: ucr},
			},
		}
	}

	testCases := map[string]struct {
		in       *PowerSupply
		expected string
	}{
		"basic with one sensor": {
			in: &PowerSupply{
				Entity: rfcommon.Entity{
					ID:   "psu-1",
					Name: "PSU Name",
				},
				CapacityWatts:   "1200W",
				FirmwareVersion: "FW1.2.3",
				HardwareVersion: "HWX",
				Location: redfish.PartLocation{
					LocationOrdinalValue: 3,
					LocationType:         "Rack",
					ServiceLabel:         "U42",
				},
				HotPluggable:   true,
				InputSourceNum: 2,
				Manufacturer:   "PSUCo",
				Model:          "PSUModel",
				PowerState:     true,
				SerialNumber:   "PSUSN",
				Sensors: []*redfish.Sensor{
					makeSensor(
						"/redfish/v1/Chassis/1/Sensors/Temp1",
						"s1",
						"Temp",
						42.0,
						100.0,
						0.0,
						"Temperature",
						"C",
						5.0, 2.0, 80.0, 90.0,
					),
				},
				Status: rfcommon.Status{
					State:  "Enabled",
					Health: "OK",
				},
			},
			expected: "" +
				"PSU Name Summary:\n" +
				"  ID: psu-1\n" +
				"  Name: PSU Name\n" +
				"  CapacityWatts: 1200W\n" +
				"  FirmwareVersion: FW1.2.3\n" +
				"  HardwareVersion: HWX\n" +
				"  HotPluggable: true\n" +
				"  InputSourceNum: 2\n" +
				"  Manufacturer: PSUCo\n" +
				"  Model: PSUModel\n" +
				"  PowerState: true\n" +
				"  SerialNumber: PSUSN\n" +
				"  Status: Enabled\n" +
				"  Health: OK\n" +
				"  Location:\n" +
				"    LocationOrdinalValue: 3\n" +
				"    LocationType: Rack\n" +
				"    ServiceLabel: U42\n" +
				"  Sensors:\n" +
				"    @odataid: /redfish/v1/Chassis/1/Sensors/Temp1\n" +
				"    Id: s1\n" +
				"    Name: Temp\n" +
				"    Reading: 42.000000\n" +
				"    ReadingRangeMax: 100.000000\n" +
				"    ReadingRangeMin: 0.000000\n" +
				"    ReadingType: Temperature\n" +
				"    ReadingUnits: C\n" +
				"    Thresholds:\n" +
				"        LowerCaution: 5.000000\n" +
				"        LowerCritical: 2.000000\n" +
				"        UpperCaution: 80.000000\n" +
				"        UpperCritical: 90.000000\n",
		},
		"no sensors still prints section header": {
			in: &PowerSupply{
				Entity: rfcommon.Entity{
					ID:   "psu-2",
					Name: "Empty Sensors PSU",
				},
				CapacityWatts:   "",
				FirmwareVersion: "",
				HardwareVersion: "",
				Location: redfish.PartLocation{
					LocationOrdinalValue: 0,
					LocationType:         "",
					ServiceLabel:         "",
				},
				HotPluggable:   false,
				InputSourceNum: 0,
				Manufacturer:   "",
				Model:          "",
				PowerState:     false,
				SerialNumber:   "",
				Sensors:        nil,
				Status: rfcommon.Status{
					State:  "",
					Health: "",
				},
			},
			expected: "" +
				"Empty Sensors PSU Summary:\n" +
				"  ID: psu-2\n" +
				"  Name: Empty Sensors PSU\n" +
				"  CapacityWatts: \n" +
				"  FirmwareVersion: \n" +
				"  HardwareVersion: \n" +
				"  HotPluggable: false\n" +
				"  InputSourceNum: 0\n" +
				"  Manufacturer: \n" +
				"  Model: \n" +
				"  PowerState: false\n" +
				"  SerialNumber: \n" +
				"  Status: \n" +
				"  Health: \n" +
				"  Location:\n" +
				"    LocationOrdinalValue: 0\n" +
				"    LocationType: \n" +
				"    ServiceLabel: \n" +
				"  Sensors:\n",
		},
		"multiple sensors": {
			in: &PowerSupply{
				Entity: rfcommon.Entity{
					ID:   "psu-3",
					Name: "Multi Sensors PSU",
				},
				CapacityWatts:   "800W",
				FirmwareVersion: "FW9.9.9",
				HardwareVersion: "HWZ",
				Location: redfish.PartLocation{
					LocationOrdinalValue: 1,
					LocationType:         "Bay",
					ServiceLabel:         "U1",
				},
				HotPluggable:   false,
				InputSourceNum: 1,
				Manufacturer:   "ACME",
				Model:          "ACME-800",
				PowerState:     true,
				SerialNumber:   "SN-123",
				Sensors: []*redfish.Sensor{
					makeSensor("/redfish/.../Temp1", "t1", "Temp1", 50.0, 120.0, 0.0, "Temperature", "C", 10.0, 5.0, 90.0, 95.0),
					makeSensor("/redfish/.../Volt1", "v1", "Volt1", 12.0, 15.0, 10.0, "Voltage", "V", 11.0, 10.5, 14.0, 14.5),
				},
				Status: rfcommon.Status{
					State:  "Enabled",
					Health: "OK",
				},
			},
			expected: "" +
				"Multi Sensors PSU Summary:\n" +
				"  ID: psu-3\n" +
				"  Name: Multi Sensors PSU\n" +
				"  CapacityWatts: 800W\n" +
				"  FirmwareVersion: FW9.9.9\n" +
				"  HardwareVersion: HWZ\n" +
				"  HotPluggable: false\n" +
				"  InputSourceNum: 1\n" +
				"  Manufacturer: ACME\n" +
				"  Model: ACME-800\n" +
				"  PowerState: true\n" +
				"  SerialNumber: SN-123\n" +
				"  Status: Enabled\n" +
				"  Health: OK\n" +
				"  Location:\n" +
				"    LocationOrdinalValue: 1\n" +
				"    LocationType: Bay\n" +
				"    ServiceLabel: U1\n" +
				"  Sensors:\n" +
				"    @odataid: /redfish/.../Temp1\n" +
				"    Id: t1\n" +
				"    Name: Temp1\n" +
				"    Reading: 50.000000\n" +
				"    ReadingRangeMax: 120.000000\n" +
				"    ReadingRangeMin: 0.000000\n" +
				"    ReadingType: Temperature\n" +
				"    ReadingUnits: C\n" +
				"    Thresholds:\n" +
				"        LowerCaution: 10.000000\n" +
				"        LowerCritical: 5.000000\n" +
				"        UpperCaution: 90.000000\n" +
				"        UpperCritical: 95.000000\n" +
				"    @odataid: /redfish/.../Volt1\n" +
				"    Id: v1\n" +
				"    Name: Volt1\n" +
				"    Reading: 12.000000\n" +
				"    ReadingRangeMax: 15.000000\n" +
				"    ReadingRangeMin: 10.000000\n" +
				"    ReadingType: Voltage\n" +
				"    ReadingUnits: V\n" +
				"    Thresholds:\n" +
				"        LowerCaution: 11.000000\n" +
				"        LowerCritical: 10.500000\n" +
				"        UpperCaution: 14.000000\n" +
				"        UpperCritical: 14.500000\n",
		},
	}

	for name, tc := range testCases {
		t.Run(name, func(t *testing.T) {
			out := tc.in.Summary()
			assert.Equal(t, tc.expected, out)
		})
	}
}
