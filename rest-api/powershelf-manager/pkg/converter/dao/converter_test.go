// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package dao

import (
	"net"
	"testing"

	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/sqltypes"
	"github.com/NVIDIA/infra-controller/rest-api/powershelf-manager/pkg/common/vendor"
	"github.com/NVIDIA/infra-controller/rest-api/powershelf-manager/pkg/db/model"
	"github.com/NVIDIA/infra-controller/rest-api/powershelf-manager/pkg/objects/pmc"
	"github.com/NVIDIA/infra-controller/rest-api/powershelf-manager/pkg/objects/powershelf"
)

// pickValidVendor returns a supported vendor code; hard-fails if none are accepted.
func pickValidVendor(t *testing.T) vendor.VendorCode {
	t.Helper()
	candidates := []vendor.VendorCode{
		vendor.VendorCodeLiteon,
	}
	mac := "00:11:22:33:44:55"
	ip := "192.168.1.10"

	for _, code := range candidates {
		if obj, err := pmc.New(mac, ip, code, nil); err == nil && obj != nil {
			return code
		}
	}
	t.Fatalf("pmc.New did not accept any known vendor codes from %v", candidates)
	return vendor.VendorCodeUnsupported
}

// Helper to parse MAC address and return model.MacAddr
func mustParseMAC(t *testing.T, s string) model.MacAddr {
	t.Helper()
	mac, err := net.ParseMAC(s)
	if err != nil {
		t.Fatalf("mustParseMAC(%q): %v", s, err)
	}
	return model.MacAddr(mac)
}

// Helper to parse IP address and return sqltypes.IPAddr
func mustParseIP(t *testing.T, s string) sqltypes.IPAddr {
	t.Helper()
	ip := net.ParseIP(s)
	if ip == nil {
		t.Fatalf("mustParseIP(%q): invalid IP", s)
	}
	return sqltypes.IPAddr(ip)
}

func TestPmcTo(t *testing.T) {
	validVendor := pickValidVendor(t)

	testCases := map[string]struct {
		create    bool
		mac       string
		ip        string
		vcode     vendor.VendorCode
		expectNil bool
	}{
		"nil input": {
			create:    false,
			expectNil: true,
		},
		"valid input": {
			create: true,
			mac:    "00:11:22:33:44:55",
			ip:     "192.168.1.10",
			vcode:  validVendor,
		},
	}

	for name, tc := range testCases {
		t.Run(name, func(t *testing.T) {
			if !tc.create {
				if got := PmcTo(nil); got != nil {
					t.Fatalf("PmcTo(nil) expected nil, got %#v", got)
				}
				return
			}

			obj, err := pmc.New(tc.mac, tc.ip, tc.vcode, nil)
			if err != nil {
				t.Fatalf("pmc.New returned error: %v", err)
			}

			daoObj := PmcTo(obj)
			if tc.expectNil {
				if daoObj != nil {
					t.Fatalf("expected nil, got %#v", daoObj)
				}
				return
			}

			if daoObj == nil {
				t.Fatalf("PmcTo returned nil for valid input")
			}
			if daoObj.MacAddress.String() != obj.GetMac().String() {
				t.Errorf("MacAddress mismatch: got %q, want %q", daoObj.MacAddress.String(), obj.GetMac().String())
			}
			if daoObj.IPAddress.String() != obj.GetIp().String() {
				t.Errorf("IPAddress mismatch: got %q, want %q", daoObj.IPAddress.String(), obj.GetIp().String())
			}
			if daoObj.Vendor != obj.GetVendor().Code {
				t.Errorf("Vendor mismatch: got %v, want %v", daoObj.Vendor, obj.GetVendor().Code)
			}
		})
	}
}

