// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"encoding/json"
	"strings"
	"testing"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	validation "github.com/go-ozzo/ozzo-validation/v4"
	"github.com/stretchr/testify/require"
)

const rackGroupTestSiteID = "550e8400-e29b-41d4-a716-446655440000"

type rackGroupValidationCase struct {
	name    string
	request APIExpectedRackGroupUpdateRequest
	wantErr bool
}

// Both public write DTOs must enforce the same Core metadata contract.
func rackGroupValidationCases() []rackGroupValidationCase {
	device := APIExpectedRackGroupMember{Type: "NVSwitch", Manufacturer: "NVIDIA", ID: "rack-01"}
	return []rackGroupValidationCase{
		{"device in two racks", APIExpectedRackGroupUpdateRequest{Racks: []APIExpectedRackGroupRack{{RackID: "rack-01", Members: []APIExpectedRackGroupMember{device}}, {RackID: "rack-02", Members: []APIExpectedRackGroupMember{device}}}}, true},
		{"replace racks", APIExpectedRackGroupUpdateRequest{Racks: []APIExpectedRackGroupRack{{RackID: "rack-01"}, {RackID: "rack-02"}}}, false},
		{"clear racks", APIExpectedRackGroupUpdateRequest{Racks: []APIExpectedRackGroupRack{}}, false},
		{"duplicate rack", APIExpectedRackGroupUpdateRequest{Racks: []APIExpectedRackGroupRack{{RackID: "rack-01"}, {RackID: "rack-01"}}}, true},
		{"blank rack", APIExpectedRackGroupUpdateRequest{Racks: []APIExpectedRackGroupRack{{RackID: " "}}}, true},
		{"device belongs to rack", APIExpectedRackGroupUpdateRequest{Racks: []APIExpectedRackGroupRack{{RackID: "rack-01", Members: []APIExpectedRackGroupMember{device}}}}, false},
		{"duplicate device", APIExpectedRackGroupUpdateRequest{Racks: []APIExpectedRackGroupRack{{RackID: "rack-01", Members: []APIExpectedRackGroupMember{device, device}}}}, true},
		{"incomplete device", APIExpectedRackGroupUpdateRequest{Racks: []APIExpectedRackGroupRack{{RackID: "rack-01", Members: []APIExpectedRackGroupMember{{Type: "NVSwitch", ID: "switch-01"}}}}}, true},
		{"Core spelling rejected", APIExpectedRackGroupUpdateRequest{Racks: []APIExpectedRackGroupRack{{RackID: "rack-01", Members: []APIExpectedRackGroupMember{{Type: "Switch", Manufacturer: "NVIDIA", ID: "switch-01"}}}}}, true},
		{"uppercase spelling rejected", APIExpectedRackGroupUpdateRequest{Racks: []APIExpectedRackGroupRack{{RackID: "rack-01", Members: []APIExpectedRackGroupMember{{Type: "NVSWITCH", Manufacturer: "NVIDIA", ID: "switch-01"}}}}}, true},
		{"name boundary", APIExpectedRackGroupUpdateRequest{Name: cutil.GetPtr(strings.Repeat("n", 256))}, false},
		{"name too long", APIExpectedRackGroupUpdateRequest{Name: cutil.GetPtr(strings.Repeat("n", 257))}, true},
		{"non-ASCII name", APIExpectedRackGroupUpdateRequest{Name: cutil.GetPtr("机架")}, true},
		{"clear metadata", APIExpectedRackGroupUpdateRequest{Name: cutil.GetPtr(""), Description: cutil.GetPtr(""), Labels: map[string]string{}}, false},
		{"description UTF-8 byte boundary", APIExpectedRackGroupUpdateRequest{Description: cutil.GetPtr(strings.Repeat("é", 512))}, false},
		{"description one byte too long", APIExpectedRackGroupUpdateRequest{Description: cutil.GetPtr(strings.Repeat("é", 512) + "x")}, true},
		{"Unicode label values allowed", APIExpectedRackGroupUpdateRequest{Labels: map[string]string{"location.room": "机房"}}, false},
		{"non-ASCII label key", APIExpectedRackGroupUpdateRequest{Labels: map[string]string{"位置": "A1"}}, true},
		{"label byte limit retained", APIExpectedRackGroupUpdateRequest{Labels: map[string]string{"location.room": strings.Repeat("é", 128)}}, true},
	}
}

