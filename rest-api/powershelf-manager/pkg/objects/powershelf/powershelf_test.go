// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package powershelf

import (
	"testing"

	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/credential"
	"github.com/NVIDIA/infra-controller/rest-api/powershelf-manager/pkg/common/vendor"
	"github.com/NVIDIA/infra-controller/rest-api/powershelf-manager/pkg/objects/pmc"
	"github.com/NVIDIA/infra-controller/rest-api/powershelf-manager/pkg/objects/powersupply"

	rfcommon "github.com/stmcginnis/gofish/common"
	gofish "github.com/stmcginnis/gofish/redfish"
	"github.com/stretchr/testify/assert"
)

func newCred(u, p string) *credential.Credential {
	c := credential.New(u, p)
	return &c
}

// pickValidVendor returns a supported vendor code; hard-fails if not accepted.
func pickValidVendor(t *testing.T) vendor.VendorCode {
	t.Helper()
	code := vendor.VendorCodeLiteon
	if obj, err := pmc.New("00:11:22:33:44:55", "192.168.1.10", code, newCred("u", "p")); err == nil && obj != nil {
		return code
	}
	t.Fatalf("pmc.New did not accept vendor code %v", code)
	return vendor.VendorCodeUnsupported
}

func TestPowerShelfConstruction(t *testing.T) {
	validVendor := pickValidVendor(t)

	// Helpers
	makePMC := func(mac, ip string, v vendor.VendorCode, user, pass string) *pmc.PMC {
		obj, err := pmc.New(mac, ip, v, newCred(user, pass))
		assert.NoError(t, err)
		return obj
	}
	makePSU := func(id, name string) *powersupply.PowerSupply {
		return &powersupply.PowerSupply{
			Entity: rfcommon.Entity{
				ID:   id,
				Name: name,
			},
			CapacityWatts: "1200W",
			Model:         "PSUModel",
		}
	}

	testCases := map[string]struct {
		build        func() *PowerShelf
		wantPMCNil   bool
		wantChNil    bool
		wantMgrNil   bool
		wantPsusNil  bool
		wantPsusLen  int
		wantFirstPSU string
	}{
		"zero value shelf": {
			build: func() *PowerShelf {
				var s PowerShelf
				return &s
			},
			wantPMCNil:  true,
			wantChNil:   true,
			wantMgrNil:  true,
			wantPsusNil: true,
			wantPsusLen: 0,
		},
		"full shelf with two PSUs": {
			build: func() *PowerShelf {
				return &PowerShelf{
					PMC: makePMC("00:11:22:33:44:55", "192.168.1.10", validVendor, "admin", "secret"),
					Chassis: &gofish.Chassis{
						SerialNumber: "CHSN",
						Model:        "CHModel",
						Manufacturer: "CHMfg",
					},
					Manager: &gofish.Manager{
						SerialNumber:    "PMCSN",
						Model:           "PMCModel",
						Manufacturer:    "PMCMfg",
						PartNumber:      "PMCPART",
						FirmwareVersion: "PMCFW",
					},
					PowerSupplies: []*powersupply.PowerSupply{
						makePSU("psu-1", "PSU 1"),
						makePSU("psu-2", "PSU 2"),
					},
				}
			},
			wantPMCNil:   false,
			wantChNil:    false,
			wantMgrNil:   false,
			wantPsusNil:  false,
			wantPsusLen:  2,
			wantFirstPSU: "psu-1",
		},
		"nil PSUs slice then append": {
			build: func() *PowerShelf {
				s := &PowerShelf{
					PMC:     makePMC("00:11:22:33:44:55", "192.168.1.10", validVendor, "admin", "secret"),
					Chassis: &gofish.Chassis{Model: "CHModel"},
					Manager: &gofish.Manager{Model: "PMCModel"},
					// PowerSupplies is nil initially
				}
				// Append after construction
				s.PowerSupplies = append(s.PowerSupplies, makePSU("psu-1", "PSU 1"))
				return s
			},
			wantPMCNil:   false,
			wantChNil:    false,
			wantMgrNil:   false,
			wantPsusNil:  false,
			wantPsusLen:  1,
			wantFirstPSU: "psu-1",
		},
		"explicit empty PSUs slice": {
			build: func() *PowerShelf {
				return &PowerShelf{
					PMC:           makePMC("00:11:22:33:44:55", "192.168.1.10", validVendor, "admin", "secret"),
					Chassis:       &gofish.Chassis{Model: "CHModel"},
					Manager:       &gofish.Manager{Model: "PMCModel"},
					PowerSupplies: []*powersupply.PowerSupply{},
				}
			},
			wantPMCNil:  false,
			wantChNil:   false,
			wantMgrNil:  false,
			wantPsusNil: false,
			wantPsusLen: 0,
		},
		"nil PMC allowed (chassis and manager present)": {
			build: func() *PowerShelf {
				return &PowerShelf{
					PMC: nil,
					Chassis: &gofish.Chassis{
						Model: "CHModel",
					},
					Manager: &gofish.Manager{
						Model: "PMCModel",
					},
					PowerSupplies: []*powersupply.PowerSupply{
						makePSU("psu-1", "PSU 1"),
					},
				}
			},
			wantPMCNil:   true,
			wantChNil:    false,
			wantMgrNil:   false,
			wantPsusNil:  false,
			wantPsusLen:  1,
			wantFirstPSU: "psu-1",
		},
	}

	for name, tc := range testCases {
		t.Run(name, func(t *testing.T) {
			shelf := tc.build()

			if tc.wantPMCNil {
				assert.Nil(t, shelf.PMC)
			} else {
				assert.NotNil(t, shelf.PMC)
				assert.Equal(t, "00:11:22:33:44:55", shelf.PMC.GetMac().String())
			}

			if tc.wantChNil {
				assert.Nil(t, shelf.Chassis)
			} else {
				assert.NotNil(t, shelf.Chassis)
			}

			if tc.wantMgrNil {
				assert.Nil(t, shelf.Manager)
			} else {
				assert.NotNil(t, shelf.Manager)
			}

			if tc.wantPsusNil {
				assert.Nil(t, shelf.PowerSupplies)
				assert.Equal(t, 0, len(shelf.PowerSupplies))
			} else {
				assert.NotNil(t, shelf.PowerSupplies)
				assert.Equal(t, tc.wantPsusLen, len(shelf.PowerSupplies))
				if tc.wantPsusLen > 0 {
					assert.Equal(t, tc.wantFirstPSU, shelf.PowerSupplies[0].ID)
				}
			}
		})
	}
}

