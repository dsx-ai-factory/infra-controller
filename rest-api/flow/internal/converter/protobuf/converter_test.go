// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package protobuf

import (
	"net"
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"google.golang.org/protobuf/types/known/timestamppb"

	dbquery "github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/query"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/operation"
	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	identifier "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/Identifier"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/deviceinfo"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/location"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/bmc"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/component"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/rack"
	pb "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/proto/v1"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/types"
)

func TestLeakStatusTo(t *testing.T) {
	cases := map[types.LeakStatus]pb.LeakStatus{
		types.LeakStatusDetected:    pb.LeakStatus_LEAK_STATUS_DETECTED,
		types.LeakStatusNotDetected: pb.LeakStatus_LEAK_STATUS_NOT_DETECTED,
		types.LeakStatusUnknown:     pb.LeakStatus_LEAK_STATUS_UNKNOWN,
		types.LeakStatus(""):        pb.LeakStatus_LEAK_STATUS_UNKNOWN,
		types.LeakStatus("bogus"):   pb.LeakStatus_LEAK_STATUS_UNKNOWN,
	}
	for in, want := range cases {
		assert.Equal(t, want, LeakStatusTo(in), "LeakStatusTo(%q)", in)
	}
}

func TestUUIDFrom(t *testing.T) {
	testID := uuid.New()
	testCases := map[string]struct {
		id       *pb.UUID
		expected uuid.UUID
	}{
		"valid protobuf uuid": {
			id:       &pb.UUID{Id: testID.String()},
			expected: testID,
		},
		"nil protobuf uuid": {
			id:       nil,
			expected: uuid.Nil,
		},
		"invalid protobuf uuid": {
			id:       &pb.UUID{Id: "invalid-uuid"},
			expected: uuid.Nil,
		},
	}

	for name, testCase := range testCases {
		t.Run(name, func(t *testing.T) {
			assert.Equal(t, testCase.expected, UUIDFrom(testCase.id))
		})
	}
}

func TestComponentTypeConverter(t *testing.T) {
	for typ, ptype := range componentTypeToMap {
		assert.Equal(t, typ, ComponentTypeFrom(ptype))
		assert.Equal(t, ptype, ComponentTypeTo(typ))
	}

	assert.Equal(t, devicetypes.ComponentTypeUnknown, ComponentTypeFrom(pb.ComponentType(-1)))               //nolint
	assert.Equal(t, pb.ComponentType_COMPONENT_TYPE_UNKNOWN, ComponentTypeTo(devicetypes.ComponentType(-1))) //nolint
}

func TestBMCTypeConverter(t *testing.T) {
	for typ, ptype := range bmcTypeToMap {
		assert.Equal(t, typ, BMCTypeFrom(ptype))
		assert.Equal(t, ptype, BMCTypeTo(typ))
	}

	assert.Equal(t, devicetypes.BMCTypeUnknown, BMCTypeFrom(pb.BMCType(-1)))         //nolint
	assert.Equal(t, pb.BMCType_BMC_TYPE_UNKNOWN, BMCTypeTo(devicetypes.BMCType(-1))) //nolint
}

func TestDeviceInfoConverter(t *testing.T) {
	shared := deviceinfo.NewRandom("some device", 5)

	sharedP := pb.DeviceInfo{
		Id:           &pb.UUID{Id: shared.ID.String()},
		Name:         shared.Name,
		Manufacturer: shared.Manufacturer,
		Model:        &shared.Model,
		SerialNumber: shared.SerialNumber,
		Description:  &shared.Description,
	}

	testCases := map[string]struct {
		source     *deviceinfo.DeviceInfo
		sourceP    *pb.DeviceInfo
		converted  *deviceinfo.DeviceInfo
		convertedP *pb.DeviceInfo
	}{
		"valid": {
			source:     &shared,
			sourceP:    &sharedP,
			converted:  &shared,
			convertedP: &sharedP,
		},
		"nil": {
			source:     nil,
			sourceP:    nil,
			converted:  &deviceinfo.DeviceInfo{},
			convertedP: nil,
		},
		"empty fields": {
			source: &deviceinfo.DeviceInfo{
				ID:           uuid.Nil,
				Name:         shared.Name,
				Manufacturer: shared.Manufacturer,
				Model:        "",
				SerialNumber: shared.SerialNumber,
				Description:  "",
			},
			sourceP: &pb.DeviceInfo{
				Id:           nil,
				Name:         sharedP.Name,
				Manufacturer: sharedP.Manufacturer,
				SerialNumber: sharedP.SerialNumber,
			},
			converted: &deviceinfo.DeviceInfo{
				ID:           uuid.Nil,
				Name:         shared.Name,
				Manufacturer: shared.Manufacturer,
				Model:        "",
				SerialNumber: shared.SerialNumber,
				Description:  "",
			},
			convertedP: &pb.DeviceInfo{
				Id:           nil,
				Name:         sharedP.Name,
				Manufacturer: sharedP.Manufacturer,
				SerialNumber: sharedP.SerialNumber,
			},
		},
	}

	for name, testCase := range testCases {
		t.Run(name, func(t *testing.T) {
			assert.Equal(t, *testCase.converted, DeviceInfoFrom(testCase.sourceP))
			assert.Equal(t, testCase.convertedP, DeviceInfoTo(testCase.source))
		})
	}
}

func TestLocationConverter(t *testing.T) {
	shared := location.Location{
		Region:     "US",
		DataCenter: "DC1",
		Room:       "Room1",
		Position:   "Pos1",
	}

	sharedP := pb.Location{
		Region:     shared.Region,
		Datacenter: shared.DataCenter,
		Room:       shared.Room,
		Position:   shared.Position,
	}

	testCases := map[string]struct {
		source     *location.Location
		sourceP    *pb.Location
		converted  *location.Location
		convertedP *pb.Location
	}{
		"valid": {
			source:     &shared,
			sourceP:    &sharedP,
			converted:  &shared,
			convertedP: &sharedP,
		},
		"nil": {
			source:     nil,
			sourceP:    nil,
			converted:  &location.Location{},
			convertedP: nil,
		},
	}

	for name, testCase := range testCases {
		t.Run(name, func(t *testing.T) {
			assert.Equal(t, *testCase.converted, LocationFrom(testCase.sourceP))
			assert.Equal(t, testCase.convertedP, LocationTo(testCase.source))
		})
	}
}

