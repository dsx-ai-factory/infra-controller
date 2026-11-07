// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package pmc

import (
	"net"
	"testing"

	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/credential"
	"github.com/NVIDIA/infra-controller/rest-api/powershelf-manager/pkg/common/vendor"

	"github.com/stretchr/testify/assert"
)

func newCred(u, p string) *credential.Credential {
	c := credential.New(u, p)
	return &c
}

func TestNew(t *testing.T) {
	testCases := map[string]struct {
		mac        string
		ip         string
		vcode      vendor.VendorCode
		cred       *credential.Credential
		expectErr  bool
		wantMacStr string
		wantIPStr  string
		wantVCode  vendor.VendorCode
	}{
		"valid liteon, 12-char mac normalization": {
			mac:        "001122334455",
			ip:         "192.168.1.100",
			vcode:      vendor.VendorCodeLiteon,
			cred:       newCred("admin", "password"),
			expectErr:  false,
			wantMacStr: "00:11:22:33:44:55",
			wantIPStr:  "192.168.1.100",
			wantVCode:  vendor.VendorCodeLiteon,
		},
		"valid liteon, colon-separated mac": {
			mac:        "00:11:22:33:44:55",
			ip:         "10.0.0.5",
			vcode:      vendor.VendorCodeLiteon,
			cred:       newCred("user", "pass"),
			expectErr:  false,
			wantMacStr: "00:11:22:33:44:55",
			wantIPStr:  "10.0.0.5",
			wantVCode:  vendor.VendorCodeLiteon,
		},
		"invalid mac string": {
			mac:       "invalid-mac",
			ip:        "192.168.1.100",
			vcode:     vendor.VendorCodeLiteon,
			cred:      newCred("admin", "password"),
			expectErr: true,
		},
		"invalid ip string": {
			mac:       "00:11:22:33:44:55",
			ip:        "not-an-ip",
			vcode:     vendor.VendorCodeLiteon,
			cred:      newCred("admin", "password"),
			expectErr: true,
		},
		"unsupported vendor code (Unsupported)": {
			mac:       "00:11:22:33:44:55",
			ip:        "192.168.1.100",
			vcode:     vendor.VendorCodeUnsupported,
			cred:      newCred("admin", "password"),
			expectErr: true,
		},
		"unsupported vendor code (Max sentinel)": {
			mac:       "00:11:22:33:44:55",
			ip:        "192.168.1.100",
			vcode:     vendor.VendorCodeMax,
			cred:      newCred("admin", "password"),
			expectErr: true,
		},
	}

	for name, tc := range testCases {
		t.Run(name, func(t *testing.T) {
			obj, err := New(tc.mac, tc.ip, tc.vcode, tc.cred)
			if tc.expectErr {
				assert.Error(t, err)
				assert.Nil(t, obj)
				return
			}
			assert.NoError(t, err)
			assert.NotNil(t, obj)

			// MAC and IP strings
			assert.Equal(t, tc.wantMacStr, obj.GetMac().String())
			assert.Equal(t, tc.wantIPStr, obj.GetIp().String())

			// Vendor code
			assert.Equal(t, tc.wantVCode, obj.GetVendor().Code)

			// Credential is copied (not the same pointer), but values equal
			if tc.cred != nil {
				assert.NotNil(t, obj.GetCredential())
				assert.NotSame(t, tc.cred, obj.GetCredential())
				assert.Equal(t, tc.cred.User, obj.GetCredential().User)
				assert.Equal(t, tc.cred.Password.Value, obj.GetCredential().Password.Value)
			}
		})
	}
}

