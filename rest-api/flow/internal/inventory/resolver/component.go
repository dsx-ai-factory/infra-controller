// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package resolver

import (
	"context"
	"net"

	"github.com/google/uuid"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"

	flowutils "github.com/NVIDIA/infra-controller/rest-api/flow/internal/common/utils"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/component"
)

// ComponentIdentifierSource resolves components by their external identifiers.
type ComponentIdentifierSource interface {
	GetComponentByBMCMAC(ctx context.Context, macAddress string) (*component.Component, error)
	GetComponentsByExternalIDs(ctx context.Context, externalIDs []string) ([]*component.Component, error)
}

// ResolveComponentIdentifier resolves a component ID or BMC MAC address. An
// unknown component type permits any type, but the result must remain unique.
func ResolveComponentIdentifier(
	ctx context.Context,
	source ComponentIdentifierSource,
	identifier string,
	componentType devicetypes.ComponentType,
) (*component.Component, error) {
	if identifier == "" {
		return nil, status.Error(codes.InvalidArgument, "component identifier is required")
	}

	candidates := make(map[uuid.UUID]*component.Component)
	byExternalID, err := source.GetComponentsByExternalIDs(ctx, []string{identifier})
	if err != nil {
		return nil, err
	}
	for _, candidate := range byExternalID {
		if matchesComponentType(candidate, componentType) {
			candidates[candidate.Info.ID] = candidate
		}
	}

	if _, parseErr := net.ParseMAC(identifier); parseErr == nil {
		byMAC, err := source.GetComponentByBMCMAC(ctx, flowutils.NormalizeMAC(identifier))
		if err == nil && matchesComponentType(byMAC, componentType) {
			candidates[byMAC.Info.ID] = byMAC
		} else if err != nil && status.Code(err) != codes.NotFound {
			return nil, err
		}
	}

	switch len(candidates) {
	case 0:
		return nil, status.Errorf(codes.NotFound, "component identifier %q not found", identifier)
	case 1:
		for _, candidate := range candidates {
			return candidate, nil
		}
	default:
		return nil, status.Errorf(codes.FailedPrecondition, "component identifier %q is ambiguous", identifier)
	}

	return nil, status.Error(codes.Internal, "component identifier resolution produced no result")
}

func matchesComponentType(candidate *component.Component, componentType devicetypes.ComponentType) bool {
	return candidate != nil && candidate.Info.ID != uuid.Nil &&
		(componentType == devicetypes.ComponentTypeUnknown || candidate.Type == componentType)
}