func TestRackPositionConverter(t *testing.T) {
	shared := component.InRackPosition{
		SlotID:    1,
		TrayIndex: 2,
		HostID:    3,
	}

	sharedP := pb.RackPosition{
		SlotId:  int32(shared.SlotID),
		TrayIdx: int32(shared.TrayIndex),
		HostId:  int32(shared.HostID),
	}

	testCases := map[string]struct {
		source     *component.InRackPosition
		sourceP    *pb.RackPosition
		converted  *component.InRackPosition
		convertedP *pb.RackPosition
	}{
		"valid": {
			source:     &shared,
			sourceP:    &sharedP,
			converted:  &shared,
			convertedP: &sharedP,
		},
		"nil": {
			source:  nil,
			sourceP: nil,
			converted: &component.InRackPosition{
				SlotID:    -1,
				TrayIndex: -1,
				HostID:    -1,
			},
			convertedP: nil,
		},
	}

	for name, testCase := range testCases {
		t.Run(name, func(t *testing.T) {
			assert.Equal(t, *testCase.converted, RackPositionFrom(testCase.sourceP))
			assert.Equal(t, testCase.convertedP, RackPositionTo(testCase.source))
		})
	}
}

func TestBMCFrom(t *testing.T) {
	stringPtr := func(s string) *string {
		return &s
	}

	mustParseMAC := func(s string) net.HardwareAddr {
		mac, err := net.ParseMAC(s)
		if err != nil {
			panic(err)
		}
		return mac
	}

	sharedMac := "00:1a:2b:3c:4d:5e"
	sharedIp := "192.168.1.1"

	testCases := map[string]struct {
		name          string
		sourceP       *pb.BMCInfo
		source        *bmc.BMC
		expectedType  devicetypes.BMCType
		expectedBMC   *bmc.BMC
		expectedProto *pb.BMCInfo
		testBMCTo     bool
		testBMCToType devicetypes.BMCType
	}{
		"valid host BMC with all fields": {
			sourceP: &pb.BMCInfo{
				Type:       pb.BMCType_BMC_TYPE_HOST,
				MacAddress: sharedMac,
				IpAddress:  stringPtr(sharedIp),
			},
			expectedType: devicetypes.BMCTypeHost,
			expectedBMC: &bmc.BMC{
				MAC: bmc.MACAddress{HardwareAddr: mustParseMAC(sharedMac)},
				IP:  net.ParseIP(sharedIp),
			},
			source: &bmc.BMC{
				MAC: bmc.MACAddress{HardwareAddr: mustParseMAC(sharedMac)},
				IP:  net.ParseIP(sharedIp),
			},
			expectedProto: &pb.BMCInfo{
				Type:       pb.BMCType_BMC_TYPE_HOST,
				MacAddress: sharedMac,
				IpAddress:  stringPtr(sharedIp),
			},
			testBMCTo:     true,
			testBMCToType: devicetypes.BMCTypeHost,
		},
		"valid DPU BMC": {
			sourceP: &pb.BMCInfo{
				Type:       pb.BMCType_BMC_TYPE_DPU,
				MacAddress: sharedMac,
				IpAddress:  stringPtr(sharedIp),
			},
			expectedType: devicetypes.BMCTypeDPU,
			expectedBMC: &bmc.BMC{
				MAC: bmc.MACAddress{HardwareAddr: mustParseMAC(sharedMac)},
				IP:  net.ParseIP(sharedIp),
			},
			source: &bmc.BMC{
				MAC: bmc.MACAddress{HardwareAddr: mustParseMAC(sharedMac)},
				IP:  net.ParseIP(sharedIp),
			},
			expectedProto: &pb.BMCInfo{
				Type:       pb.BMCType_BMC_TYPE_DPU,
				MacAddress: sharedMac,
				IpAddress:  stringPtr(sharedIp),
			},
			testBMCTo:     true,
			testBMCToType: devicetypes.BMCTypeDPU,
		},
		"unknown BMC type": {
			sourceP: &pb.BMCInfo{
				Type:       pb.BMCType_BMC_TYPE_UNKNOWN,
				MacAddress: sharedMac,
			},
			expectedType: devicetypes.BMCTypeUnknown,
			expectedBMC: &bmc.BMC{
				MAC: bmc.MACAddress{HardwareAddr: mustParseMAC(sharedMac)},
			},
			source: &bmc.BMC{
				MAC: bmc.MACAddress{HardwareAddr: mustParseMAC(sharedMac)},
			},
			expectedProto: &pb.BMCInfo{
				Type:       pb.BMCType_BMC_TYPE_UNKNOWN,
				MacAddress: sharedMac,
			},
			testBMCTo:     true,
			testBMCToType: devicetypes.BMCTypeUnknown,
		},
		"nil BMCInfo": {
			sourceP:       nil,
			expectedType:  devicetypes.BMCTypeUnknown,
			expectedBMC:   nil,
			source:        nil,
			expectedProto: nil,
			testBMCTo:     true,
			testBMCToType: devicetypes.BMCTypeHost,
		},
		"invalid MAC address": {
			sourceP: &pb.BMCInfo{
				Type:       pb.BMCType_BMC_TYPE_HOST,
				MacAddress: "invalid-mac-address",
			},
			expectedType: devicetypes.BMCTypeHost,
			expectedBMC: &bmc.BMC{
				MAC: bmc.MACAddress{},
			},
			source: &bmc.BMC{
				MAC: bmc.MACAddress{},
			},
			expectedProto: &pb.BMCInfo{
				Type:       pb.BMCType_BMC_TYPE_HOST,
				MacAddress: "",
			},
			testBMCTo:     true,
			testBMCToType: devicetypes.BMCTypeHost,
		},
		"empty MAC address": {
			sourceP: &pb.BMCInfo{
				Type:       pb.BMCType_BMC_TYPE_HOST,
				MacAddress: "",
			},
			expectedType: devicetypes.BMCTypeHost,
			expectedBMC: &bmc.BMC{
				MAC: bmc.MACAddress{},
			},
			source: &bmc.BMC{
				MAC: bmc.MACAddress{},
			},
			expectedProto: &pb.BMCInfo{
				Type:       pb.BMCType_BMC_TYPE_HOST,
				MacAddress: "",
			},
			testBMCTo:     true,
			testBMCToType: devicetypes.BMCTypeHost,
		},
		"nil IP address": {
			sourceP: &pb.BMCInfo{
				Type:       pb.BMCType_BMC_TYPE_HOST,
				MacAddress: sharedMac,
				IpAddress:  nil,
			},
			expectedType: devicetypes.BMCTypeHost,
			expectedBMC: &bmc.BMC{
				MAC: bmc.MACAddress{HardwareAddr: mustParseMAC(sharedMac)},
				IP:  nil,
			},
			source: &bmc.BMC{
				MAC: bmc.MACAddress{HardwareAddr: mustParseMAC(sharedMac)},
				IP:  nil,
			},
			expectedProto: &pb.BMCInfo{
				Type:       pb.BMCType_BMC_TYPE_HOST,
				MacAddress: sharedMac,
				IpAddress:  nil,
			},
			testBMCTo:     true,
			testBMCToType: devicetypes.BMCTypeHost,
		},
		"empty IP address": {
			sourceP: &pb.BMCInfo{
				Type:       pb.BMCType_BMC_TYPE_HOST,
				MacAddress: sharedMac,
				IpAddress:  stringPtr(""),
			},
			expectedType: devicetypes.BMCTypeHost,
			expectedBMC: &bmc.BMC{
				MAC: bmc.MACAddress{HardwareAddr: mustParseMAC(sharedMac)},
				IP:  nil,
			},
			testBMCTo: false,
		},
		"invalid IP address": {
			sourceP: &pb.BMCInfo{
				Type:       pb.BMCType_BMC_TYPE_HOST,
				MacAddress: sharedMac,
				IpAddress:  stringPtr("invalid-ip"),
			},
			expectedType: devicetypes.BMCTypeHost,
			expectedBMC: &bmc.BMC{
				MAC: bmc.MACAddress{HardwareAddr: mustParseMAC(sharedMac)},
				IP:  nil,
			},
			testBMCTo: false,
		},
		"IPv6 address": {
			sourceP: &pb.BMCInfo{
				Type:       pb.BMCType_BMC_TYPE_HOST,
				MacAddress: sharedMac,
				IpAddress:  stringPtr("2001:db8::1"),
			},
			expectedType: devicetypes.BMCTypeHost,
			expectedBMC: &bmc.BMC{
				MAC: bmc.MACAddress{HardwareAddr: mustParseMAC(sharedMac)},
				IP:  net.ParseIP("2001:db8::1"),
			},
			source: &bmc.BMC{
				MAC: bmc.MACAddress{HardwareAddr: mustParseMAC(sharedMac)},
				IP:  net.ParseIP("2001:db8::1"),
			},
			expectedProto: &pb.BMCInfo{
				Type:       pb.BMCType_BMC_TYPE_HOST,
				MacAddress: sharedMac,
				IpAddress:  stringPtr("2001:db8::1"),
			},
			testBMCTo:     true,
			testBMCToType: devicetypes.BMCTypeHost,
		},
		"BMC without credentials": {
			sourceP: &pb.BMCInfo{
				Type:       pb.BMCType_BMC_TYPE_HOST,
				MacAddress: sharedMac,
			},
			expectedType: devicetypes.BMCTypeHost,
			expectedBMC: &bmc.BMC{
				MAC: bmc.MACAddress{HardwareAddr: mustParseMAC(sharedMac)},
			},
			source: &bmc.BMC{
				MAC: bmc.MACAddress{HardwareAddr: mustParseMAC(sharedMac)},
			},
			expectedProto: &pb.BMCInfo{
				Type:       pb.BMCType_BMC_TYPE_HOST,
				MacAddress: sharedMac,
			},
			testBMCTo:     true,
			testBMCToType: devicetypes.BMCTypeHost,
		},
		"different MAC formats": {
			sourceP: &pb.BMCInfo{
				Type:       pb.BMCType_BMC_TYPE_DPU,
				MacAddress: "AA-BB-CC-DD-EE-FF",
			},
			expectedType: devicetypes.BMCTypeDPU,
			expectedBMC: &bmc.BMC{
				MAC: bmc.MACAddress{HardwareAddr: mustParseMAC("AA-BB-CC-DD-EE-FF")},
			},
			source: &bmc.BMC{
				MAC: bmc.MACAddress{HardwareAddr: mustParseMAC("AA-BB-CC-DD-EE-FF")},
			},
			expectedProto: &pb.BMCInfo{
				Type:       pb.BMCType_BMC_TYPE_DPU,
				MacAddress: "aa:bb:cc:dd:ee:ff",
			},
			testBMCTo:     true,
			testBMCToType: devicetypes.BMCTypeDPU,
		},
		"invalid BMC type": {
			sourceP: &pb.BMCInfo{
				Type:       pb.BMCType(-1),
				MacAddress: sharedMac,
			},
			expectedType: devicetypes.BMCTypeUnknown,
			expectedBMC: &bmc.BMC{
				MAC: bmc.MACAddress{HardwareAddr: mustParseMAC(sharedMac)},
			},
			testBMCTo: false,
		},
	}

	for name, tc := range testCases {
		t.Run(name, func(t *testing.T) {
			convertedType, converted := BMCFrom(tc.sourceP)
			assert.Equal(t, tc.expectedType, convertedType, "BMCFrom should return expected type") //nolint
			assert.Equal(t, tc.expectedBMC, converted, "BMCFrom should return expected BMC")       //nolint

			if tc.testBMCTo && tc.source != nil {
				convertedProto := BMCTo(tc.testBMCToType, tc.source)
				assert.Equal(t, tc.expectedProto, convertedProto, "BMCTo should return expected protobuf BMC") //nolint
			} else if tc.testBMCTo && tc.source == nil {
				convertedProto := BMCTo(tc.testBMCToType, tc.source)
				assert.Nil(t, convertedProto, "BMCTo should return nil for nil BMC") //nolint
			}
		})
	}
}

