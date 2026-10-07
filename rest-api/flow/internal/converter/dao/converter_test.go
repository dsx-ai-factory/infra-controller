// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package dao

import (
	"net"
	"testing"

	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"

	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/credential"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/model"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/deviceinfo"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/location"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/utils"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/bmc"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/component"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/rack"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/types"
)

func TestComponentFrom_LeakStatus(t *testing.T) {
	daoComponent := model.Component{
		ID:         uuid.New(),
		Type:       devicetypes.ComponentTypeToString(devicetypes.ComponentTypeCompute),
		LeakStatus: types.LeakStatusDetected,
	}
	assert.Equal(t, types.LeakStatusDetected, ComponentFrom(daoComponent).LeakStatus)

	// The resting value round-trips as LeakStatusUnknown.
	daoComponent.LeakStatus = types.LeakStatusUnknown
	assert.Equal(t, types.LeakStatusUnknown, ComponentFrom(daoComponent).LeakStatus)
}

func TestBMCConversion(t *testing.T) {
	testCases := map[string]struct {
		name            string
		setupDAO        func() model.BMC
		setupInternal   func() *bmc.BMC
		bmcType         devicetypes.BMCType
		component       *model.Component
		expectFromEqual bool
		expectToEqual   bool
	}{
		"valid with all fields": {
			setupDAO: func() model.BMC {
				macStr := "00:11:22:33:44:55"
				ip := "192.168.0.1"
				user := "admin"
				password := "password"
				comp := &model.Component{ID: uuid.New()}
				return model.BMC{
					MacAddress:  macStr,
					Type:        BMCTypeTo(devicetypes.BMCTypeHost),
					User:        &user,
					Password:    &password,
					IPAddress:   &ip,
					ComponentID: comp.ID,
					Component:   comp,
				}
			},
			setupInternal: func() *bmc.BMC {
				mac, _ := net.ParseMAC("00:11:22:33:44:55")
				cred := credential.New("admin", "password")
				return &bmc.BMC{
					MAC:        bmc.MACAddress{HardwareAddr: mac},
					IP:         net.ParseIP("192.168.0.1"),
					Credential: &cred,
				}
			},
			bmcType:         devicetypes.BMCTypeHost,
			component:       &model.Component{ID: uuid.New()},
			expectFromEqual: true,
			expectToEqual:   true,
		},
		"valid without component": {
			setupDAO: func() model.BMC {
				macStr := "aa:bb:cc:dd:ee:ff"
				ip := "10.0.0.1"
				user := "root"
				password := "secret"
				return model.BMC{
					MacAddress:  macStr,
					Type:        BMCTypeTo(devicetypes.BMCTypeDPU),
					User:        &user,
					Password:    &password,
					IPAddress:   &ip,
					ComponentID: uuid.Nil,
					Component:   nil,
				}
			},
			setupInternal: func() *bmc.BMC {
				mac, _ := net.ParseMAC("aa:bb:cc:dd:ee:ff")
				cred := credential.New("root", "secret")
				return &bmc.BMC{
					MAC:        bmc.MACAddress{HardwareAddr: mac},
					IP:         net.ParseIP("10.0.0.1"),
					Credential: &cred,
				}
			},
			bmcType:         devicetypes.BMCTypeDPU,
			component:       nil,
			expectFromEqual: true,
			expectToEqual:   true,
		},
		"invalid MAC address": {
			setupDAO: func() model.BMC {
				return model.BMC{
					MacAddress: "invalid-mac",
					Type:       BMCTypeTo(devicetypes.BMCTypeHost),
				}
			},
			setupInternal: func() *bmc.BMC {
				return &bmc.BMC{
					MAC: bmc.MACAddress{}, // Invalid MAC results in zero-value MACAddress (HardwareAddr is nil)
				}
			},
			bmcType:         devicetypes.BMCTypeHost,
			component:       nil,
			expectFromEqual: true,
			expectToEqual:   false, // BMCTo requires valid MAC
		},
		"nil IP address": {
			setupDAO: func() model.BMC {
				macStr := "11:22:33:44:55:66"
				return model.BMC{
					MacAddress:  macStr,
					Type:        BMCTypeTo(devicetypes.BMCTypeUnknown),
					IPAddress:   nil,
					ComponentID: uuid.Nil,
				}
			},
			setupInternal: func() *bmc.BMC {
				mac, _ := net.ParseMAC("11:22:33:44:55:66")
				return &bmc.BMC{
					MAC: bmc.MACAddress{HardwareAddr: mac},
					IP:  nil,
				}
			},
			bmcType:         devicetypes.BMCTypeUnknown,
			component:       nil,
			expectFromEqual: true,
			expectToEqual:   true,
		},
		"empty IP address": {
			setupDAO: func() model.BMC {
				macStr := "11:22:33:44:55:77"
				emptyIP := ""
				return model.BMC{
					MacAddress:  macStr,
					Type:        BMCTypeTo(devicetypes.BMCTypeHost),
					IPAddress:   &emptyIP,
					ComponentID: uuid.Nil,
				}
			},
			setupInternal: func() *bmc.BMC {
				mac, _ := net.ParseMAC("11:22:33:44:55:77")
				return &bmc.BMC{
					MAC: bmc.MACAddress{HardwareAddr: mac},
					IP:  nil, // Empty string results in nil IP
				}
			},
			bmcType:         devicetypes.BMCTypeHost,
			component:       nil,
			expectFromEqual: true,
			expectToEqual:   false, // Can't reconstruct empty IP string from nil
		},
		"nil credentials": {
			setupDAO: func() model.BMC {
				macStr := "aa:bb:cc:dd:ee:11"
				return model.BMC{
					MacAddress:  macStr,
					Type:        BMCTypeTo(devicetypes.BMCTypeHost),
					User:        nil,
					Password:    nil,
					ComponentID: uuid.Nil,
				}
			},
			setupInternal: func() *bmc.BMC {
				mac, _ := net.ParseMAC("aa:bb:cc:dd:ee:11")
				return &bmc.BMC{
					MAC:        bmc.MACAddress{HardwareAddr: mac},
					Credential: nil,
				}
			},
			bmcType:         devicetypes.BMCTypeHost,
			component:       nil,
			expectFromEqual: true,
			expectToEqual:   true,
		},
		"partial credentials - only user": {
			setupDAO: func() model.BMC {
				macStr := "aa:bb:cc:dd:ee:22"
				user := "testuser"
				return model.BMC{
					MacAddress:  macStr,
					Type:        BMCTypeTo(devicetypes.BMCTypeHost),
					User:        &user,
					Password:    nil,
					ComponentID: uuid.Nil,
				}
			},
			setupInternal: func() *bmc.BMC {
				mac, _ := net.ParseMAC("aa:bb:cc:dd:ee:22")
				return &bmc.BMC{
					MAC:        bmc.MACAddress{HardwareAddr: mac},
					Credential: nil, // BMCFrom requires both user and password
				}
			},
			bmcType:         devicetypes.BMCTypeHost,
			component:       nil,
			expectFromEqual: true,
			expectToEqual:   false, // Can't reconstruct partial credentials
		},
		"partial credentials - only password": {
			setupDAO: func() model.BMC {
				macStr := "aa:bb:cc:dd:ee:33"
				password := "testpass"
				return model.BMC{
					MacAddress:  macStr,
					Type:        BMCTypeTo(devicetypes.BMCTypeHost),
					User:        nil,
					Password:    &password,
					ComponentID: uuid.Nil,
				}
			},
			setupInternal: func() *bmc.BMC {
				mac, _ := net.ParseMAC("aa:bb:cc:dd:ee:33")
				return &bmc.BMC{
					MAC:        bmc.MACAddress{HardwareAddr: mac},
					Credential: nil, // BMCFrom requires both user and password
				}
			},
			bmcType:         devicetypes.BMCTypeHost,
			component:       nil,
			expectFromEqual: true,
			expectToEqual:   false, // Can't reconstruct partial credentials
		},
		"different MAC formats": {
			setupDAO: func() model.BMC {
				macStr := "AA-BB-CC-DD-EE-FF" // Different format
				return model.BMC{
					MacAddress:  macStr,
					Type:        BMCTypeTo(devicetypes.BMCTypeDPU),
					ComponentID: uuid.Nil,
				}
			},
			setupInternal: func() *bmc.BMC {
				mac, _ := net.ParseMAC("AA-BB-CC-DD-EE-FF")
				return &bmc.BMC{
					MAC: bmc.MACAddress{HardwareAddr: mac},
				}
			},
			bmcType:         devicetypes.BMCTypeDPU,
			component:       nil,
			expectFromEqual: true,
			expectToEqual:   false, // BMCTo uses standard format with colons
		},
		"IPv6 address": {
			setupDAO: func() model.BMC {
				macStr := "11:22:33:44:55:88"
				ipv6 := "2001:db8::1"
				return model.BMC{
					MacAddress:  macStr,
					Type:        BMCTypeTo(devicetypes.BMCTypeHost),
					IPAddress:   &ipv6,
					ComponentID: uuid.Nil,
				}
			},
			setupInternal: func() *bmc.BMC {
				mac, _ := net.ParseMAC("11:22:33:44:55:88")
				return &bmc.BMC{
					MAC: bmc.MACAddress{HardwareAddr: mac},
					IP:  net.ParseIP("2001:db8::1"),
				}
			},
			bmcType:         devicetypes.BMCTypeHost,
			component:       nil,
			expectFromEqual: true,
			expectToEqual:   true,
		},
		"empty credentials": {
			setupDAO: func() model.BMC {
				macStr := "11:22:33:44:55:99"
				emptyUser := ""
				emptyPass := ""
				return model.BMC{
					MacAddress:  macStr,
					Type:        BMCTypeTo(devicetypes.BMCTypeHost),
					User:        &emptyUser,
					Password:    &emptyPass,
					ComponentID: uuid.Nil,
				}
			},
			setupInternal: func() *bmc.BMC {
				mac, _ := net.ParseMAC("11:22:33:44:55:99")
				cred := credential.New("", "")
				return &bmc.BMC{
					MAC:        bmc.MACAddress{HardwareAddr: mac},
					Credential: &cred,
				}
			},
			bmcType:         devicetypes.BMCTypeHost,
			component:       nil,
			expectFromEqual: true,
			expectToEqual:   false, // BMCTo will convert empty credentials to nil since they're invalid
		},
	}

	for name, tc := range testCases {
		t.Run(name, func(t *testing.T) {
			daoModel := tc.setupDAO()
			internalModel := tc.setupInternal()

			// Test BMCFrom conversion
			if tc.expectFromEqual {
				result := BMCFrom(daoModel)
				assert.Equal(t, *internalModel, result, "BMCFrom conversion should match expected result")
			}

			// Test BMCTo conversion
			if tc.expectToEqual && internalModel != nil {
				// Adjust component reference for comparison
				if tc.component != nil {
					daoModel.Component = tc.component
					daoModel.ComponentID = tc.component.ID
				}
				result := BMCTo(tc.bmcType, internalModel, tc.component)
				assert.Equal(t, &daoModel, result, "BMCTo conversion should match expected result")
			}
		})
	}
}

