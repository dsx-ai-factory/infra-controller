// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package protobuf

import (
	"testing"

	pb "github.com/NVIDIA/infra-controller/rest-api/powershelf-manager/internal/proto/v1"
	"github.com/NVIDIA/infra-controller/rest-api/powershelf-manager/pkg/common/vendor"
	"github.com/NVIDIA/infra-controller/rest-api/powershelf-manager/pkg/objects/pmc"
	"github.com/NVIDIA/infra-controller/rest-api/powershelf-manager/pkg/objects/powershelf"
	"github.com/NVIDIA/infra-controller/rest-api/powershelf-manager/pkg/objects/powersupply"

	rfcommon "github.com/stmcginnis/gofish/common"
	gofish "github.com/stmcginnis/gofish/redfish"
)

// pickValidVendor returns a supported vendor code; hard-fails if not accepted.
func pickValidVendor(t *testing.T) vendor.VendorCode {
	t.Helper()
	code := vendor.VendorCodeLiteon
	if obj, err := pmc.New("00:11:22:33:44:55", "192.168.1.10", code, nil); err == nil && obj != nil {
		return code
	}
	t.Fatalf("pmc.New did not accept vendor code %v", code)
	return vendor.VendorCodeUnsupported
}

func TestPMCVendorFrom(t *testing.T) {
	testCases := map[string]struct {
		in       pb.PMCVendor
		expected vendor.VendorCode
	}{
		"unknown -> unsupported": {
			in:       pb.PMCVendor_PMC_TYPE_UNKNOWN,
			expected: vendor.VendorCodeUnsupported,
		},
		"liteon -> liteon": {
			in:       pb.PMCVendor_PMC_TYPE_LITEON,
			expected: vendor.VendorCodeLiteon,
		},
		"delta -> delta": {
			in:       pb.PMCVendor_PMC_TYPE_DELTA,
			expected: vendor.VendorCodeDelta,
		},
		"unmapped enum -> unsupported": {
			in:       pb.PMCVendor(999),
			expected: vendor.VendorCodeUnsupported,
		},
	}

	for name, tc := range testCases {
		t.Run(name, func(t *testing.T) {
			got := PMCVendorFrom(tc.in)
			if got != tc.expected {
				t.Errorf("PMCVendorFrom(%v) = %v; want %v", tc.in, got, tc.expected)
			}
		})
	}
}

func TestVendorCodeTo(t *testing.T) {
	testCases := map[string]struct {
		in       vendor.VendorCode
		expected pb.PMCVendor
	}{
		"unsupported -> unknown": {
			in:       vendor.VendorCodeUnsupported,
			expected: pb.PMCVendor_PMC_TYPE_UNKNOWN,
		},
		"liteon -> liteon": {
			in:       vendor.VendorCodeLiteon,
			expected: pb.PMCVendor_PMC_TYPE_LITEON,
		},
		"delta -> delta": {
			in:       vendor.VendorCodeDelta,
			expected: pb.PMCVendor_PMC_TYPE_DELTA,
		},
		"unmapped code -> unknown": {
			in:       vendor.VendorCodeMax,
			expected: pb.PMCVendor_PMC_TYPE_UNKNOWN,
		},
	}

	for name, tc := range testCases {
		t.Run(name, func(t *testing.T) {
			got := VendorCodeTo(tc.in)
			if got != tc.expected {
				t.Errorf("VendorCodeTo(%v) = %v; want %v", tc.in, got, tc.expected)
			}
		})
	}
}