func TestComponentConverter(t *testing.T) {
	domainID := uuid.New()
	shared := component.Component{
		Type:            devicetypes.ComponentTypeCompute,
		Info:            deviceinfo.NewRandom("TestComponent", 6),
		FirmwareVersion: "1.0.0",
		RackExternalID:  "rack-external-1",
		Position: component.InRackPosition{
			SlotID:    26,
			TrayIndex: 12,
			HostID:    0,
		},
		BmcsByType:  make(map[devicetypes.BMCType][]bmc.BMC),
		NVLDomainID: domainID,
	}

	sharedP := pb.Component{
		Type: pb.ComponentType_COMPONENT_TYPE_COMPUTE,
		Info: &pb.DeviceInfo{
			Id:           &pb.UUID{Id: shared.Info.ID.String()},
			Name:         shared.Info.Name,
			Manufacturer: shared.Info.Manufacturer,
			Model:        &shared.Info.Model,
			SerialNumber: shared.Info.SerialNumber,
			Description:  &shared.Info.Description,
		},
		FirmwareVersion: shared.FirmwareVersion,
		Position: &pb.RackPosition{
			SlotId:  int32(shared.Position.SlotID),
			TrayIdx: int32(shared.Position.TrayIndex),
			HostId:  int32(shared.Position.HostID),
		},
		Bmcs:           make([]*pb.BMCInfo, 0),
		NvlDomainId:    &pb.UUID{Id: domainID.String()},
		RackExternalId: shared.RackExternalID,
	}

	testCases := map[string]struct {
		source     *component.Component
		sourceP    *pb.Component
		converted  *component.Component
		convertedP *pb.Component
	}{
		"valid": {
			source:     &shared,
			sourceP:    &sharedP,
			converted:  &shared,
			convertedP: &sharedP,
		},
		"nil": {
			source:     nil,
			sourceP:    nil,
			converted:  nil,
			convertedP: nil,
		},
	}

	for name, testCase := range testCases {
		t.Run(name, func(t *testing.T) {
			assert.Equal(t, testCase.converted, ComponentFrom(testCase.sourceP))
			assert.Equal(t, testCase.convertedP, ComponentTo(testCase.source))
		})
	}
}

