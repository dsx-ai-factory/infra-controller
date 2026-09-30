// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"testing"

	"github.com/stretchr/testify/assert"
)

const expectedInventoryBulkTestSiteID = "f97df110-f4de-492e-8849-4a6af68026b0"

func TestAPIReplaceAllExpectedMachinesRequest_Validate(t *testing.T) {
	valid := func(mac, serial string) *APIExpectedMachineCreateRequest {
		return &APIExpectedMachineCreateRequest{SiteID: expectedInventoryBulkTestSiteID, BmcMacAddress: mac, ChassisSerialNumber: serial}
	}
	tests := []struct {
		name    string
		request APIReplaceAllExpectedMachinesRequest
		wantErr bool
	}{
		{name: "empty replacement", request: APIReplaceAllExpectedMachinesRequest{SiteID: expectedInventoryBulkTestSiteID, ExpectedMachines: []*APIExpectedMachineCreateRequest{}}},
		{name: "missing replacement list", request: APIReplaceAllExpectedMachinesRequest{SiteID: expectedInventoryBulkTestSiteID}, wantErr: true},
		{name: "replacement list exceeds maximum", request: APIReplaceAllExpectedMachinesRequest{SiteID: expectedInventoryBulkTestSiteID, ExpectedMachines: make([]*APIExpectedMachineCreateRequest, ExpectedInventoryMaxReplaceItems+1)}, wantErr: true},
		{name: "valid replacement", request: APIReplaceAllExpectedMachinesRequest{SiteID: expectedInventoryBulkTestSiteID, ExpectedMachines: []*APIExpectedMachineCreateRequest{valid("00:1a:2b:3c:4d:50", "machine-1")}}},
		{name: "null entry", request: APIReplaceAllExpectedMachinesRequest{SiteID: expectedInventoryBulkTestSiteID, ExpectedMachines: []*APIExpectedMachineCreateRequest{nil}}, wantErr: true},
		{name: "mismatched site", request: APIReplaceAllExpectedMachinesRequest{SiteID: expectedInventoryBulkTestSiteID, ExpectedMachines: []*APIExpectedMachineCreateRequest{{SiteID: "497f6eca-6276-4993-bfeb-53cbbbba6f08", BmcMacAddress: "00:1a:2b:3c:4d:50", ChassisSerialNumber: "machine-1"}}}, wantErr: true},
		{name: "duplicate normalized MAC", request: APIReplaceAllExpectedMachinesRequest{SiteID: expectedInventoryBulkTestSiteID, ExpectedMachines: []*APIExpectedMachineCreateRequest{valid("00:1A:2B:3C:4D:50", "machine-1"), valid("00-1a-2b-3c-4d-50", "machine-2")}}, wantErr: true},
		{name: "duplicate case-insensitive serial", request: APIReplaceAllExpectedMachinesRequest{SiteID: expectedInventoryBulkTestSiteID, ExpectedMachines: []*APIExpectedMachineCreateRequest{valid("00:1a:2b:3c:4d:50", "MACHINE-1"), valid("00:1a:2b:3c:4d:51", "machine-1")}}, wantErr: true},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			assert.Equal(t, test.wantErr, test.request.Validate() != nil)
		})
	}
}