func TestBMCTo_NilBMC(t *testing.T) {
	// Test BMCTo with nil BMC
	result := BMCTo(devicetypes.BMCTypeHost, nil, nil)
	assert.Nil(t, result, "BMCTo should return nil for nil BMC input")

	// Test BMCTo with nil BMC but valid component
	comp := &model.Component{ID: uuid.New()}
	result = BMCTo(devicetypes.BMCTypeHost, nil, comp)
	assert.Nil(t, result, "BMCTo should return nil for nil BMC input even with valid component")
}

func TestComponentConversion(t *testing.T) {
	// Test ComponentFrom and ComponentTo
	compID := uuid.New()
	rackID := uuid.New()
	description := map[string]any{"description": "A compute component"}

	daoComponent := model.Component{
		ID:              compID,
		Type:            devicetypes.ComponentTypeToString(devicetypes.ComponentTypeCompute),
		Manufacturer:    "NVIDIA",
		Model:           "ModelX",
		SerialNumber:    "12345",
		Description:     description,
		RackID:          rackID,
		FirmwareVersion: "1.1.21",
		SlotID:          23,
		TrayIndex:       2,
		HostID:          0,
	}

	internalComponent := component.Component{
		Type: devicetypes.ComponentTypeCompute,
		Info: deviceinfo.DeviceInfo{
			ID:           compID,
			Manufacturer: "NVIDIA",
			Model:        "ModelX",
			SerialNumber: "12345",
			Description:  utils.MapToJSONString(description),
		},
		FirmwareVersion: "1.1.21",
		Position: component.InRackPosition{
			SlotID:    23,
			TrayIndex: 2,
			HostID:    0,
		},
		RackID:     rackID,
		BmcsByType: make(map[devicetypes.BMCType][]bmc.BMC),
	}

	assert.Equal(t, internalComponent, *ComponentFrom(daoComponent))
	assert.Equal(t, &daoComponent, ComponentTo(&internalComponent, rackID))
}