func TestRackConverter(t *testing.T) {
	domainID := uuid.New()
	shared := rack.Rack{
		Info:       deviceinfo.NewRandom("TestRack", 12),
		ExternalID: "rack-external-1",
		Loc: location.Location{
			Region:     "US",
			DataCenter: "Santa Clara",
			Room:       "Mars",
			Position:   "Row 12",
		},
		Components:  make([]component.Component, 0),
		NVLDomainID: domainID,
	}

	sharedP := pb.Rack{
		Info: &pb.DeviceInfo{
			Id:           &pb.UUID{Id: shared.Info.ID.String()},
			Name:         shared.Info.Name,
			Manufacturer: shared.Info.Manufacturer,
			Model:        &shared.Info.Model,
			SerialNumber: shared.Info.SerialNumber,
			Description:  &shared.Info.Description,
		},
		Location: &pb.Location{
			Region:     shared.Loc.Region,
			Datacenter: shared.Loc.DataCenter,
			Room:       shared.Loc.Room,
			Position:   shared.Loc.Position,
		},
		Components:   make([]*pb.Component, 0),
		NvlDomainIds: []*pb.UUID{{Id: domainID.String()}},
		ExternalId:   shared.ExternalID,
	}
	testCases := map[string]struct {
		source     *rack.Rack
		sourceP    *pb.Rack
		converted  *rack.Rack
		convertedP *pb.Rack
	}{
		"valid": {
			source:     &shared,
			sourceP:    &sharedP,
			converted:  &shared,
			convertedP: &sharedP,
		},
		"nil": {
			source:     nil,
			sourceP:    nil,
			converted:  nil,
			convertedP: nil,
		},
	}

	for name, testCase := range testCases {
		t.Run(name, func(t *testing.T) {
			assert.Equal(t, testCase.converted, RackFrom(testCase.sourceP))
			assert.Equal(t, testCase.convertedP, RackTo(testCase.source))
		})
	}
}

func TestRackConverterPropagatesDomainToNestedComponents(t *testing.T) {
	domainID := uuid.New()
	explicitComponentDomainID := uuid.New()
	componentID := uuid.New()

	fromProto := RackFrom(&pb.Rack{
		Info:         &pb.DeviceInfo{Id: UUIDTo(uuid.New())},
		NvlDomainIds: UUIDsTo([]uuid.UUID{domainID, uuid.New()}),
		Components: []*pb.Component{
			{Info: &pb.DeviceInfo{Id: UUIDTo(componentID)}},
			{
				Info:        &pb.DeviceInfo{Id: UUIDTo(uuid.New())},
				NvlDomainId: UUIDTo(explicitComponentDomainID),
			},
		},
	})
	require.Len(t, fromProto.Components, 2)
	assert.Equal(t, domainID, fromProto.NVLDomainID)
	assert.Equal(t, domainID, fromProto.Components[0].NVLDomainID)
	assert.Equal(t, explicitComponentDomainID, fromProto.Components[1].NVLDomainID)

	toProto := RackTo(&rack.Rack{
		NVLDomainID: domainID,
		Components: []component.Component{
			{Info: deviceinfo.DeviceInfo{ID: componentID}},
			{NVLDomainID: explicitComponentDomainID},
		},
	})
	require.Len(t, toProto.GetComponents(), 2)
	assert.Equal(t, domainID.String(), toProto.GetComponents()[0].GetNvlDomainId().GetId())
	assert.Equal(t, explicitComponentDomainID.String(), toProto.GetComponents()[1].GetNvlDomainId().GetId())
}