func TestSetCredential(t *testing.T) {
	testCases := map[string]struct {
		startPMC *PMC
		newCred  *credential.Credential
		wantSet  bool
		wantUser string
		wantPass string
	}{
		"set new credential on empty PMC": {
			startPMC: &PMC{},
			newCred:  newCred("admin", "secret"),
			wantSet:  true,
			wantUser: "admin",
			wantPass: "secret",
		},
		"replace existing credential": {
			startPMC: func() *PMC {
				p, _ := New("00:11:22:33:44:55", "192.168.1.10", vendor.VendorCodeLiteon, newCred("old", "oldpass"))
				return p
			}(),
			newCred:  newCred("new", "newpass"),
			wantSet:  true,
			wantUser: "new",
			wantPass: "newpass",
		},
		"nil credential does nothing": {
			startPMC: func() *PMC {
				p, _ := New("00:11:22:33:44:55", "192.168.1.10", vendor.VendorCodeLiteon, newCred("old", "oldpass"))
				return p
			}(),
			newCred: nil,
			wantSet: false,
		},
	}

	for name, tc := range testCases {
		t.Run(name, func(t *testing.T) {
			tc.startPMC.SetCredential(tc.newCred)
			if tc.wantSet {
				assert.NotNil(t, tc.startPMC.Credential)
				assert.Equal(t, tc.wantUser, tc.startPMC.Credential.User)
				assert.Equal(t, tc.wantPass, tc.startPMC.Credential.Password.Value)
				// Copy semantics: ensure pointer differs from the source (if provided)
				if tc.newCred != nil {
					assert.NotSame(t, tc.newCred, tc.startPMC.Credential)
				}
			} else {
				// unchanged
				assert.NotNil(t, tc.startPMC.Credential)
				assert.Equal(t, "old", tc.startPMC.Credential.User)
				assert.Equal(t, "oldpass", tc.startPMC.Credential.Password.Value)
			}
		})
	}
}

func TestSetIP(t *testing.T) {
	testCases := map[string]struct {
		startIP string
		setTo   string
		wantIP  string
		wantNil bool
	}{
		"set valid ip": {
			startIP: "192.168.1.10",
			setTo:   "10.0.0.5",
			wantIP:  "10.0.0.5",
		},
		"set invalid ip -> nil": {
			startIP: "192.168.1.10",
			setTo:   "not-an-ip",
			wantNil: true,
		},
	}

	for name, tc := range testCases {
		t.Run(name, func(t *testing.T) {
			p, err := New("00:11:22:33:44:55", tc.startIP, vendor.VendorCodeLiteon, newCred("u", "p"))
			assert.NoError(t, err)
			p.SetIP(tc.setTo)
			if tc.wantNil {
				assert.Nil(t, p.IP)
			} else {
				assert.Equal(t, tc.wantIP, p.IP.String())
			}
		})
	}
}

func TestSetVendor(t *testing.T) {
	testCases := map[string]struct {
		startCode vendor.VendorCode
		setTo     vendor.VendorCode
		expectErr bool
		wantCode  vendor.VendorCode
	}{
		"set to supported (Liteon)": {
			startCode: vendor.VendorCodeLiteon,
			setTo:     vendor.VendorCodeLiteon,
			expectErr: false,
			wantCode:  vendor.VendorCodeLiteon,
		},
		"set to unsupported (Unsupported)": {
			startCode: vendor.VendorCodeLiteon,
			setTo:     vendor.VendorCodeUnsupported,
			expectErr: true,
			wantCode:  vendor.VendorCodeLiteon, // unchanged
		},
		"set to unsupported (Max)": {
			startCode: vendor.VendorCodeLiteon,
			setTo:     vendor.VendorCodeMax,
			expectErr: true,
			wantCode:  vendor.VendorCodeLiteon, // unchanged
		},
	}

	for name, tc := range testCases {
		t.Run(name, func(t *testing.T) {
			p, err := New("00:11:22:33:44:55", "192.168.1.10", tc.startCode, newCred("u", "p"))
			assert.NoError(t, err)
			err = p.SetVendor(tc.setTo)
			if tc.expectErr {
				assert.Error(t, err)
				assert.Equal(t, tc.wantCode, p.Vendor.Code)
			} else {
				assert.NoError(t, err)
				assert.Equal(t, tc.wantCode, p.Vendor.Code)
			}
		})
	}
}

