// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package resolver

import (
	"context"
	"testing"

	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"

	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/deviceinfo"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/component"
)

type componentIdentifierSource struct {
	byExternalID map[string][]*component.Component
	byMAC        map[string]*component.Component
	macLookups   int
}

func (s *componentIdentifierSource) GetComponentsByExternalIDs(
	_ context.Context,
	externalIDs []string,
) ([]*component.Component, error) {
	return s.byExternalID[externalIDs[0]], nil
}

func (s *componentIdentifierSource) GetComponentByBMCMAC(
	_ context.Context,
	macAddress string,
) (*component.Component, error) {
	s.macLookups++
	if comp := s.byMAC[macAddress]; comp != nil {
		return comp, nil
	}
	return nil, status.Error(codes.NotFound, "component not found")
}

func TestResolveComponentIdentifier(t *testing.T) {
	compute := &component.Component{
		Type:        devicetypes.ComponentTypeCompute,
		Info:        deviceinfo.DeviceInfo{ID: uuid.New()},
		ComponentID: "shared-id",
	}
	switchComponent := &component.Component{
		Type:        devicetypes.ComponentTypeNVSwitch,
		Info:        deviceinfo.DeviceInfo{ID: uuid.New()},
		ComponentID: "shared-id",
	}

	tests := []struct {
		name          string
		identifier    string
		componentType devicetypes.ComponentType
		want          *component.Component
		wantCode      codes.Code
		wantMACCalls  int
	}{
		{
			name:          "type disambiguates shared component ID",
			identifier:    "shared-id",
			componentType: devicetypes.ComponentTypeCompute,
			want:          compute,
		},
		{
			name:       "untyped shared component ID is ambiguous",
			identifier: "shared-id",
			wantCode:   codes.FailedPrecondition,
		},
		{
			name:         "MAC address resolves component",
			identifier:   "AA-BB-CC-DD-EE-FF",
			want:         switchComponent,
			wantMACCalls: 1,
		},
		{
			name:       "non-MAC identifier skips MAC lookup",
			identifier: "missing-id",
			wantCode:   codes.NotFound,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			source := &componentIdentifierSource{
				byExternalID: map[string][]*component.Component{
					"shared-id": {compute, switchComponent},
				},
				byMAC: map[string]*component.Component{
					"aa:bb:cc:dd:ee:ff": switchComponent,
				},
			}
			got, err := ResolveComponentIdentifier(
				context.Background(), source, tt.identifier, tt.componentType,
			)
			if tt.wantCode != codes.OK {
				require.Error(t, err)
				assert.Equal(t, tt.wantCode, status.Code(err))
			} else {
				require.NoError(t, err)
				assert.Same(t, tt.want, got)
			}
			assert.Equal(t, tt.wantMACCalls, source.macLookups)
		})
	}
}
