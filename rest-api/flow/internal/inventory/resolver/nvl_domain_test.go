// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package resolver

import (
	"context"
	"errors"
	"testing"

	"github.com/google/uuid"
	"github.com/stretchr/testify/require"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/operation"
	identifier "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/Identifier"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/deviceinfo"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/rack"
)

func TestResolveNVLDomainRackTargets(t *testing.T) {
	domainOneID := uuid.New()
	domainTwoID := uuid.New()
	rackOneID := uuid.New()
	rackTwoID := uuid.New()
	lookupErr := errors.New("lookup failed")

	tests := []struct {
		name        string
		domains     []operation.NVLDomainTarget
		inventory   *fakeNVLDomainRackReader
		want        []operation.RackTarget
		wantErr     string
		wantLookups []identifier.Identifier
	}{
		{
			name: "expands mixed external and legacy IDs and preserves component filters",
			domains: []operation.NVLDomainTarget{
				{
					Identifier:     identifier.Identifier{ExternalID: "rack-01"},
					ComponentTypes: []devicetypes.ComponentType{devicetypes.ComponentTypeCompute},
				},
				{Identifier: identifier.Identifier{ID: domainTwoID}},
			},
			inventory: &fakeNVLDomainRackReader{
				racksByDomain: map[identifier.Identifier][]*rack.Rack{
					{ExternalID: "rack-01"}: {{Info: deviceinfo.DeviceInfo{ID: rackOneID}}},
					{ID: domainTwoID}:       {{Info: deviceinfo.DeviceInfo{ID: rackTwoID}}},
				},
			},
			want: []operation.RackTarget{
				{
					Identifier:     identifier.Identifier{ID: rackOneID},
					ComponentTypes: []devicetypes.ComponentType{devicetypes.ComponentTypeCompute},
				},
				{Identifier: identifier.Identifier{ID: rackTwoID}},
			},
			wantLookups: []identifier.Identifier{{ExternalID: "rack-01"}, {ID: domainTwoID}},
		},
		{
			name: "reports lookup failure with domain index",
			domains: []operation.NVLDomainTarget{
				{Identifier: identifier.Identifier{ID: domainOneID}},
				{Identifier: identifier.Identifier{ID: domainTwoID}},
			},
			inventory: &fakeNVLDomainRackReader{
				racksByDomain: map[identifier.Identifier][]*rack.Rack{
					{ID: domainOneID}: {{Info: deviceinfo.DeviceInfo{ID: rackOneID}}},
				},
				errorsByDomain: map[identifier.Identifier]error{{ID: domainTwoID}: lookupErr},
			},
			wantErr:     "NVLink domain target 1: lookup failed",
			wantLookups: []identifier.Identifier{{ID: domainOneID}, {ID: domainTwoID}},
		},
		{
			name:    "rejects nil rack",
			domains: []operation.NVLDomainTarget{{Identifier: identifier.Identifier{ID: domainOneID}}},
			inventory: &fakeNVLDomainRackReader{
				racksByDomain: map[identifier.Identifier][]*rack.Rack{{ID: domainOneID}: {nil}},
			},
			wantErr:     "NVLink domain target 0 rack 0 has no ID",
			wantLookups: []identifier.Identifier{{ID: domainOneID}},
		},
		{
			name:    "rejects rack without ID",
			domains: []operation.NVLDomainTarget{{Identifier: identifier.Identifier{ID: domainOneID}}},
			inventory: &fakeNVLDomainRackReader{
				racksByDomain: map[identifier.Identifier][]*rack.Rack{{ID: domainOneID}: {{}}},
			},
			wantErr:     "NVLink domain target 0 rack 0 has no ID",
			wantLookups: []identifier.Identifier{{ID: domainOneID}},
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			got, err := ResolveNVLDomainRackTargets(
				context.Background(),
				test.inventory,
				test.domains,
			)
			if test.wantErr != "" {
				require.EqualError(t, err, test.wantErr)
				require.Nil(t, got)
			} else {
				require.NoError(t, err)
				require.Equal(t, test.want, got)
			}
			require.Equal(t, test.wantLookups, test.inventory.lookups)
		})
	}
}

type fakeNVLDomainRackReader struct {
	racksByDomain  map[identifier.Identifier][]*rack.Rack
	errorsByDomain map[identifier.Identifier]error
	lookups        []identifier.Identifier
}

func (f *fakeNVLDomainRackReader) GetRacksForNVLDomain(
	_ context.Context,
	domain identifier.Identifier,
	_ bool,
) ([]*rack.Rack, error) {
	f.lookups = append(f.lookups, domain)
	return f.racksByDomain[domain], f.errorsByDomain[domain]
}