func TestOrderByConverter(t *testing.T) {
	testCases := map[string]struct {
		source     *pb.OrderBy
		sourceDB   *dbquery.OrderBy
		queryType  QueryType // QueryTypeRack or QueryTypeComponent
		converted  *dbquery.OrderBy
		convertedP *pb.OrderBy
	}{
		"rack name ASC": {
			source: &pb.OrderBy{
				Field:     &pb.OrderBy_RackField{RackField: pb.RackOrderByField_RACK_ORDER_BY_FIELD_NAME},
				Direction: "ASC",
			},
			sourceDB: &dbquery.OrderBy{
				Column:    "name",
				Direction: dbquery.OrderAscending,
			},
			queryType: QueryTypeRack,
			converted: &dbquery.OrderBy{
				Column:    "name",
				Direction: dbquery.OrderAscending,
			},
			convertedP: &pb.OrderBy{
				Field:     &pb.OrderBy_RackField{RackField: pb.RackOrderByField_RACK_ORDER_BY_FIELD_NAME},
				Direction: "ASC",
			},
		},
		"rack manufacturer DESC": {
			source: &pb.OrderBy{
				Field:     &pb.OrderBy_RackField{RackField: pb.RackOrderByField_RACK_ORDER_BY_FIELD_MANUFACTURER},
				Direction: "DESC",
			},
			sourceDB: &dbquery.OrderBy{
				Column:    "manufacturer",
				Direction: dbquery.OrderDescending,
			},
			queryType: QueryTypeRack,
			converted: &dbquery.OrderBy{
				Column:    "manufacturer",
				Direction: dbquery.OrderDescending,
			},
			convertedP: &pb.OrderBy{
				Field:     &pb.OrderBy_RackField{RackField: pb.RackOrderByField_RACK_ORDER_BY_FIELD_MANUFACTURER},
				Direction: "DESC",
			},
		},
		"component name ASC": {
			source: &pb.OrderBy{
				Field:     &pb.OrderBy_ComponentField{ComponentField: pb.ComponentOrderByField_COMPONENT_ORDER_BY_FIELD_NAME},
				Direction: "ASC",
			},
			sourceDB: &dbquery.OrderBy{
				Column:    "name",
				Direction: dbquery.OrderAscending,
			},
			queryType: QueryTypeComponent,
			converted: &dbquery.OrderBy{
				Column:    "name",
				Direction: dbquery.OrderAscending,
			},
			convertedP: &pb.OrderBy{
				Field:     &pb.OrderBy_ComponentField{ComponentField: pb.ComponentOrderByField_COMPONENT_ORDER_BY_FIELD_NAME},
				Direction: "ASC",
			},
		},
		"component type DESC": {
			source: &pb.OrderBy{
				Field:     &pb.OrderBy_ComponentField{ComponentField: pb.ComponentOrderByField_COMPONENT_ORDER_BY_FIELD_TYPE},
				Direction: "DESC",
			},
			sourceDB: &dbquery.OrderBy{
				Column:    "type",
				Direction: dbquery.OrderDescending,
			},
			queryType: QueryTypeComponent,
			converted: &dbquery.OrderBy{
				Column:    "type",
				Direction: dbquery.OrderDescending,
			},
			convertedP: &pb.OrderBy{
				Field:     &pb.OrderBy_ComponentField{ComponentField: pb.ComponentOrderByField_COMPONENT_ORDER_BY_FIELD_TYPE},
				Direction: "DESC",
			},
		},
		"nil protobuf": {
			source:     nil,
			sourceDB:   nil,
			queryType:  QueryTypeRack,
			converted:  nil,
			convertedP: nil,
		},
		"nil dbquery": {
			source:     nil,
			sourceDB:   nil,
			queryType:  QueryTypeRack,
			converted:  nil,
			convertedP: nil,
		},
	}

	for name, tc := range testCases {
		t.Run(name, func(t *testing.T) {
			// Test OrderByFrom conversion
			converted := OrderByFrom(tc.source)
			assert.Equal(t, tc.converted, converted, "OrderByFrom should return expected OrderBy")

			// Test OrderByTo conversion
			convertedP := OrderByTo(tc.sourceDB, tc.queryType)
			assert.Equal(t, tc.convertedP, convertedP, "OrderByTo should return expected protobuf OrderBy")
		})
	}
}

func TestOptionalUUIDFrom(t *testing.T) {
	testID := uuid.New()

	t.Run("nil input returns nil", func(t *testing.T) {
		result := OptionalUUIDFrom(nil)
		assert.Nil(t, result)
	})

	t.Run("valid UUID returns pointer", func(t *testing.T) {
		result := OptionalUUIDFrom(&pb.UUID{Id: testID.String()})
		assert.NotNil(t, result)
		assert.Equal(t, testID, *result)
	})

	t.Run("empty string returns nil", func(t *testing.T) {
		result := OptionalUUIDFrom(&pb.UUID{Id: ""})
		assert.Nil(t, result)
	})

	t.Run("invalid UUID returns nil", func(t *testing.T) {
		result := OptionalUUIDFrom(&pb.UUID{Id: "not-a-uuid"})
		assert.Nil(t, result)
	})
}

func TestRackTargetFrom(t *testing.T) {
	rackID := uuid.New()

	testCases := map[string]struct {
		input   *pb.RackTarget
		want    operation.RackTarget
		wantErr string
	}{
		"nil input": {
			input:   nil,
			wantErr: "rack target is nil",
		},
		"no identifier set": {
			input:   &pb.RackTarget{},
			wantErr: "rack target must have either id, external_id, or name set",
		},
		"valid UUID": {
			input: &pb.RackTarget{
				Identifier: &pb.RackTarget_Id{Id: &pb.UUID{Id: rackID.String()}},
			},
			want: operation.RackTarget{Identifier: identifier.Identifier{ID: rackID}},
		},
		"external rack ID": {
			input: &pb.RackTarget{
				Identifier: &pb.RackTarget_ExternalId{ExternalId: "rack-1"},
			},
			want: operation.RackTarget{Identifier: identifier.Identifier{ExternalID: "rack-1"}},
		},
		"invalid UUID": {
			input: &pb.RackTarget{
				Identifier: &pb.RackTarget_Id{Id: &pb.UUID{Id: "rack-1"}},
			},
			wantErr: "invalid rack uuid",
		},
		"empty ID": {
			input: &pb.RackTarget{
				Identifier: &pb.RackTarget_Id{Id: &pb.UUID{}},
			},
			wantErr: "rack target id must not be empty",
		},
		"empty external rack ID": {
			input: &pb.RackTarget{
				Identifier: &pb.RackTarget_ExternalId{},
			},
			wantErr: "rack target external_id must not be empty",
		},
		"valid name": {
			input: &pb.RackTarget{
				Identifier: &pb.RackTarget_Name{Name: "rack-1"},
			},
			want: operation.RackTarget{Identifier: identifier.Identifier{Name: "rack-1"}},
		},
		"empty name": {
			input: &pb.RackTarget{
				Identifier: &pb.RackTarget_Name{Name: ""},
			},
			wantErr: "rack target name must not be empty",
		},
	}

	for name, tc := range testCases {
		t.Run(name, func(t *testing.T) {
			got, err := RackTargetFrom(tc.input)
			if tc.wantErr != "" {
				assert.ErrorContains(t, err, tc.wantErr)
				assert.Equal(t, operation.RackTarget{}, got)
				return
			}
			assert.NoError(t, err)
			assert.Equal(t, tc.want, got)
		})
	}
}