func TestPMCTo(t *testing.T) {
	validVendor := pickValidVendor(t)

	testCases := map[string]struct {
		in       *pmc.PMC
		wantMac  string
		wantIP   string
		wantVend pb.PMCVendor
	}{
		"nil input": {
			in: nil,
		},
		"valid pmc": {
			in: func() *pmc.PMC {
				obj, err := pmc.New("00:11:22:33:44:55", "192.168.1.10", validVendor, nil)
				if err != nil {
					t.Fatalf("pmc.New error: %v", err)
				}
				return obj
			}(),
			wantMac:  "00:11:22:33:44:55",
			wantIP:   "192.168.1.10",
			wantVend: pb.PMCVendor_PMC_TYPE_LITEON,
		},
	}

	for name, tc := range testCases {
		t.Run(name, func(t *testing.T) {
			got := PMCTo(tc.in)
			if tc.in == nil {
				if got != nil {
					t.Fatalf("expected nil, got %#v", got)
				}
				return
			}
			if got == nil {
				t.Fatalf("expected non-nil")
			}
			if got.MacAddress != tc.wantMac {
				t.Errorf("MacAddress = %q; want %q", got.MacAddress, tc.wantMac)
			}
			if got.IpAddress != tc.wantIP {
				t.Errorf("IpAddress = %q; want %q", got.IpAddress, tc.wantIP)
			}
			if got.Vendor != tc.wantVend {
				t.Errorf("Vendor = %v; want %v", got.Vendor, tc.wantVend)
			}
		})
	}
}

func TestChassisTo(t *testing.T) {
	testCases := map[string]struct {
		in             *gofish.Chassis
		wantSN         string
		wantModel      string
		wantMfgFromMod string
	}{
		"nil input": {
			in: nil,
		},
		"valid chassis": {
			in: &gofish.Chassis{
				SerialNumber: "SN123",
				Model:        "ModelX",
				Manufacturer: "MfgCo",
			},
			wantSN:         "SN123",
			wantModel:      "ModelX",
			wantMfgFromMod: "ModelX", // implementation sets Manufacturer from Model
		},
	}

	for name, tc := range testCases {
		t.Run(name, func(t *testing.T) {
			got := ChassisTo(tc.in)
			if tc.in == nil {
				if got != nil {
					t.Fatalf("expected nil, got %#v", got)
				}
				return
			}
			if got.SerialNumber != tc.wantSN {
				t.Errorf("SerialNumber = %q; want %q", got.SerialNumber, tc.wantSN)
			}
			if got.Model != tc.wantModel {
				t.Errorf("Model = %q; want %q", got.Model, tc.wantModel)
			}
			if got.Manufacturer != tc.wantMfgFromMod {
				t.Errorf("Manufacturer = %q; want %q", got.Manufacturer, tc.wantMfgFromMod)
			}
		})
	}
}

func TestThresholdTo(t *testing.T) {
	testCases := map[string]struct {
		in       *gofish.Threshold
		wantRead float32
	}{
		"nil input": {
			in: nil,
		},
		"valid threshold": {
			in:       &gofish.Threshold{Reading: 42.5},
			wantRead: 42.5,
		},
	}

	for name, tc := range testCases {
		t.Run(name, func(t *testing.T) {
			got := ThresholdTo(tc.in)
			if tc.in == nil {
				if got != nil {
					t.Fatalf("expected nil, got %#v", got)
				}
				return
			}
			if got.Reading != tc.wantRead {
				t.Errorf("Reading = %v; want %v", got.Reading, tc.wantRead)
			}
		})
	}
}

func TestThresholdsTo(t *testing.T) {
	testCases := map[string]struct {
		in              *gofish.Thresholds
		wantLC, wantLCR float32
		wantUC, wantUCR float32
	}{
		"nil input": {
			in: nil,
		},
		"valid thresholds": {
			in: &gofish.Thresholds{
				LowerCaution:  gofish.Threshold{Reading: 1.0},
				LowerCritical: gofish.Threshold{Reading: 2.0},
				UpperCaution:  gofish.Threshold{Reading: 3.0},
				UpperCritical: gofish.Threshold{Reading: 4.0},
			},
			wantLC:  1.0,
			wantLCR: 2.0,
			wantUC:  3.0,
			wantUCR: 4.0,
		},
	}

	for name, tc := range testCases {
		t.Run(name, func(t *testing.T) {
			got := ThresholdsTo(tc.in)
			if tc.in == nil {
				if got != nil {
					t.Fatalf("expected nil, got %#v", got)
				}
				return
			}
			if got.LowerCaution.Reading != tc.wantLC {
				t.Errorf("LowerCaution.Reading = %v; want %v", got.LowerCaution.Reading, tc.wantLC)
			}
			if got.LowerCritical.Reading != tc.wantLCR {
				t.Errorf("LowerCritical.Reading = %v; want %v", got.LowerCritical.Reading, tc.wantLCR)
			}
			if got.UpperCaution.Reading != tc.wantUC {
				t.Errorf("UpperCaution.Reading = %v; want %v", got.UpperCaution.Reading, tc.wantUC)
			}
			if got.UpperCritical.Reading != tc.wantUCR {
				t.Errorf("UpperCritical.Reading = %v; want %v", got.UpperCritical.Reading, tc.wantUCR)
			}
		})
	}
}