func TestAPIReplaceAllExpectedSwitchesRequest_Validate(t *testing.T) {
	valid := func(mac, serial string, nvos ...string) *APIExpectedSwitchCreateRequest {
		return &APIExpectedSwitchCreateRequest{SiteID: expectedInventoryBulkTestSiteID, BmcMacAddress: mac, SwitchSerialNumber: serial, NvosMacAddresses: nvos}
	}
	tests := []struct {
		name    string
		request APIReplaceAllExpectedSwitchesRequest
		wantErr bool
	}{
		{name: "empty replacement", request: APIReplaceAllExpectedSwitchesRequest{SiteID: expectedInventoryBulkTestSiteID, ExpectedSwitches: []*APIExpectedSwitchCreateRequest{}}},
		{name: "missing replacement list", request: APIReplaceAllExpectedSwitchesRequest{SiteID: expectedInventoryBulkTestSiteID}, wantErr: true},
		{name: "replacement list exceeds maximum", request: APIReplaceAllExpectedSwitchesRequest{SiteID: expectedInventoryBulkTestSiteID, ExpectedSwitches: make([]*APIExpectedSwitchCreateRequest, ExpectedInventoryMaxReplaceItems+1)}, wantErr: true},
		{name: "valid replacement", request: APIReplaceAllExpectedSwitchesRequest{SiteID: expectedInventoryBulkTestSiteID, ExpectedSwitches: []*APIExpectedSwitchCreateRequest{valid("00:1a:2b:3c:4d:60", "switch-1", "00:1a:2b:3c:4d:61")}}},
		{name: "duplicate normalized BMC MAC", request: APIReplaceAllExpectedSwitchesRequest{SiteID: expectedInventoryBulkTestSiteID, ExpectedSwitches: []*APIExpectedSwitchCreateRequest{valid("00:1A:2B:3C:4D:60", "switch-1"), valid("00-1a-2b-3c-4d-60", "switch-2")}}, wantErr: true},
		{name: "duplicate NVOS MAC across switches", request: APIReplaceAllExpectedSwitchesRequest{SiteID: expectedInventoryBulkTestSiteID, ExpectedSwitches: []*APIExpectedSwitchCreateRequest{valid("00:1a:2b:3c:4d:60", "switch-1", "00:1A:2B:3C:4D:70"), valid("00:1a:2b:3c:4d:61", "switch-2", "00-1a-2b-3c-4d-70")}}, wantErr: true},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			assert.Equal(t, test.wantErr, test.request.Validate() != nil)
		})
	}
}

func TestAPIReplaceAllExpectedPowerShelvesRequest_Validate(t *testing.T) {
	valid := func(mac, serial string) *APIExpectedPowerShelfCreateRequest {
		return &APIExpectedPowerShelfCreateRequest{SiteID: expectedInventoryBulkTestSiteID, BmcMacAddress: mac, ShelfSerialNumber: serial}
	}
	tests := []struct {
		name    string
		request APIReplaceAllExpectedPowerShelvesRequest
		wantErr bool
	}{
		{name: "empty replacement", request: APIReplaceAllExpectedPowerShelvesRequest{SiteID: expectedInventoryBulkTestSiteID, ExpectedPowerShelves: []*APIExpectedPowerShelfCreateRequest{}}},
		{name: "missing replacement list", request: APIReplaceAllExpectedPowerShelvesRequest{SiteID: expectedInventoryBulkTestSiteID}, wantErr: true},
		{name: "replacement list exceeds maximum", request: APIReplaceAllExpectedPowerShelvesRequest{SiteID: expectedInventoryBulkTestSiteID, ExpectedPowerShelves: make([]*APIExpectedPowerShelfCreateRequest, ExpectedInventoryMaxReplaceItems+1)}, wantErr: true},
		{name: "valid replacement", request: APIReplaceAllExpectedPowerShelvesRequest{SiteID: expectedInventoryBulkTestSiteID, ExpectedPowerShelves: []*APIExpectedPowerShelfCreateRequest{valid("00:1a:2b:3c:4d:70", "shelf-1")}}},
		{name: "duplicate normalized BMC MAC", request: APIReplaceAllExpectedPowerShelvesRequest{SiteID: expectedInventoryBulkTestSiteID, ExpectedPowerShelves: []*APIExpectedPowerShelfCreateRequest{valid("00:1A:2B:3C:4D:70", "shelf-1"), valid("00-1a-2b-3c-4d-70", "shelf-2")}}, wantErr: true},
		{name: "duplicate case-insensitive serial", request: APIReplaceAllExpectedPowerShelvesRequest{SiteID: expectedInventoryBulkTestSiteID, ExpectedPowerShelves: []*APIExpectedPowerShelfCreateRequest{valid("00:1a:2b:3c:4d:70", "SHELF-1"), valid("00:1a:2b:3c:4d:71", "shelf-1")}}, wantErr: true},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			assert.Equal(t, test.wantErr, test.request.Validate() != nil)
		})
	}
}
