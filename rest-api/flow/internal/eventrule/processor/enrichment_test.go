// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package processor

import (
	"context"
	"errors"
	"testing"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule"
	identifier "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/Identifier"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/deviceinfo"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/location"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/component"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/rack"
	flowtypes "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/types"
	"github.com/google/uuid"
	"github.com/stretchr/testify/require"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"
)

func TestEnrichComponent(t *testing.T) {
	componentID := uuid.New()
	rackID := uuid.New()
	resolved := component.New(
		devicetypes.ComponentTypeCompute,
		&deviceinfo.DeviceInfo{ID: componentID},
		"",
		nil,
	)
	resolved.RackID = rackID
	processor := newTestProcessor(
		t,
		&processorInventory{
			components: []*component.Component{&resolved},
		},
		nil,
	)

	result, err := processor.enrich(
		context.Background(),
		validEnvelope(eventrule.Resource{
			Kind:       eventrule.ResourceKindComponent,
			ExternalID: "component-1",
		}),
	)
	require.NoError(t, err)
	require.Equal(t, componentID, result.ID)
	require.Equal(t, flowtypes.ComponentTypeCompute, result.ComponentType)
	require.Equal(t, rackID, result.RackID)
}

func TestEnrichClassifiesFailures(t *testing.T) {
	inventoryErr := errors.New("inventory unavailable")
	componentID := uuid.New()
	componentWithoutRack := component.New(
		devicetypes.ComponentTypeCompute,
		&deviceinfo.DeviceInfo{ID: componentID},
		"",
		nil,
	)
	componentWithInvalidType := component.New(
		devicetypes.ComponentType(100),
		&deviceinfo.DeviceInfo{ID: componentID},
		"",
		nil,
	)
	componentWithInvalidType.RackID = uuid.New()

	tests := map[string]struct {
		resource    eventrule.Resource
		inventory   *processorInventory
		wantErr     error
		wantMessage string
		notTerminal bool
	}{
		"missing identity is terminal": {
			resource:  eventrule.Resource{Kind: eventrule.ResourceKindComponent},
			inventory: &processorInventory{},
			wantErr:   ErrTerminal,
		},
		"inventory failure is retryable": {
			resource: eventrule.Resource{
				Kind:       eventrule.ResourceKindComponent,
				ExternalID: "component-1",
			},
			inventory:   &processorInventory{err: inventoryErr},
			wantErr:     inventoryErr,
			notTerminal: true,
		},
		"inventory not found is terminal": {
			resource: eventrule.Resource{
				Kind:       eventrule.ResourceKindComponent,
				ExternalID: "component-1",
			},
			inventory: &processorInventory{
				err: status.Error(codes.NotFound, "component not found"),
			},
			wantErr: ErrTerminal,
		},
		"component without rack is terminal": {
			resource: eventrule.Resource{
				Kind:       eventrule.ResourceKindComponent,
				ExternalID: "component-1",
			},
			inventory: &processorInventory{
				components: []*component.Component{&componentWithoutRack},
			},
			wantErr:     ErrTerminal,
			wantMessage: "has no resolved rack",
		},
		"invalid component type is terminal": {
			resource: eventrule.Resource{
				Kind:       eventrule.ResourceKindComponent,
				ExternalID: "component-1",
			},
			inventory: &processorInventory{
				components: []*component.Component{&componentWithInvalidType},
			},
			wantErr:     ErrTerminal,
			wantMessage: "unknown inventory component type",
		},
		"unsupported resource kind is terminal": {
			resource:    eventrule.Resource{Kind: eventrule.ResourceKind("unsupported")},
			inventory:   &processorInventory{},
			wantErr:     ErrTerminal,
			wantMessage: "unsupported resource kind",
		},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			processor := newTestProcessor(t, test.inventory, nil)
			_, err := processor.enrich(
				context.Background(),
				validEnvelope(test.resource),
			)
			require.ErrorIs(t, err, test.wantErr)
			if test.wantMessage != "" {
				require.ErrorContains(t, err, test.wantMessage)
			}
			if test.notTerminal {
				require.NotErrorIs(t, err, ErrTerminal)
			}
		})
	}
}

func TestEnrichRackUsesResolvedResourceAsRack(t *testing.T) {
	rackID := uuid.New()
	inventory := &processorInventory{
		rack: rack.New(deviceinfo.DeviceInfo{ID: rackID}, location.Location{}),
	}
	processor := newTestProcessor(
		t,
		inventory,
		nil,
	)

	result, err := processor.enrich(
		context.Background(),
		validEnvelope(eventrule.Resource{
			Kind:       eventrule.ResourceKindRack,
			ExternalID: "rack-1",
		}),
	)
	require.NoError(t, err)
	require.Equal(t, rackID, result.ID)
	require.Equal(t, rackID, result.RackID)
	require.Equal(
		t,
		identifier.Identifier{ExternalID: "rack-1"},
		inventory.rackRef,
	)
}

func validEnvelope(resource eventrule.Resource) eventrule.Envelope {
	return eventrule.Envelope{
		Key:      eventrule.EventKey{SourceName: "test", SourceKey: uuid.NewString()},
		Type:     "test.event",
		Resource: resource,
	}
}

type processorInventory struct {
	component  *component.Component
	components []*component.Component
	rack       *rack.Rack
	rackRef    identifier.Identifier
	err        error
}

func (f *processorInventory) GetComponentByID(
	context.Context,
	uuid.UUID,
) (*component.Component, error) {
	return f.component, f.err
}

func (f *processorInventory) GetComponentsByExternalIDs(
	context.Context,
	[]string,
) ([]*component.Component, error) {
	return f.components, f.err
}

func (f *processorInventory) GetRackByIdentifier(
	_ context.Context,
	reference identifier.Identifier,
	_ bool,
) (*rack.Rack, error) {
	f.rackRef = reference
	return f.rack, f.err
}