func TestComponentTargetFrom(t *testing.T) {
	compID := uuid.New()

	testCases := map[string]struct {
		input   *pb.ComponentTarget
		want    operation.ComponentTarget
		wantErr string
	}{
		"nil input": {
			input:   nil,
			wantErr: "component target is nil",
		},
		"no identifier set": {
			input:   &pb.ComponentTarget{},
			wantErr: "component target must have either uuid or external set",
		},
		"valid UUID": {
			input: &pb.ComponentTarget{
				Identifier: &pb.ComponentTarget_Id{Id: &pb.UUID{Id: compID.String()}},
			},
			want: operation.ComponentTarget{UUID: compID},
		},
		"invalid UUID string": {
			input: &pb.ComponentTarget{
				Identifier: &pb.ComponentTarget_Id{Id: &pb.UUID{Id: "bad-uuid"}},
			},
			wantErr: "invalid component uuid",
		},
		"valid external": {
			input: &pb.ComponentTarget{
				Identifier: &pb.ComponentTarget_External{
					External: &pb.ExternalRef{
						Type: pb.ComponentType_COMPONENT_TYPE_COMPUTE,
						Id:   "ext-123",
					},
				},
			},
			want: operation.ComponentTarget{
				External: &operation.ExternalRef{
					Type: devicetypes.ComponentTypeCompute,
					ID:   "ext-123",
				},
			},
		},
		"external with unknown component type": {
			input: &pb.ComponentTarget{
				Identifier: &pb.ComponentTarget_External{
					External: &pb.ExternalRef{
						Type: pb.ComponentType_COMPONENT_TYPE_UNKNOWN,
						Id:   "ext-123",
					},
				},
			},
			want: operation.ComponentTarget{
				External: &operation.ExternalRef{
					Type: devicetypes.ComponentTypeUnknown,
					ID:   "ext-123",
				},
			},
		},
		"external with empty ID": {
			input: &pb.ComponentTarget{
				Identifier: &pb.ComponentTarget_External{
					External: &pb.ExternalRef{
						Type: pb.ComponentType_COMPONENT_TYPE_COMPUTE,
						Id:   "",
					},
				},
			},
			wantErr: "external component id must not be empty",
		},
	}

	for name, tc := range testCases {
		t.Run(name, func(t *testing.T) {
			got, err := ComponentTargetFrom(tc.input)
			if tc.wantErr != "" {
				assert.ErrorContains(t, err, tc.wantErr)
				assert.Equal(t, operation.ComponentTarget{}, got)
				return
			}
			assert.NoError(t, err)
			assert.Equal(t, tc.want, got)
		})
	}
}

func TestNVLDomainTargetFrom(t *testing.T) {
	domainID := uuid.New()
	testCases := map[string]struct {
		input   *pb.NVLDomainTarget
		want    operation.NVLDomainTarget
		wantErr string
	}{
		"nil input": {
			wantErr: "NVLink domain target is nil",
		},
		"no identifier": {
			input:   &pb.NVLDomainTarget{},
			wantErr: "must have either id or name set",
		},
		"ID with filter": {
			input: &pb.NVLDomainTarget{
				Identifier: &pb.NVLDomainTarget_Id{Id: &pb.UUID{Id: domainID.String()}},
				ComponentTypes: []pb.ComponentType{
					pb.ComponentType_COMPONENT_TYPE_COMPUTE,
				},
			},
			want: operation.NVLDomainTarget{
				Identifier: identifier.Identifier{ID: domainID},
				ComponentTypes: []devicetypes.ComponentType{
					devicetypes.ComponentTypeCompute,
				},
			},
		},
		"name": {
			input: &pb.NVLDomainTarget{
				Identifier: &pb.NVLDomainTarget_Name{Name: "domain-1"},
			},
			want: operation.NVLDomainTarget{
				Identifier: identifier.Identifier{Name: "domain-1"},
			},
		},
		"invalid ID": {
			input: &pb.NVLDomainTarget{
				Identifier: &pb.NVLDomainTarget_Id{Id: &pb.UUID{Id: "invalid"}},
			},
			wantErr: "invalid NVLink domain id",
		},
		"unknown component type": {
			input: &pb.NVLDomainTarget{
				Identifier: &pb.NVLDomainTarget_Name{Name: "domain-1"},
				ComponentTypes: []pb.ComponentType{
					pb.ComponentType_COMPONENT_TYPE_UNKNOWN,
				},
			},
			wantErr: "unknown component type",
		},
	}

	for name, testCase := range testCases {
		t.Run(name, func(t *testing.T) {
			got, err := NVLDomainTargetFrom(testCase.input)
			if testCase.wantErr != "" {
				assert.ErrorContains(t, err, testCase.wantErr)
				return
			}
			assert.NoError(t, err)
			assert.Equal(t, testCase.want, got)
		})
	}
}

func TestTargetSpecTo(t *testing.T) {
	rackID := uuid.New()
	compID := uuid.New()

	testCases := map[string]struct {
		input   operation.TargetSpec
		wantErr string
		check   func(*testing.T, *pb.OperationTargetSpec)
	}{
		"multiple target kinds set": {
			input: operation.TargetSpec{
				Racks:      []operation.RackTarget{{Identifier: identifier.Identifier{Name: "rack-1"}}},
				Components: []operation.ComponentTarget{{UUID: compID}},
			},
			wantErr: "must have exactly one of racks, nvl_domains, or components",
		},
		"no target kind set": {
			input:   operation.TargetSpec{},
			wantErr: "must have exactly one of racks, nvl_domains, or components",
		},
		"rack target by name": {
			input: operation.TargetSpec{
				Racks: []operation.RackTarget{
					{Identifier: identifier.Identifier{Name: "rack-1"}},
				},
			},
		},
		"rack target by UUID": {
			input: operation.TargetSpec{
				Racks: []operation.RackTarget{
					{Identifier: identifier.Identifier{ID: rackID}},
				},
			},
		},
		"rack target by external ID": {
			input: operation.TargetSpec{
				Racks: []operation.RackTarget{
					{Identifier: identifier.Identifier{ExternalID: "rack-1"}},
				},
			},
			check: func(t *testing.T, got *pb.OperationTargetSpec) {
				t.Helper()
				targets := got.GetRacks().GetTargets()
				require.Len(t, targets, 1)
				assert.Equal(t, "rack-1", targets[0].GetExternalId())
				assert.Nil(t, targets[0].GetId())
			},
		},
		"component target by UUID": {
			input: operation.TargetSpec{
				Components: []operation.ComponentTarget{{UUID: compID}},
			},
		},
		"NVLink domain target by UUID": {
			input: operation.TargetSpec{
				NVLDomains: []operation.NVLDomainTarget{
					{
						Identifier: identifier.Identifier{ID: rackID},
						ComponentTypes: []devicetypes.ComponentType{
							devicetypes.ComponentTypeCompute,
						},
					},
				},
			},
		},
		"NVLink domain target with unmapped component type": {
			input: operation.TargetSpec{
				NVLDomains: []operation.NVLDomainTarget{
					{
						Identifier: identifier.Identifier{ID: rackID},
						ComponentTypes: []devicetypes.ComponentType{
							devicetypes.ComponentType(999),
						},
					},
				},
			},
			wantErr: "unknown component type filter",
		},
		"component target with no UUID and no external": {
			input: operation.TargetSpec{
				Components: []operation.ComponentTarget{{}},
			},
			wantErr: "invalid component target",
		},
		"rack target with neither id nor name": {
			input: operation.TargetSpec{
				Racks: []operation.RackTarget{
					{Identifier: identifier.Identifier{}}, // zero value: ID == uuid.Nil, Name == ""
				},
			},
			wantErr: "invalid rack target",
		},
	}

	for name, tc := range testCases {
		t.Run(name, func(t *testing.T) {
			got, err := TargetSpecTo(tc.input)
			if tc.wantErr != "" {
				assert.ErrorContains(t, err, tc.wantErr)
				assert.Nil(t, got)
				return
			}
			assert.NoError(t, err)
			assert.NotNil(t, got)
			if tc.check != nil {
				tc.check(t, got)
			}
		})
	}
}