func TestPatch(t *testing.T) {
	credPtr := func(u, p string) *credential.Credential { return newCred(u, p) }

	// Helper to create PMC quickly
	makePMC := func(mac, ip string, v vendor.VendorCode, cred *credential.Credential) PMC {
		obj, err := New(mac, ip, v, cred)
		assert.NoError(t, err)
		return *obj
	}

	testCases := map[string]struct {
		base        PMC
		to          PMC
		wantPatched bool
		wantIP      string
		wantUser    string
		wantPass    string
	}{
		"no patch if MAC different": {
			base:        makePMC("00:11:22:33:44:55", "192.168.1.10", vendor.VendorCodeLiteon, credPtr("admin", "a")),
			to:          makePMC("66:77:88:99:00:11", "192.168.1.100", vendor.VendorCodeLiteon, credPtr("root", "r")),
			wantPatched: false,
			wantIP:      "192.168.1.10",
			wantUser:    "admin",
			wantPass:    "a",
		},
		"patch IP only when MAC matches": {
			base:        makePMC("00:11:22:33:44:55", "192.168.1.10", vendor.VendorCodeLiteon, credPtr("admin", "a")),
			to:          makePMC("00:11:22:33:44:55", "192.168.1.100", vendor.VendorCodeLiteon, credPtr("admin", "a")),
			wantPatched: true,
			wantIP:      "192.168.1.100",
			wantUser:    "admin",
			wantPass:    "a",
		},
		"patch credential only when MAC matches": {
			base:        makePMC("00:11:22:33:44:55", "192.168.1.10", vendor.VendorCodeLiteon, credPtr("admin", "a")),
			to:          makePMC("00:11:22:33:44:55", "192.168.1.10", vendor.VendorCodeLiteon, credPtr("root", "r")),
			wantPatched: true,
			wantIP:      "192.168.1.10",
			wantUser:    "root",
			wantPass:    "r",
		},
		"patch both IP and credential when MAC matches": {
			base:        makePMC("00:11:22:33:44:55", "192.168.1.10", vendor.VendorCodeLiteon, credPtr("admin", "a")),
			to:          makePMC("00:11:22:33:44:55", "10.0.0.5", vendor.VendorCodeLiteon, credPtr("root", "r")),
			wantPatched: true,
			wantIP:      "10.0.0.5",
			wantUser:    "root",
			wantPass:    "r",
		},
		"no patch when MAC matches but IP equal and credential equal": {
			base:        makePMC("00:11:22:33:44:55", "192.168.1.10", vendor.VendorCodeLiteon, credPtr("admin", "a")),
			to:          makePMC("00:11:22:33:44:55", "192.168.1.10", vendor.VendorCodeLiteon, credPtr("admin", "a")),
			wantPatched: false,
			wantIP:      "192.168.1.10",
			wantUser:    "admin",
			wantPass:    "a",
		},
		"no IP patch when to.IP is nil; credential patch still possible": {
			base: makePMC("00:11:22:33:44:55", "192.168.1.10", vendor.VendorCodeLiteon, credPtr("admin", "a")),
			to: PMC{ // construct with same MAC, nil IP, and different cred
				MAC:        net.HardwareAddr{0x00, 0x11, 0x22, 0x33, 0x44, 0x55},
				IP:         nil,
				Vendor:     vendor.CodeToVendor(vendor.VendorCodeLiteon),
				Credential: credPtr("root", "r"),
			},
			wantPatched: true,
			wantIP:      "192.168.1.10",
			wantUser:    "root",
			wantPass:    "r",
		},
		"no credential patch when to.Credential is nil; IP patch can still apply": {
			base: makePMC("00:11:22:33:44:55", "192.168.1.10", vendor.VendorCodeLiteon, credPtr("admin", "a")),
			to: PMC{
				MAC:        net.HardwareAddr{0x00, 0x11, 0x22, 0x33, 0x44, 0x55},
				IP:         net.ParseIP("192.168.1.100"),
				Vendor:     vendor.CodeToVendor(vendor.VendorCodeLiteon),
				Credential: nil,
			},
			wantPatched: true,
			wantIP:      "192.168.1.100",
			wantUser:    "admin",
			wantPass:    "a",
		},
		"credential patch ignores empty fields": {
			base: makePMC("00:11:22:33:44:55", "192.168.1.10", vendor.VendorCodeLiteon, credPtr("admin", "a")),
			to: PMC{
				MAC:    net.HardwareAddr{0x00, 0x11, 0x22, 0x33, 0x44, 0x55},
				IP:     net.ParseIP("192.168.1.10"), // same IP
				Vendor: vendor.CodeToVendor(vendor.VendorCodeLiteon),
				Credential: &credential.Credential{ // empty user and empty password => no change
					User:     "",
					Password: newCred("", "").Password,
				},
			},
			wantPatched: false,
			wantIP:      "192.168.1.10",
			wantUser:    "admin",
			wantPass:    "a",
		},
	}

	for name, tc := range testCases {
		t.Run(name, func(t *testing.T) {
			base := tc.base
			patched := base.Patch(tc.to)
			assert.Equal(t, tc.wantPatched, patched)
			assert.Equal(t, tc.wantIP, base.IP.String())
			if base.Credential != nil {
				assert.Equal(t, tc.wantUser, base.Credential.User)
				assert.Equal(t, tc.wantPass, base.Credential.Password.Value)
			} else {
				assert.Nil(t, tc.wantUser)
				assert.Nil(t, tc.wantPass)
			}
		})
	}
}