func TestPmcFrom(t *testing.T) {
	validVendor := pickValidVendor(t)

	testCases := map[string]struct {
		useNilInput bool
		mac         string
		ip          string
		vcode       vendor.VendorCode
		expectErr   bool
		expectNil   bool
	}{
		"nil input": {
			useNilInput: true,
			expectNil:   true,
		},
		"valid input": {
			mac:   "00:11:22:33:44:55",
			ip:    "192.168.1.10",
			vcode: validVendor,
		},
		"invalid vendor": {
			mac:       "00:11:22:33:44:55",
			ip:        "192.168.1.10",
			vcode:     vendor.VendorCodeMax, // outside valid range -> invalid
			expectErr: true,
			expectNil: true,
		},
	}

	for name, tc := range testCases {
		t.Run(name, func(t *testing.T) {
			if tc.useNilInput {
				got, err := PmcFrom(nil)
				if err != nil {
					t.Fatalf("PmcFrom(nil) unexpected error: %v", err)
				}
				if got != nil {
					t.Fatalf("PmcFrom(nil) expected nil, got %#v", got)
				}
				return
			}

			// For invalid vendor, ensure pmc.New would reject it; acceptance is a failure.
			if name == "invalid vendor" {
				if obj, err := pmc.New(tc.mac, tc.ip, tc.vcode, nil); err == nil && obj != nil {
					t.Fatalf("pmc.New unexpectedly accepted invalid vendor code %v", tc.vcode)
				}
			}

			in := &model.PMC{
				MacAddress: mustParseMAC(t, tc.mac),
				IPAddress:  mustParseIP(t, tc.ip),
				Vendor:     tc.vcode,
			}

			out, err := PmcFrom(in)

			if tc.expectErr {
				if err == nil {
					t.Fatalf("expected error, got nil (out=%#v)", out)
				}
			} else if err != nil {
				t.Fatalf("unexpected error: %v", err)
			}

			if tc.expectNil {
				if out != nil {
					t.Fatalf("expected nil PMC, got %#v", out)
				}
				return
			}

			if out == nil {
				t.Fatalf("expected non-nil PMC for valid input")
			}
			if out.GetMac().String() != tc.mac {
				t.Errorf("Mac mismatch: got %q, want %q", out.GetMac().String(), tc.mac)
			}
			if out.GetIp().String() != tc.ip {
				t.Errorf("IP mismatch: got %q, want %q", out.GetIp().String(), tc.ip)
			}
			if out.GetVendor().Code != tc.vcode {
				t.Errorf("Vendor code mismatch: got %v, want %v", out.GetVendor().Code, tc.vcode)
			}
		})
	}
}

// TestFirmwareUpdateTo_InvalidMAC verifies that FirmwareUpdateTo returns an error
// when the MAC address is invalid, preventing nil pointer dereferences downstream.
// This is a regression test for: FirmwareUpdateTo returning nil without error on invalid MAC.
func TestFirmwareUpdateTo_InvalidMAC(t *testing.T) {
	testCases := map[string]struct {
		update    *powershelf.FirmwareUpdate
		expectErr bool
		errMsg    string
	}{
		"nil input returns error": {
			update:    nil,
			expectErr: true,
			errMsg:    "cannot convert nil FirmwareUpdate",
		},
		"invalid MAC address returns error": {
			update: &powershelf.FirmwareUpdate{
				PmcMacAddress: "invalid-mac",
				Component:     powershelf.PMC,
				VersionFrom:   "1.0.0",
				VersionTo:     "2.0.0",
				State:         powershelf.FirmwareStateQueued,
			},
			expectErr: true,
			errMsg:    "invalid MAC address",
		},
		"empty MAC address returns error": {
			update: &powershelf.FirmwareUpdate{
				PmcMacAddress: "",
				Component:     powershelf.PMC,
				VersionFrom:   "1.0.0",
				VersionTo:     "2.0.0",
				State:         powershelf.FirmwareStateQueued,
			},
			expectErr: true,
			errMsg:    "invalid MAC address",
		},
		"malformed MAC address returns error": {
			update: &powershelf.FirmwareUpdate{
				PmcMacAddress: "00:11:22:33:44", // missing last octet
				Component:     powershelf.PMC,
				VersionFrom:   "1.0.0",
				VersionTo:     "2.0.0",
				State:         powershelf.FirmwareStateQueued,
			},
			expectErr: true,
			errMsg:    "invalid MAC address",
		},
		"valid MAC address succeeds": {
			update: &powershelf.FirmwareUpdate{
				PmcMacAddress: "00:11:22:33:44:55",
				Component:     powershelf.PMC,
				VersionFrom:   "1.0.0",
				VersionTo:     "2.0.0",
				State:         powershelf.FirmwareStateQueued,
			},
			expectErr: false,
		},
	}

	for name, tc := range testCases {
		t.Run(name, func(t *testing.T) {
			result, err := FirmwareUpdateTo(tc.update)

			if tc.expectErr {
				if err == nil {
					t.Fatalf("expected error containing %q, got nil (result=%#v)", tc.errMsg, result)
				}
				if result != nil {
					t.Fatalf("expected nil result on error, got %#v", result)
				}
				// Verify error message contains expected substring
				if tc.errMsg != "" && !containsString(err.Error(), tc.errMsg) {
					t.Errorf("error %q does not contain expected message %q", err.Error(), tc.errMsg)
				}
			} else {
				if err != nil {
					t.Fatalf("unexpected error: %v", err)
				}
				if result == nil {
					t.Fatalf("expected non-nil result for valid input")
				}
				// Verify the MAC was correctly converted
				if result.PmcMacAddress.String() != tc.update.PmcMacAddress {
					t.Errorf("MAC mismatch: got %q, want %q", result.PmcMacAddress.String(), tc.update.PmcMacAddress)
				}
			}
		})
	}
}