func TestTargetSpecFromNVLDomains(t *testing.T) {
	domainID := uuid.New()
	testCases := map[string]struct {
		input   *pb.OperationTargetSpec
		want    operation.TargetSpec
		wantErr string
	}{
		"empty targets": {
			input: &pb.OperationTargetSpec{
				Targets: &pb.OperationTargetSpec_NvlDomains{
					NvlDomains: &pb.NVLDomainTargets{},
				},
			},
			wantErr: "nvl_domains.targets must have at least one entry",
		},
		"ID and name targets": {
			input: &pb.OperationTargetSpec{
				Targets: &pb.OperationTargetSpec_NvlDomains{
					NvlDomains: &pb.NVLDomainTargets{
						Targets: []*pb.NVLDomainTarget{
							{
								Identifier: &pb.NVLDomainTarget_Id{
									Id: &pb.UUID{Id: domainID.String()},
								},
							},
							{
								Identifier: &pb.NVLDomainTarget_Name{Name: "domain-2"},
							},
						},
					},
				},
			},
			want: operation.TargetSpec{
				NVLDomains: []operation.NVLDomainTarget{
					{Identifier: identifier.Identifier{ID: domainID}},
					{Identifier: identifier.Identifier{Name: "domain-2"}},
				},
			},
		},
	}

	for name, testCase := range testCases {
		t.Run(name, func(t *testing.T) {
			got, err := TargetSpecFrom(testCase.input)
			if testCase.wantErr != "" {
				assert.ErrorContains(t, err, testCase.wantErr)
				return
			}
			assert.NoError(t, err)
			assert.Equal(t, testCase.want, got)
		})
	}
}