func TestSensorTo(t *testing.T) {
	testCases := map[string]struct {
		in        *gofish.Sensor
		wantID    string
		wantName  string
		wantRead  float32
		wantType  string
		wantUnits string
	}{
		"nil input": {
			in: nil,
		},
		"valid sensor": {
			in: &gofish.Sensor{
				// Set embedded common.Entity fields via the field name "Entity"
				Entity: rfcommon.Entity{
					ID:   "sensor-1",
					Name: "Temp",
				},
				Reading:         55.0,
				ReadingRangeMax: 100.0,
				ReadingRangeMin: 0.0,
				ReadingType:     gofish.ReadingType("Temperature"),
				ReadingUnits:    "C",
				Thresholds: gofish.Thresholds{
					LowerCaution:  gofish.Threshold{Reading: 10.0},
					LowerCritical: gofish.Threshold{Reading: 5.0},
					UpperCaution:  gofish.Threshold{Reading: 80.0},
					UpperCritical: gofish.Threshold{Reading: 90.0},
				},
			},
			wantID:    "sensor-1",
			wantName:  "Temp",
			wantRead:  55.0,
			wantType:  "Temperature",
			wantUnits: "C",
		},
	}

	for name, tc := range testCases {
		t.Run(name, func(t *testing.T) {
			got := SensorTo(tc.in)
			if tc.in == nil {
				if got != nil {
					t.Fatalf("expected nil, got %#v", got)
				}
				return
			}
			if got.Id != tc.wantID {
				t.Errorf("Id = %q; want %q", got.Id, tc.wantID)
			}
			if got.Name != tc.wantName {
				t.Errorf("Name = %q; want %q", got.Name, tc.wantName)
			}
			if got.Reading != tc.wantRead {
				t.Errorf("Reading = %v; want %v", got.Reading, tc.wantRead)
			}
			if got.ReadingType != tc.wantType {
				t.Errorf("ReadingType = %q; want %q", got.ReadingType, tc.wantType)
			}
			if got.ReadingUnits != tc.wantUnits {
				t.Errorf("ReadingUnits = %q; want %q", got.ReadingUnits, tc.wantUnits)
			}
			if got.Thresholds == nil {
				t.Fatalf("Thresholds is nil")
			}
			if got.Thresholds.UpperCritical.Reading != 90.0 {
				t.Errorf("UpperCritical.Reading = %v; want %v", got.Thresholds.UpperCritical.Reading, 90.0)
			}
		})
	}
}

func TestPSUTo(t *testing.T) {
	testCases := map[string]struct {
		in         *powersupply.PowerSupply
		wantID     string
		wantModel  string
		wantSensor int
	}{
		"nil input": {
			in: nil,
		},
		"valid psu": {
			in: &powersupply.PowerSupply{
				// Set embedded common.Entity via field name "Entity"
				Entity: rfcommon.Entity{
					ID:   "psu-1",
					Name: "PSU Name",
				},
				CapacityWatts:   "1200W",
				FirmwareVersion: "FW1.2.3",
				HardwareVersion: "HWX",
				Manufacturer:    "PSUCo",
				Model:           "PSUModel",
				PowerState:      true,
				Sensors: []*gofish.Sensor{
					{
						Entity: rfcommon.Entity{
							ID:   "s1",
							Name: "Temp",
						},
						Reading:         42.0,
						ReadingRangeMax: 100.0,
						ReadingRangeMin: 0.0,
						ReadingType:     gofish.ReadingType("Temperature"),
						ReadingUnits:    "C",
						Thresholds: gofish.Thresholds{
							LowerCaution:  gofish.Threshold{Reading: 5.0},
							LowerCritical: gofish.Threshold{Reading: 2.0},
							UpperCaution:  gofish.Threshold{Reading: 80.0},
							UpperCritical: gofish.Threshold{Reading: 90.0},
						},
					},
				},
				SerialNumber: "PSUSN",
			},
			wantID:     "psu-1",
			wantModel:  "PSUModel",
			wantSensor: 1,
		},
	}

	for name, tc := range testCases {
		t.Run(name, func(t *testing.T) {
			got := PSUTo(tc.in)
			if tc.in == nil {
				if got != nil {
					t.Fatalf("expected nil, got %#v", got)
				}
				return
			}
			if got.Id != tc.wantID {
				t.Errorf("Id = %q; want %q", got.Id, tc.wantID)
			}
			if got.Model != tc.wantModel {
				t.Errorf("Model = %q; want %q", got.Model, tc.wantModel)
			}
			if len(got.Sensors) != tc.wantSensor {
				t.Fatalf("Sensors len = %d; want %d", len(got.Sensors), tc.wantSensor)
			}
			if got.Sensors[0].Id != "s1" {
				t.Errorf("First sensor Id = %q; want %q", got.Sensors[0].Id, "s1")
			}
		})
	}
}