// TestFirmwareUpdateFrom verifies FirmwareUpdateFrom correctly converts model to domain.
func TestFirmwareUpdateFrom(t *testing.T) {
	testCases := map[string]struct {
		input     *model.FirmwareUpdate
		expectNil bool
	}{
		"nil input returns nil": {
			input:     nil,
			expectNil: true,
		},
		"valid input converts correctly": {
			input: &model.FirmwareUpdate{
				PmcMacAddress: mustParseMAC(t, "00:11:22:33:44:55"),
				Component:     powershelf.PMC,
				VersionFrom:   "1.0.0",
				VersionTo:     "2.0.0",
				State:         powershelf.FirmwareStateQueued,
				ErrorMessage:  "test error",
				JobID:         "job-123",
			},
			expectNil: false,
		},
	}

	for name, tc := range testCases {
		t.Run(name, func(t *testing.T) {
			result := FirmwareUpdateFrom(tc.input)

			if tc.expectNil {
				if result != nil {
					t.Fatalf("expected nil, got %#v", result)
				}
				return
			}

			if result == nil {
				t.Fatalf("expected non-nil result")
			}

			// Verify fields are correctly mapped
			if result.PmcMacAddress != tc.input.PmcMacAddress.String() {
				t.Errorf("PmcMacAddress mismatch: got %q, want %q", result.PmcMacAddress, tc.input.PmcMacAddress.String())
			}
			if result.Component != tc.input.Component {
				t.Errorf("Component mismatch: got %v, want %v", result.Component, tc.input.Component)
			}
			if result.VersionFrom != tc.input.VersionFrom {
				t.Errorf("VersionFrom mismatch: got %q, want %q", result.VersionFrom, tc.input.VersionFrom)
			}
			if result.VersionTo != tc.input.VersionTo {
				t.Errorf("VersionTo mismatch: got %q, want %q", result.VersionTo, tc.input.VersionTo)
			}
			if result.State != tc.input.State {
				t.Errorf("State mismatch: got %v, want %v", result.State, tc.input.State)
			}
			if result.ErrorMessage != tc.input.ErrorMessage {
				t.Errorf("ErrorMessage mismatch: got %q, want %q", result.ErrorMessage, tc.input.ErrorMessage)
			}
			if result.JobID != tc.input.JobID {
				t.Errorf("JobID mismatch: got %q, want %q", result.JobID, tc.input.JobID)
			}
		})
	}
}

// containsString checks if s contains substr (helper for error message checks)
func containsString(s, substr string) bool {
	return len(s) >= len(substr) && (s == substr || len(substr) == 0 ||
		(len(s) > 0 && len(substr) > 0 && findSubstring(s, substr)))
}

func findSubstring(s, substr string) bool {
	for i := 0; i <= len(s)-len(substr); i++ {
		if s[i:i+len(substr)] == substr {
			return true
		}
	}
	return false
}
