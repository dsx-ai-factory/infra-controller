// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package resolver

import (
	"context"
	"errors"
	"testing"

	identifier "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/Identifier"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/deviceinfo"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/location"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/component"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/rack"
	"github.com/google/uuid"
	"github.com/stretchr/testify/require"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"
)

func TestComponentByID(t *testing.T) {
	componentID := uuid.New()
	rackID := uuid.New()
	inventoryErr := errors.New("inventory unavailable")
	notFoundErr := status.Error(codes.NotFound, "component not found")
	tests := map[string]struct {
		id             uuid.UUID
		inventory      *fakeInventory
		wantID         uuid.UUID
		wantErr        error
		wantMessage    string
		retryableError bool
	}{
		"found": {
			id: componentID,
			inventory: &fakeInventory{component: testComponent(
				componentID,
				rackID,
				devicetypes.ComponentTypeCompute,
			)},
			wantID: componentID,
		},
		"missing ID": {
			inventory:   &fakeInventory{},
			wantErr:     ErrUnresolvable,
			wantMessage: "component id is required",
		},
		"inventory failure": {
			id:             componentID,
			inventory:      &fakeInventory{err: inventoryErr},
			wantErr:        inventoryErr,
			wantMessage:    "component id ",
			retryableError: true,
		},
		"store not found": {
			id:          componentID,
			inventory:   &fakeInventory{err: notFoundErr},
			wantErr:     ErrUnresolvable,
			wantMessage: "component id ",
		},
		"unknown component type": {
			id: componentID,
			inventory: &fakeInventory{component: testComponent(
				componentID,
				rackID,
				devicetypes.ComponentTypeUnknown,
			)},
			wantErr:     ErrUnresolvable,
			wantMessage: "has unknown type",
		},
		"mismatched canonical ID": {
			id: componentID,
			inventory: &fakeInventory{component: testComponent(
				uuid.New(),
				rackID,
				devicetypes.ComponentTypeCompute,
			)},
			wantErr:     ErrUnresolvable,
			wantMessage: "resolved to component id",
		},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			resolved, err := New(test.inventory).ComponentByID(
				context.Background(),
				test.id,
			)
			if test.wantErr != nil {
				require.ErrorIs(t, err, test.wantErr)
				require.ErrorContains(t, err, test.wantMessage)
				if test.retryableError {
					require.NotErrorIs(t, err, ErrUnresolvable)
				}
				return
			}

			require.NoError(t, err)
			require.Equal(t, test.wantID, resolved.Info.ID)
			require.Equal(t, test.id, test.inventory.componentID)
		})
	}
}

func TestComponentByExternalID(t *testing.T) {
	componentID := uuid.New()
	rackID := uuid.New()
	compute := testComponent(componentID, rackID, devicetypes.ComponentTypeCompute)
	nvswitch := testComponent(uuid.New(), rackID, devicetypes.ComponentTypeNVSwitch)
	inventoryErr := errors.New("inventory unavailable")

	tests := map[string]struct {
		externalID    string
		componentType devicetypes.ComponentType
		inventory     *fakeInventory
		expectedID    uuid.UUID
		wantErr       error
		wantMessage   string
	}{
		"typed": {
			externalID:    "component-1",
			componentType: devicetypes.ComponentTypeCompute,
			inventory:     &fakeInventory{components: []*component.Component{nvswitch, compute}},
			expectedID:    componentID,
		},
		"untyped unambiguous": {
			externalID: "component-1",
			inventory:  &fakeInventory{components: []*component.Component{compute}},
			expectedID: componentID,
		},
		"untyped ambiguous": {
			externalID: "component-1",
			inventory:  &fakeInventory{components: []*component.Component{compute, nvswitch}},
			wantErr:    ErrUnresolvable,
		},
		"missing external id": {
			inventory: &fakeInventory{},
			wantErr:   ErrUnresolvable,
		},
		"inventory failure": {
			externalID:  "component-1",
			inventory:   &fakeInventory{err: inventoryErr},
			wantErr:     inventoryErr,
			wantMessage: `component external id "component-1"`,
		},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			resolved, err := New(test.inventory).ComponentByExternalID(
				context.Background(),
				test.externalID,
				test.componentType,
			)
			if test.wantErr != nil {
				require.ErrorIs(t, err, test.wantErr)
				if test.wantMessage != "" {
					require.ErrorContains(t, err, test.wantMessage)
					require.NotErrorIs(t, err, ErrUnresolvable)
				}
				return
			}
			require.NoError(t, err)
			require.Equal(t, test.expectedID, resolved.Info.ID)
			require.Equal(t, []string{test.externalID}, test.inventory.externalIDs)
		})
	}
}

