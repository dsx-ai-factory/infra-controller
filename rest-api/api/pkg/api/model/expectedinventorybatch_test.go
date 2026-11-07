// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"testing"

	"github.com/stretchr/testify/assert"
)

const expectedInventoryBatchTestSiteID = "f97df110-f4de-492e-8849-4a6af68026b0"

func TestAPIReplaceAllExpectedMachinesRequest_Validate(t *testing.T) {
	valid := func(mac, serial string) *APIExpectedMachineCreateRequest {
		return &APIExpectedMachineCreateRequest{SiteID: expectedInventoryBatchTestSiteID, BmcMacAddress: mac, ChassisSerialNumber: serial}
	}
	tests := []struct {
		name    string
		request APIReplaceAllExpectedMachinesRequest
		wantErr bool
	}{
		{name: "empty replacement", request: APIReplaceAllExpectedMachinesRequest{SiteID: expectedInventoryBatchTestSiteID, ExpectedMachines: []*APIExpectedMachineCreateRequest{}}},
		{name: "missing replacement list", request: APIReplaceAllExpectedMachinesRequest{SiteID: expectedInventoryBatchTestSiteID}, wantErr: true},
		{name: "replacement list exceeds maximum", request: APIReplaceAllExpectedMachinesRequest{SiteID: expectedInventoryBatchTestSiteID, ExpectedMachines: make([]*APIExpectedMachineCreateRequest, ExpectedInventoryMaxReplaceItems+1)}, wantErr: true},
		{name: "valid replacement", request: APIReplaceAllExpectedMachinesRequest{SiteID: expectedInventoryBatchTestSiteID, ExpectedMachines: []*APIExpectedMachineCreateRequest{valid("00:1a:2b:3c:4d:50", "machine-1")}}},
		{name: "null entry", request: APIReplaceAllExpectedMachinesRequest{SiteID: expectedInventoryBatchTestSiteID, ExpectedMachines: []*APIExpectedMachineCreateRequest{nil}}, wantErr: true},
		{name: "mismatched site", request: APIReplaceAllExpectedMachinesRequest{SiteID: expectedInventoryBatchTestSiteID, ExpectedMachines: []*APIExpectedMachineCreateRequest{{SiteID: "497f6eca-6276-4993-bfeb-53cbbbba6f08", BmcMacAddress: "00:1a:2b:3c:4d:50", ChassisSerialNumber: "machine-1"}}}, wantErr: true},
		{name: "duplicate normalized MAC", request: APIReplaceAllExpectedMachinesRequest{SiteID: expectedInventoryBatchTestSiteID, ExpectedMachines: []*APIExpectedMachineCreateRequest{valid("00:1A:2B:3C:4D:50", "machine-1"), valid("00-1a-2b-3c-4d-50", "machine-2")}}, wantErr: true},
		{name: "duplicate case-insensitive serial", request: APIReplaceAllExpectedMachinesRequest{SiteID: expectedInventoryBatchTestSiteID, ExpectedMachines: []*APIExpectedMachineCreateRequest{valid("00:1a:2b:3c:4d:50", "MACHINE-1"), valid("00:1a:2b:3c:4d:51", "machine-1")}}, wantErr: true},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			assert.Equal(t, test.wantErr, test.request.Validate() != nil)
		})
	}
}

