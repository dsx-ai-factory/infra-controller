// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package target

import (
	"context"
	"fmt"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule"
)

type componentResolver struct{}

func (componentResolver) Resolve(
	_ context.Context,
	request ResolveRequest,
) ([]Target, error) {
	resource := request.Resource
	if resource.Kind != eventrule.ResourceKindComponent {
		return nil, fmt.Errorf(
			"%w: "+
				"component target strategy requires a component resource, got %q",
			ErrUnresolvable,
			resource.Kind,
		)
	}
	return []Target{{
		Kind:   eventrule.ResourceKindComponent,
		ID:     resource.ID,
		RackID: resource.RackID,
	}}, nil
}

type rackResolver struct{}

func (rackResolver) Resolve(
	_ context.Context,
	request ResolveRequest,
) ([]Target, error) {
	return []Target{{
		Kind:   eventrule.ResourceKindRack,
		ID:     request.Resource.RackID,
		RackID: request.Resource.RackID,
	}}, nil
}

type affectedComponentsResolver struct{}

func (affectedComponentsResolver) Resolve(
	ctx context.Context,
	request ResolveRequest,
) ([]Target, error) {
	switch request.Resource.Kind {
	case eventrule.ResourceKindComponent:
		return (componentResolver{}).Resolve(ctx, request)
	case eventrule.ResourceKindRack:
		return (rackResolver{}).Resolve(ctx, request)
	default:
		return nil, fmt.Errorf(
			"%w: "+
				"affected-components target strategy does not support resource kind %q",
			ErrUnresolvable,
			request.Resource.Kind,
		)
	}
}