func TestScheduledOperationFrom(t *testing.T) {
	rackTargetProto := &pb.OperationTargetSpec{
		Targets: &pb.OperationTargetSpec_Racks{
			Racks: &pb.RackTargets{
				Targets: []*pb.RackTarget{
					{Identifier: &pb.RackTarget_Name{Name: "rack-1"}},
				},
			},
		},
	}
	rackSpec := operation.TargetSpec{
		Racks: []operation.RackTarget{
			{Identifier: identifier.Identifier{Name: "rack-1"}},
		},
	}

	strPtr := func(s string) *string { return &s }

	startTime := time.Date(2026, 1, 1, 0, 0, 0, 0, time.UTC)
	endTime := time.Date(2026, 1, 2, 0, 0, 0, 0, time.UTC)

	testCases := map[string]struct {
		input        *pb.ScheduledOperation
		wantOp       operations.Operation
		wantTS       operation.TargetSpec
		wantQueueOpt *pb.QueueOptions
		wantRuleID   *pb.UUID
		wantErr      string
	}{
		"nil input": {
			input:   nil,
			wantErr: "operation is required",
		},
		"no operation set": {
			input:   &pb.ScheduledOperation{},
			wantErr: "operation is required",
		},
		"missing target_spec": {
			input: &pb.ScheduledOperation{
				Operation: &pb.ScheduledOperation_PowerOn{
					PowerOn: &pb.PowerOnRackRequest{TargetSpec: nil},
				},
			},
			wantErr: "invalid target_spec: target_spec is required",
		},
		"empty racks targets": {
			input: &pb.ScheduledOperation{
				Operation: &pb.ScheduledOperation_PowerOn{
					PowerOn: &pb.PowerOnRackRequest{
						TargetSpec: &pb.OperationTargetSpec{
							Targets: &pb.OperationTargetSpec_Racks{
								Racks: &pb.RackTargets{Targets: []*pb.RackTarget{}},
							},
						},
					},
				},
			},
			wantErr: "invalid target_spec: racks.targets must have at least one entry",
		},
		"empty components targets": {
			input: &pb.ScheduledOperation{
				Operation: &pb.ScheduledOperation_PowerOn{
					PowerOn: &pb.PowerOnRackRequest{
						TargetSpec: &pb.OperationTargetSpec{
							Targets: &pb.OperationTargetSpec_Components{
								Components: &pb.ComponentTargets{Targets: []*pb.ComponentTarget{}},
							},
						},
					},
				},
			},
			wantErr: "invalid target_spec: components.targets must have at least one entry",
		},
		"power_on": {
			input: &pb.ScheduledOperation{
				Operation: &pb.ScheduledOperation_PowerOn{
					PowerOn: &pb.PowerOnRackRequest{TargetSpec: rackTargetProto},
				},
			},
			wantOp: &operations.PowerControlTaskInfo{Operation: operations.PowerOperationPowerOn},
			wantTS: rackSpec,
		},
		"power_off unforced": {
			input: &pb.ScheduledOperation{
				Operation: &pb.ScheduledOperation_PowerOff{
					PowerOff: &pb.PowerOffRackRequest{TargetSpec: rackTargetProto, Forced: false},
				},
			},
			wantOp: &operations.PowerControlTaskInfo{Operation: operations.PowerOperationPowerOff},
			wantTS: rackSpec,
		},
		"power_off forced": {
			input: &pb.ScheduledOperation{
				Operation: &pb.ScheduledOperation_PowerOff{
					PowerOff: &pb.PowerOffRackRequest{TargetSpec: rackTargetProto, Forced: true},
				},
			},
			wantOp: &operations.PowerControlTaskInfo{
				Operation: operations.PowerOperationForcePowerOff,
				Forced:    true,
			},
			wantTS: rackSpec,
		},
		"power_reset unforced": {
			input: &pb.ScheduledOperation{
				Operation: &pb.ScheduledOperation_PowerReset{
					PowerReset: &pb.PowerResetRackRequest{TargetSpec: rackTargetProto, Forced: false},
				},
			},
			wantOp: &operations.PowerControlTaskInfo{Operation: operations.PowerOperationRestart},
			wantTS: rackSpec,
		},
		"power_reset forced": {
			input: &pb.ScheduledOperation{
				Operation: &pb.ScheduledOperation_PowerReset{
					PowerReset: &pb.PowerResetRackRequest{TargetSpec: rackTargetProto, Forced: true},
				},
			},
			wantOp: &operations.PowerControlTaskInfo{
				Operation: operations.PowerOperationForceRestart,
				Forced:    true,
			},
			wantTS: rackSpec,
		},
		"bring_up": {
			input: &pb.ScheduledOperation{
				Operation: &pb.ScheduledOperation_BringUp{
					BringUp: &pb.BringUpRackRequest{TargetSpec: rackTargetProto},
				},
			},
			wantOp: &operations.BringUpTaskInfo{},
			wantTS: rackSpec,
		},
		"upgrade_firmware": {
			input: &pb.ScheduledOperation{
				Operation: &pb.ScheduledOperation_UpgradeFirmware{
					UpgradeFirmware: &pb.UpgradeFirmwareRequest{
						TargetSpec:    rackTargetProto,
						TargetVersion: strPtr("v1.0.0"),
					},
				},
			},
			wantOp: &operations.FirmwareControlTaskInfo{
				Operation:     operations.FirmwareOperationUpgrade,
				TargetVersion: "v1.0.0",
			},
			wantTS: rackSpec,
		},
		"upgrade_firmware with start and end time": {
			input: &pb.ScheduledOperation{
				Operation: &pb.ScheduledOperation_UpgradeFirmware{
					UpgradeFirmware: &pb.UpgradeFirmwareRequest{
						TargetSpec:    rackTargetProto,
						TargetVersion: strPtr("v1.0.0"),
						StartTime:     timestamppb.New(startTime),
						EndTime:       timestamppb.New(endTime),
					},
				},
			},
			wantOp: &operations.FirmwareControlTaskInfo{
				Operation:     operations.FirmwareOperationUpgrade,
				TargetVersion: "v1.0.0",
				StartTime:     startTime.Unix(),
				EndTime:       endTime.Unix(),
			},
			wantTS: rackSpec,
		},
		"power_on with queue_options and rule_id": {
			input: &pb.ScheduledOperation{
				Operation: &pb.ScheduledOperation_PowerOn{
					PowerOn: &pb.PowerOnRackRequest{
						TargetSpec: rackTargetProto,
						QueueOptions: &pb.QueueOptions{
							ConflictStrategy:    pb.ConflictStrategy_CONFLICT_STRATEGY_QUEUE,
							QueueTimeoutSeconds: 60,
						},
						RuleId: &pb.UUID{Id: "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"},
					},
				},
			},
			wantOp: &operations.PowerControlTaskInfo{Operation: operations.PowerOperationPowerOn},
			wantTS: rackSpec,
			wantQueueOpt: &pb.QueueOptions{
				ConflictStrategy:    pb.ConflictStrategy_CONFLICT_STRATEGY_QUEUE,
				QueueTimeoutSeconds: 60,
			},
			wantRuleID: &pb.UUID{Id: "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"},
		},
		"bring_up with rule_id (no queue_options)": {
			input: &pb.ScheduledOperation{
				Operation: &pb.ScheduledOperation_BringUp{
					BringUp: &pb.BringUpRackRequest{
						TargetSpec: rackTargetProto,
						RuleId:     &pb.UUID{Id: "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"},
					},
				},
			},
			wantOp:       &operations.BringUpTaskInfo{},
			wantTS:       rackSpec,
			wantQueueOpt: nil,
			wantRuleID:   &pb.UUID{Id: "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"},
		},
		"ingest": {
			// Ingest is scheduled as BringUpTaskInfo{OpCode: "ingest"} internally;
			// it has no queue_options field, so wantQueueOpt is nil.
			input: &pb.ScheduledOperation{
				Operation: &pb.ScheduledOperation_Ingest{
					Ingest: &pb.IngestRackRequest{
						TargetSpec: rackTargetProto,
					},
				},
			},
			wantOp: &operations.BringUpTaskInfo{OpCode: taskcommon.OpCodeIngest},
			wantTS: rackSpec,
		},
		"ingest with rule_id": {
			input: &pb.ScheduledOperation{
				Operation: &pb.ScheduledOperation_Ingest{
					Ingest: &pb.IngestRackRequest{
						TargetSpec: rackTargetProto,
						RuleId:     &pb.UUID{Id: "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"},
					},
				},
			},
			wantOp:     &operations.BringUpTaskInfo{OpCode: taskcommon.OpCodeIngest},
			wantTS:     rackSpec,
			wantRuleID: &pb.UUID{Id: "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"},
		},
	}

	for name, tc := range testCases {
		t.Run(name, func(t *testing.T) {
			op, ts, queueOpt, ruleID, err := ScheduledOperationFrom(tc.input)
			if tc.wantErr != "" {
				assert.ErrorContains(t, err, tc.wantErr)
				assert.Nil(t, op)
				assert.Equal(t, operation.TargetSpec{}, ts)
				assert.Nil(t, queueOpt)
				assert.Nil(t, ruleID)
				return
			}
			assert.NoError(t, err)
			assert.Equal(t, tc.wantOp, op)
			assert.Equal(t, tc.wantTS, ts)
			assert.Equal(t, tc.wantQueueOpt, queueOpt)
			assert.Equal(t, tc.wantRuleID, ruleID)
		})
	}
}