func TestAPIExpectedRackGroupCreateRequest_Validate(t *testing.T) {
	cases := append(rackGroupValidationCases(),
		rackGroupValidationCase{name: "omitted optional fields"},
		rackGroupValidationCase{name: "blank identity", request: APIExpectedRackGroupUpdateRequest{RackGroupID: cutil.GetPtr(" ")}, wantErr: true},
	)
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			request := APIExpectedRackGroupCreateRequest{
				SiteID: rackGroupTestSiteID, RackGroupID: "group", Topology: "gb200_nvl72r1_c2g4",
				Racks: tc.request.Racks,
				Name:  tc.request.Name, Description: tc.request.Description, Labels: tc.request.Labels,
			}
			if tc.request.RackGroupID != nil {
				request.RackGroupID = *tc.request.RackGroupID
			}
			err := request.Validate()
			if tc.wantErr {
				require.Error(t, err)
				require.IsType(t, validation.Errors{}, err)
			} else {
				require.NoError(t, err)
			}
		})
	}
}

func TestAPIExpectedRackGroupUpdateRequest_Validate(t *testing.T) {
	cases := append(rackGroupValidationCases(),
		rackGroupValidationCase{name: "empty update", wantErr: true},
		rackGroupValidationCase{name: "replace topology", request: APIExpectedRackGroupUpdateRequest{Topology: cutil.GetPtr("gb300_nvl72r1_c2g4")}},
		rackGroupValidationCase{name: "blank topology", request: APIExpectedRackGroupUpdateRequest{Topology: cutil.GetPtr(" ")}, wantErr: true},
	)
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			err := tc.request.Validate()
			if tc.wantErr {
				require.Error(t, err)
				require.IsType(t, validation.Errors{}, err)
			} else {
				require.NoError(t, err)
			}
		})
	}
}

func TestAPIReplaceAllExpectedRackGroupsRequest_Validate(t *testing.T) {
	for _, tc := range []struct {
		name    string
		body    string
		wantErr bool
	}{
		{"omitted array", "{}", true},
		{"null array", `{"expectedRackGroups":null}`, true},
		{"explicit empty clears", `{"expectedRackGroups":[]}`, false},
		{"null entry", `{"expectedRackGroups":[null]}`, true},
		{"valid entry", `{"expectedRackGroups":[{"siteId":"550e8400-e29b-41d4-a716-446655440000","rackGroupId":"group","topology":"gb200_nvl72r1_c2g4"}]}`, false},
		{"other site", `{"expectedRackGroups":[{"siteId":"550e8400-e29b-41d4-a716-446655440001","rackGroupId":"group","topology":"gb200_nvl72r1_c2g4"}]}`, true},
		{"uppercase site rejected by UUID validation", `{"siteId":"550E8400-E29B-41D4-A716-446655440000","expectedRackGroups":[{"siteId":"550e8400-e29b-41d4-a716-446655440000","rackGroupId":"group","topology":"t"}]}`, true},
		{"uppercase entry site rejected by UUID validation", `{"expectedRackGroups":[{"siteId":"550E8400-E29B-41D4-A716-446655440000","rackGroupId":"group","topology":"t"}]}`, true},
		{"duplicate group", `{"expectedRackGroups":[{"siteId":"550e8400-e29b-41d4-a716-446655440000","rackGroupId":"group","topology":"t"},{"siteId":"550e8400-e29b-41d4-a716-446655440000","rackGroupId":"group","topology":"t"}]}`, true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			request := APIReplaceAllExpectedRackGroupsRequest{SiteID: rackGroupTestSiteID}
			require.NoError(t, json.Unmarshal([]byte(tc.body), &request))
			err := request.Validate()
			if tc.wantErr {
				require.Error(t, err)
				require.IsType(t, validation.Errors{}, err)
			} else {
				require.NoError(t, err)
			}
		})
	}
}

func TestAPIExpectedRackGroupRequest_UnmarshalJSON(t *testing.T) {
	for _, tc := range []struct {
		name, body       string
		wantErr, present bool
	}{
		{"omitted racks", `{"name":"renamed"}`, false, false},
		{"null racks", `{"racks":null}`, false, false},
		{"empty racks", `{"racks":[]}`, false, true},
		{"nested membership", `{"racks":[{"rackId":"rack-01","members":[{"type":"NVSwitch","manufacturer":"NVIDIA","id":"switch-01"}]}]}`, false, true},
		{"legacy rack IDs", `{"rackIds":["rack-01"]}`, true, false},
		{"legacy devices", `{"members":[{"type":"NVSwitch","manufacturer":"NVIDIA","id":"switch-01"}]}`, true, false},
		{"misspelled rack field", `{"racks":[{"rackIds":["rack-01"]}]}`, true, false},
	} {
		t.Run(tc.name, func(t *testing.T) {
			var create APIExpectedRackGroupCreateRequest
			var update APIExpectedRackGroupUpdateRequest
			for _, target := range []interface{}{&create, &update} {
				err := json.Unmarshal([]byte(tc.body), target)
				if tc.wantErr {
					require.Error(t, err)
				} else {
					require.NoError(t, err)
				}
			}
			if !tc.wantErr {
				require.Equal(t, tc.present, create.Racks != nil)
				require.Equal(t, tc.present, update.Racks != nil)
				require.Equal(t, create.Racks, update.Racks)
			}
		})
	}
}