func TestPowershelfTo(t *testing.T) {
	validVendor := pickValidVendor(t)

	testCases := map[string]struct {
		in            *powershelf.PowerShelf
		wantVendor    pb.PMCVendor
		wantPsusCount int
		wantChModel   string
		wantSerial    string
		wantModel     string
		wantMfg       string
		wantPart      string
		wantFW        string
	}{
		"nil input": {
			in: nil,
		},
		"valid powershelf": {
			in: func() *powershelf.PowerShelf {
				obj, err := pmc.New("00:11:22:33:44:55", "192.168.1.10", validVendor, nil)
				if err != nil {
					t.Fatalf("pmc.New error: %v", err)
				}
				return &powershelf.PowerShelf{
					PMC: obj,
					Manager: &gofish.Manager{
						SerialNumber:    "PMCSN",
						Model:           "PMCModel",
						Manufacturer:    "PMCMfg",
						PartNumber:      "PMCPART",
						FirmwareVersion: "PMCFW",
					},
					Chassis: &gofish.Chassis{
						SerialNumber: "CHSN",
						Model:        "CHModel",
						Manufacturer: "CHMfg",
					},
					PowerSupplies: []*powersupply.PowerSupply{
						{Entity: rfcommon.Entity{ID: "psu-1"}},
						{Entity: rfcommon.Entity{ID: "psu-2"}},
					},
				}
			}(),
			wantVendor:    pb.PMCVendor_PMC_TYPE_LITEON,
			wantPsusCount: 2,
			wantChModel:   "CHModel",
			wantSerial:    "PMCSN",
			wantModel:     "PMCModel",
			wantMfg:       "PMCMfg",
			wantPart:      "PMCPART",
			wantFW:        "PMCFW",
		},
	}

	for name, tc := range testCases {
		t.Run(name, func(t *testing.T) {
			got := PowershelfTo(tc.in)
			if tc.in == nil {
				if got != nil {
					t.Fatalf("expected nil, got %#v", got)
				}
				return
			}
			if got == nil || got.Pmc == nil || got.Chassis == nil {
				t.Fatalf("expected non-nil PowerShelf, Pmc, and Chassis, got %#v", got)
			}
			if got.Pmc.Vendor != tc.wantVendor {
				t.Errorf("pmc.Vendor = %v; want %v", got.Pmc.Vendor, tc.wantVendor)
			}
			if got.Pmc.SerialNumber != tc.wantSerial {
				t.Errorf("pmc.SerialNumber = %q; want %q", got.Pmc.SerialNumber, tc.wantSerial)
			}
			if got.Pmc.Model != tc.wantModel {
				t.Errorf("pmc.Model = %q; want %q", got.Pmc.Model, tc.wantModel)
			}
			if got.Pmc.Manufacturer != tc.wantMfg {
				t.Errorf("pmc.Manufacturer = %q; want %q", got.Pmc.Manufacturer, tc.wantMfg)
			}
			if got.Pmc.PartNumber != tc.wantPart {
				t.Errorf("pmc.PartNumber = %q; want %q", got.Pmc.PartNumber, tc.wantPart)
			}
			if got.Pmc.FirmwareVersion != tc.wantFW {
				t.Errorf("pmc.FirmwareVersion = %q; want %q", got.Pmc.FirmwareVersion, tc.wantFW)
			}
			if got.Chassis.Model != tc.wantChModel {
				t.Errorf("chassis.Model = %q; want %q", got.Chassis.Model, tc.wantChModel)
			}
			if len(got.Psus) != tc.wantPsusCount {
				t.Errorf("psus len = %d; want %d", len(got.Psus), tc.wantPsusCount)
			}
		})
	}
}