func TestRackByID(t *testing.T) {
	rackID := uuid.New()
	inventoryErr := errors.New("inventory unavailable")
	tests := map[string]struct {
		id             uuid.UUID
		withComponents bool
		inventory      *fakeInventory
		wantErr        error
		wantMessage    string
	}{
		"found": {
			id:             rackID,
			withComponents: true,
			inventory: &fakeInventory{
				rack: rack.New(deviceinfo.DeviceInfo{ID: rackID}, location.Location{}),
			},
		},
		"missing ID": {
			inventory: &fakeInventory{},
			wantErr:   ErrUnresolvable,
		},
		"reader returns nil rack": {
			id:          rackID,
			inventory:   &fakeInventory{},
			wantErr:     ErrUnresolvable,
			wantMessage: "has no canonical id",
		},
		"inventory failure": {
			id:          rackID,
			inventory:   &fakeInventory{err: inventoryErr},
			wantErr:     inventoryErr,
			wantMessage: "rack id ",
		},
		"mismatched canonical ID": {
			id: rackID,
			inventory: &fakeInventory{
				rack: rack.New(deviceinfo.DeviceInfo{ID: uuid.New()}, location.Location{}),
			},
			wantErr:     ErrUnresolvable,
			wantMessage: "resolved to rack id",
		},
		"component without ID": {
			id: rackID,
			inventory: &fakeInventory{
				rack: testRack(rackID, component.Component{}),
			},
			wantErr:     ErrUnresolvable,
			wantMessage: "component 0 id is required",
		},
		"duplicate component ID": {
			id: rackID,
			inventory: &fakeInventory{
				rack: testRack(
					rackID,
					component.Component{Info: deviceinfo.DeviceInfo{ID: rackID}},
					component.Component{Info: deviceinfo.DeviceInfo{ID: rackID}},
				),
			},
			wantErr:     ErrUnresolvable,
			wantMessage: "component 1 duplicates id",
		},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			resolved, err := New(test.inventory).RackByID(
				context.Background(),
				test.id,
				test.withComponents,
			)
			if test.wantErr != nil {
				require.ErrorIs(t, err, test.wantErr)
				if test.wantMessage != "" {
					require.ErrorContains(t, err, test.wantMessage)
				}
				return
			}

			require.NoError(t, err)
			require.Equal(t, rackID, resolved.Info.ID)
			require.Equal(t, identifier.Identifier{ID: rackID}, test.inventory.rackIdentifier)
			require.Equal(t, test.withComponents, test.inventory.withComponents)
		})
	}
}