func TestAPIReplaceAllExpectedSwitchesRequest_Validate(t *testing.T) {
	valid := func(mac, serial string, nvos ...string) *APIExpectedSwitchCreateRequest {
		if len(nvos) == 0 {
			nvos = []string{mac}
		}
		interfaces := make(APINvosInterfaces, 0, len(nvos))
		for i, nvosMAC := range nvos {
			interfaces = append(interfaces, APINvosInterface{MacAddress: nvosMAC, IsPrimary: i == 0})
		}
		return &APIExpectedSwitchCreateRequest{SiteID: expectedInventoryBatchTestSiteID, BmcMacAddress: mac, SwitchSerialNumber: serial, NvosInterfaces: interfaces}
	}
	tests := []struct {
		name    string
		request APIReplaceAllExpectedSwitchesRequest
		wantErr bool
	}{
		{name: "empty replacement", request: APIReplaceAllExpectedSwitchesRequest{SiteID: expectedInventoryBatchTestSiteID, ExpectedSwitches: []*APIExpectedSwitchCreateRequest{}}},
		{name: "missing replacement list", request: APIReplaceAllExpectedSwitchesRequest{SiteID: expectedInventoryBatchTestSiteID}, wantErr: true},
		{name: "replacement list exceeds maximum", request: APIReplaceAllExpectedSwitchesRequest{SiteID: expectedInventoryBatchTestSiteID, ExpectedSwitches: make([]*APIExpectedSwitchCreateRequest, ExpectedInventoryMaxReplaceItems+1)}, wantErr: true},
		{name: "valid replacement", request: APIReplaceAllExpectedSwitchesRequest{SiteID: expectedInventoryBatchTestSiteID, ExpectedSwitches: []*APIExpectedSwitchCreateRequest{valid("00:1a:2b:3c:4d:60", "switch-1", "00:1a:2b:3c:4d:61")}}},
		{name: "duplicate normalized BMC MAC", request: APIReplaceAllExpectedSwitchesRequest{SiteID: expectedInventoryBatchTestSiteID, ExpectedSwitches: []*APIExpectedSwitchCreateRequest{valid("00:1A:2B:3C:4D:60", "switch-1"), valid("00-1a-2b-3c-4d-60", "switch-2")}}, wantErr: true},
		{name: "duplicate NVOS MAC across switches", request: APIReplaceAllExpectedSwitchesRequest{SiteID: expectedInventoryBatchTestSiteID, ExpectedSwitches: []*APIExpectedSwitchCreateRequest{valid("00:1a:2b:3c:4d:60", "switch-1", "00:1A:2B:3C:4D:70"), valid("00:1a:2b:3c:4d:61", "switch-2", "00-1a-2b-3c-4d-70")}}, wantErr: true},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			assert.Equal(t, test.wantErr, test.request.Validate() != nil)
		})
	}
}

func TestAPIReplaceAllExpectedPowerShelvesRequest_Validate(t *testing.T) {
	valid := func(mac, serial string) *APIExpectedPowerShelfCreateRequest {
		return &APIExpectedPowerShelfCreateRequest{SiteID: expectedInventoryBatchTestSiteID, BmcMacAddress: mac, ShelfSerialNumber: serial}
	}
	tests := []struct {
		name    string
		request APIReplaceAllExpectedPowerShelvesRequest
		wantErr bool
	}{
		{name: "empty replacement", request: APIReplaceAllExpectedPowerShelvesRequest{SiteID: expectedInventoryBatchTestSiteID, ExpectedPowerShelves: []*APIExpectedPowerShelfCreateRequest{}}},
		{name: "missing replacement list", request: APIReplaceAllExpectedPowerShelvesRequest{SiteID: expectedInventoryBatchTestSiteID}, wantErr: true},
		{name: "replacement list exceeds maximum", request: APIReplaceAllExpectedPowerShelvesRequest{SiteID: expectedInventoryBatchTestSiteID, ExpectedPowerShelves: make([]*APIExpectedPowerShelfCreateRequest, ExpectedInventoryMaxReplaceItems+1)}, wantErr: true},
		{name: "valid replacement", request: APIReplaceAllExpectedPowerShelvesRequest{SiteID: expectedInventoryBatchTestSiteID, ExpectedPowerShelves: []*APIExpectedPowerShelfCreateRequest{valid("00:1a:2b:3c:4d:70", "shelf-1")}}},
		{name: "duplicate normalized BMC MAC", request: APIReplaceAllExpectedPowerShelvesRequest{SiteID: expectedInventoryBatchTestSiteID, ExpectedPowerShelves: []*APIExpectedPowerShelfCreateRequest{valid("00:1A:2B:3C:4D:70", "shelf-1"), valid("00-1a-2b-3c-4d-70", "shelf-2")}}, wantErr: true},
		{name: "duplicate case-insensitive serial", request: APIReplaceAllExpectedPowerShelvesRequest{SiteID: expectedInventoryBatchTestSiteID, ExpectedPowerShelves: []*APIExpectedPowerShelfCreateRequest{valid("00:1a:2b:3c:4d:70", "SHELF-1"), valid("00:1a:2b:3c:4d:71", "shelf-1")}}, wantErr: true},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			assert.Equal(t, test.wantErr, test.request.Validate() != nil)
		})
	}
}