func TestComponentFromNVLDomainMembership(t *testing.T) {
	domainID := uuid.New()
	tests := []struct {
		name       string
		rack       *model.Rack
		want       uuid.UUID
		externalID *string
	}{
		{name: "rack relation not loaded"},
		{name: "rack is unassigned", rack: &model.Rack{}},
		{name: "rack is assigned", rack: &model.Rack{NVLDomainID: domainID}, want: domainID},
		{name: "external identity from domain table", rack: &model.Rack{NVLDomainID: domainID, NVLDomain: &model.NVLDomain{ID: domainID, ExternalID: new("group-01")}}, want: domainID, externalID: new("group-01")},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := ComponentFrom(model.Component{Rack: tt.rack})
			assert.Equal(t, tt.want, got.NVLDomainID)
			assert.Equal(t, tt.externalID, got.NVLDomainExternalID)
		})
	}
}

func TestComponentConversionWithComponentID(t *testing.T) {
	compID := uuid.New()
	rackID := uuid.New()
	componentID := "nico-machine-12345"

	testCases := map[string]struct {
		daoComponentID      *string
		expectedComponentID string
	}{
		"with component ID": {
			daoComponentID:      &componentID,
			expectedComponentID: componentID,
		},
		"without component ID (nil)": {
			daoComponentID:      nil,
			expectedComponentID: "",
		},
		"with empty component ID": {
			daoComponentID:      func() *string { s := ""; return &s }(),
			expectedComponentID: "",
		},
	}

	for name, tc := range testCases {
		t.Run(name, func(t *testing.T) {
			daoComponent := model.Component{
				ID:           compID,
				Type:         ComponentTypeTo(devicetypes.ComponentTypeCompute),
				Manufacturer: "NVIDIA",
				SerialNumber: "12345",
				RackID:       rackID,
				ComponentID:  tc.daoComponentID,
			}

			// Test ComponentFrom
			result := ComponentFrom(daoComponent)
			assert.Equal(t, tc.expectedComponentID, result.ComponentID)

			// Test ComponentTo (round-trip)
			internalComponent := component.Component{
				Type: devicetypes.ComponentTypeCompute,
				Info: deviceinfo.DeviceInfo{
					ID:           compID,
					Manufacturer: "NVIDIA",
					SerialNumber: "12345",
				},
				BmcsByType:  make(map[devicetypes.BMCType][]bmc.BMC),
				ComponentID: tc.expectedComponentID,
			}

			daoResult := ComponentTo(&internalComponent, rackID)

			if tc.expectedComponentID == "" {
				assert.Nil(t, daoResult.ComponentID)
			} else {
				assert.NotNil(t, daoResult.ComponentID)
				assert.Equal(t, tc.expectedComponentID, *daoResult.ComponentID)
			}
		})
	}
}