func TestPMCFrom(t *testing.T) {
	validVendor := pickValidVendor(t)

	testCases := map[string]struct {
		inPmc     *pb.PowerManagementController
		inCreds   *pb.Credentials
		expectErr bool
		expectNil bool
		wantMac   string
		wantIP    string
		wantVCode vendor.VendorCode
		wantUser  string
		wantPass  string
	}{
		"nil protobuf pmc": {
			inPmc:     nil,
			inCreds:   &pb.Credentials{Username: "user", Password: "pass"},
			expectErr: true,
			expectNil: true,
		},
		"valid liteon pmc": {
			inPmc: &pb.PowerManagementController{
				MacAddress: "00:11:22:33:44:55",
				IpAddress:  "192.168.1.10",
				Vendor:     pb.PMCVendor_PMC_TYPE_LITEON,
			},
			inCreds:   &pb.Credentials{Username: "admin", Password: "secret"},
			wantMac:   "00:11:22:33:44:55",
			wantIP:    "192.168.1.10",
			wantVCode: validVendor,
			wantUser:  "admin",
			wantPass:  "secret",
		},
		"invalid mac": {
			inPmc: &pb.PowerManagementController{
				MacAddress: "invalid-mac",
				IpAddress:  "192.168.1.10",
				Vendor:     pb.PMCVendor_PMC_TYPE_LITEON,
			},
			inCreds:   &pb.Credentials{Username: "u", Password: "p"},
			expectErr: true,
			expectNil: true,
		},
		"invalid ip": {
			inPmc: &pb.PowerManagementController{
				MacAddress: "00:11:22:33:44:55",
				IpAddress:  "not-an-ip",
				Vendor:     pb.PMCVendor_PMC_TYPE_LITEON,
			},
			inCreds:   &pb.Credentials{Username: "u", Password: "p"},
			expectErr: true,
			expectNil: true,
		},
		"unsupported vendor (unknown)": {
			inPmc: &pb.PowerManagementController{
				MacAddress: "00:11:22:33:44:55",
				IpAddress:  "192.168.1.10",
				Vendor:     pb.PMCVendor_PMC_TYPE_UNKNOWN,
			},
			inCreds:   &pb.Credentials{Username: "u", Password: "p"},
			expectErr: true,
			expectNil: true,
		},
	}

	for name, tc := range testCases {
		t.Run(name, func(t *testing.T) {
			out, err := PMCFrom(tc.inPmc, tc.inCreds)

			if tc.expectErr {
				if err == nil {
					t.Fatalf("expected error, got nil (out=%#v)", out)
				}
			} else if err != nil {
				t.Fatalf("unexpected error: %v", err)
			}

			if tc.expectNil {
				if out != nil {
					t.Fatalf("expected nil pmc, got %#v", out)
				}
				return
			}

			if out == nil {
				t.Fatalf("expected non-nil pmc")
			}
			if out.GetMac().String() != tc.wantMac {
				t.Errorf("MAC = %q; want %q", out.GetMac().String(), tc.wantMac)
			}
			if out.GetIp().String() != tc.wantIP {
				t.Errorf("IP = %q; want %q", out.GetIp().String(), tc.wantIP)
			}
			if out.GetVendor().Code != tc.wantVCode {
				t.Errorf("VendorCode = %v; want %v", out.GetVendor().Code, tc.wantVCode)
			}
			cred := out.GetCredential()
			if cred == nil {
				t.Fatalf("expected credential set")
			}
			if cred.User != tc.wantUser {
				t.Errorf("credential user = %q; want %q", cred.User, tc.wantUser)
			}
			if cred.Password.Value != tc.wantPass {
				t.Errorf("credential password = %q; want %q", cred.Password.Value, tc.wantPass)
			}
		})
	}
}