func TestRackByName(t *testing.T) {
	rackID := uuid.New()
	inventoryErr := errors.New("inventory unavailable")
	notFoundErr := status.Error(codes.NotFound, "rack not found")
	tests := map[string]struct {
		name           string
		withComponents bool
		inventory      *fakeInventory
		wantErr        error
		wantMessage    string
	}{
		"found": {
			name: "rack-1",
			inventory: &fakeInventory{
				rack: rack.New(deviceinfo.DeviceInfo{ID: rackID}, location.Location{}),
			},
		},
		"missing name": {
			inventory: &fakeInventory{},
			wantErr:   ErrUnresolvable,
		},
		"inventory failure": {
			name:        "rack-1",
			inventory:   &fakeInventory{err: inventoryErr},
			wantErr:     inventoryErr,
			wantMessage: `rack name "rack-1"`,
		},
		"store not found": {
			name:        "rack-1",
			inventory:   &fakeInventory{err: notFoundErr},
			wantErr:     ErrUnresolvable,
			wantMessage: `rack name "rack-1"`,
		},
		"nil rack": {
			name:        "rack-1",
			inventory:   &fakeInventory{},
			wantErr:     ErrUnresolvable,
			wantMessage: "has no canonical id",
		},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			resolved, err := New(test.inventory).RackByName(
				context.Background(),
				test.name,
				test.withComponents,
			)
			if test.wantErr != nil {
				require.ErrorIs(t, err, test.wantErr)
				if test.wantMessage != "" {
					require.ErrorContains(t, err, test.wantMessage)
				}
				return
			}

			require.NoError(t, err)
			require.Equal(t, rackID, resolved.Info.ID)
			require.Equal(t, identifier.Identifier{Name: "rack-1"}, test.inventory.rackIdentifier)
			require.Equal(t, test.withComponents, test.inventory.withComponents)
		})
	}
}

func TestRackByExternalID(t *testing.T) {
	rackID := uuid.New()
	inventoryErr := errors.New("inventory unavailable")
	tests := map[string]struct {
		externalID     string
		withComponents bool
		inventory      *fakeInventory
		wantErr        error
		wantMessage    string
	}{
		"found": {
			externalID:     "D09",
			withComponents: true,
			inventory: &fakeInventory{
				rack: rack.New(deviceinfo.DeviceInfo{ID: rackID}, location.Location{}),
			},
		},
		"missing external ID": {
			inventory: &fakeInventory{},
			wantErr:   ErrUnresolvable,
		},
		"inventory failure": {
			externalID:  "D09",
			inventory:   &fakeInventory{err: inventoryErr},
			wantErr:     inventoryErr,
			wantMessage: `rack external id "D09"`,
		},
		"nil rack": {
			externalID:  "D09",
			inventory:   &fakeInventory{},
			wantErr:     ErrUnresolvable,
			wantMessage: "has no canonical id",
		},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			resolved, err := New(test.inventory).RackByExternalID(
				context.Background(),
				test.externalID,
				test.withComponents,
			)
			if test.wantErr != nil {
				require.ErrorIs(t, err, test.wantErr)
				if test.wantMessage != "" {
					require.ErrorContains(t, err, test.wantMessage)
				}
				return
			}

			require.NoError(t, err)
			require.Equal(t, rackID, resolved.Info.ID)
			require.Equal(
				t,
				identifier.Identifier{ExternalID: "D09"},
				test.inventory.rackIdentifier,
			)
			require.Equal(t, test.withComponents, test.inventory.withComponents)
		})
	}
}

func testComponent(
	id uuid.UUID,
	rackID uuid.UUID,
	componentType devicetypes.ComponentType,
) *component.Component {
	resolved := component.New(componentType, &deviceinfo.DeviceInfo{ID: id}, "", nil)
	resolved.RackID = rackID
	return &resolved
}

func testRack(id uuid.UUID, components ...component.Component) *rack.Rack {
	resolved := rack.New(deviceinfo.DeviceInfo{ID: id}, location.Location{})
	resolved.Components = components
	return resolved
}

type fakeInventory struct {
	component      *component.Component
	components     []*component.Component
	rack           *rack.Rack
	componentID    uuid.UUID
	externalIDs    []string
	rackIdentifier identifier.Identifier
	withComponents bool
	err            error
}

func (f *fakeInventory) GetComponentByID(
	_ context.Context,
	id uuid.UUID,
) (*component.Component, error) {
	f.componentID = id
	return f.component, f.err
}

func (f *fakeInventory) GetComponentsByExternalIDs(
	_ context.Context,
	externalIDs []string,
) ([]*component.Component, error) {
	f.externalIDs = externalIDs
	return f.components, f.err
}

func (f *fakeInventory) GetRackByIdentifier(
	_ context.Context,
	ref identifier.Identifier,
	withComponents bool,
) (*rack.Rack, error) {
	f.rackIdentifier = ref
	f.withComponents = withComponents
	return f.rack, f.err
}