func TestRackConversion(t *testing.T) {
	// Test RackFrom and RackTo
	rackID := uuid.New()
	domainID := uuid.New()
	description := map[string]any{"description": "A rack", "model": "NVL72"}
	loc := `{"region":"US-West","data_center":"Santa Clara","room":"Mars","position":"Row 5"}`

	daoRack := model.Rack{
		ID:           rackID,
		Name:         "Rack1",
		Manufacturer: "NVIDIA",
		SerialNumber: "67890",
		Description:  description,
		Location:     utils.JSONStringToMap("location", loc),
		Components:   []model.Component{},
		NVLDomainID:  domainID,
	}

	internalRack := rack.Rack{
		Info: deviceinfo.DeviceInfo{
			ID:           rackID,
			Name:         "Rack1",
			Manufacturer: "NVIDIA",
			Model:        "NVL72",
			SerialNumber: "67890",
			Description:  "A rack",
		},
		Loc:         location.New([]byte(loc)),
		Components:  []component.Component{},
		NVLDomainID: domainID,
	}

	assert.Equal(t, &internalRack, RackFrom(&daoRack))
	assert.Equal(t, &daoRack, RackTo(&internalRack))
}

func TestRackFromPropagatesNVLDomainMembershipToComponents(t *testing.T) {
	domainID := uuid.New()
	got := RackFrom(&model.Rack{
		NVLDomainID: domainID,
		NVLDomain:   &model.NVLDomain{ID: domainID, ExternalID: new("group-01")},
		Components:  []model.Component{{ID: uuid.New()}},
	})

	assert.Equal(t, domainID, got.Components[0].NVLDomainID)
	assert.Equal(t, new("group-01"), got.NVLDomainExternalID)
	assert.Equal(t, got.NVLDomainExternalID, got.Components[0].NVLDomainExternalID)
}

func TestRackMetadataFromDescription(t *testing.T) {
	tests := []struct {
		name            string
		stored          map[string]any
		wantModel       string
		wantDescription string
	}{
		{
			name:            "Core model and text",
			stored:          map[string]any{"model": "NVL72", "text": "Core rack"},
			wantModel:       "NVL72",
			wantDescription: "Core rack",
		},
		{
			name:            "Flow description",
			stored:          map[string]any{"description": "Flow rack"},
			wantDescription: "Flow rack",
		},
		{
			name:      "model only",
			stored:    map[string]any{"model": "NVL72"},
			wantModel: "NVL72",
		},
		{
			name:            "unknown structured metadata remains JSON",
			stored:          map[string]any{"owner": "ops", "zone": "west"},
			wantDescription: `{"owner":"ops","zone":"west"}`,
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			gotModel, gotDescription := rackMetadataFromDescription(tc.stored)
			assert.Equal(t, tc.wantModel, gotModel)
			assert.Equal(t, tc.wantDescription, gotDescription)
		})
	}
}
