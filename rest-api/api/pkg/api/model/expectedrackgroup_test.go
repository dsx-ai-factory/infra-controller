// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	"testing"
)

func TestAPIExpectedRackGroupCreateRequestValidate(t *testing.T) {
	base := APIExpectedRackGroupCreateRequest{
		SiteID:      "550e8400-e29b-41d4-a716-446655440000",
		RackGroupID: "nvl5-gp1-jhb01",
		Topology:    "gb200-nvl72",
		RackIDs:     []string{"rack-01", "rack-02"},
	}

	tests := []struct {
		name    string
		mutate  func(*APIExpectedRackGroupCreateRequest)
		wantErr bool
	}{
		{name: "valid group"},
		{name: "empty rack declaration allowed", mutate: func(request *APIExpectedRackGroupCreateRequest) {
			request.RackIDs = nil
		}},
		{name: "duplicate rack rejected", mutate: func(request *APIExpectedRackGroupCreateRequest) {
			request.RackIDs = []string{"rack-01", "rack-01"}
		}, wantErr: true},
		{name: "devices independent of racks", mutate: func(request *APIExpectedRackGroupCreateRequest) {
			request.Members = cdbm.ExpectedRackGroupMembers{{Type: "switch", Manufacturer: "NVIDIA", ID: "rack-01"}}
		}},
		{name: "duplicate device rejected", mutate: func(request *APIExpectedRackGroupCreateRequest) {
			device := cdbm.ExpectedRackGroupMember{Type: "switch", Manufacturer: "NVIDIA", ID: "switch-01"}
			request.Members = cdbm.ExpectedRackGroupMembers{device, device}
		}, wantErr: true},
		{name: "incomplete device rejected", mutate: func(request *APIExpectedRackGroupCreateRequest) {
			request.Members = cdbm.ExpectedRackGroupMembers{{Type: "switch", ID: "switch-01"}}
		}, wantErr: true},
		{name: "empty rack id rejected", mutate: func(request *APIExpectedRackGroupCreateRequest) {
			request.RackIDs = []string{""}
		}, wantErr: true},
		{name: "empty description allowed", mutate: func(request *APIExpectedRackGroupCreateRequest) {
			value := ""
			request.Description = &value
		}},
		{name: "external group identifier required", mutate: func(request *APIExpectedRackGroupCreateRequest) {
			request.RackGroupID = ""
		}, wantErr: true},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			request := base
			request.RackIDs = append([]string(nil), base.RackIDs...)
			if test.mutate != nil {
				test.mutate(&request)
			}
			if gotErr := request.Validate() != nil; gotErr != test.wantErr {
				t.Fatalf("Validate() error = %v, want error %v", gotErr, test.wantErr)
			}
		})
	}
}

func TestAPIExpectedRackGroupUpdateRequestValidateMembership(t *testing.T) {
	tests := []struct {
		name    string
		rackIDs []string
		wantErr bool
	}{
		{name: "replace membership", rackIDs: []string{"rack-03"}},
		{name: "empty membership clears", rackIDs: []string{}},
		{name: "duplicate membership rejected", rackIDs: []string{"rack-03", "rack-03"}, wantErr: true},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			request := APIExpectedRackGroupUpdateRequest{RackIDs: test.rackIDs}
			if gotErr := request.Validate() != nil; gotErr != test.wantErr {
				t.Fatalf("Validate() error = %v, want error %v", gotErr, test.wantErr)
			}
		})
	}
}