func TestPowerShelfPointerSemantics(t *testing.T) {
	validVendor := pickValidVendor(t)

	testCases := map[string]struct {
		setup         func() *PowerShelf
		newIP         string
		expectIPAfter string
	}{
		"modifying underlying PMC reflects in shelf (pointer semantics)": {
			setup: func() *PowerShelf {
				p := &PowerShelf{
					PMC:           nil,
					Chassis:       &gofish.Chassis{Model: "CHModel"},
					Manager:       &gofish.Manager{Model: "PMCModel"},
					PowerSupplies: []*powersupply.PowerSupply{&powersupply.PowerSupply{Entity: rfcommon.Entity{ID: "psu-1"}}},
				}
				p.PMC = func() *pmc.PMC {
					obj, err := pmc.New("00:11:22:33:44:55", "192.168.1.10", validVendor, newCred("u", "p"))
					assert.NoError(t, err)
					return obj
				}()
				return p
			},
			newIP:         "10.0.0.5",
			expectIPAfter: "10.0.0.5",
		},
	}

	for name, tc := range testCases {
		t.Run(name, func(t *testing.T) {
			shelf := tc.setup()
			assert.NotNil(t, shelf.PMC)
			assert.Equal(t, "192.168.1.10", shelf.PMC.GetIp().String())

			// Mutate the PMC through its method; shelf should reflect change
			shelf.PMC.SetIP(tc.newIP)
			assert.Equal(t, tc.expectIPAfter, shelf.PMC.GetIp().String())
		})
	}
}
