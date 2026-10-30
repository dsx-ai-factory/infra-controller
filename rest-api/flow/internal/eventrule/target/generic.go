// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package target

import (
	"context"
	"fmt"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule"
)

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

type sourceResolver struct{}

func (sourceResolver) Resolve(_ context.Context, request ResolveRequest) ([]Target, error) {
	resource := request.Resource
	switch resource.Kind {
	case eventrule.ResourceKindComponent, eventrule.ResourceKindRack:
		return []Target{{Kind: resource.Kind, ID: resource.ID, RackID: resource.RackID}}, nil
	default:
		return nil, fmt.Errorf("%w: source target strategy does not support resource kind %q", ErrUnresolvable, resource.Kind)
	}
}

type affectedComponentsResolver struct{}

func (affectedComponentsResolver) Resolve(ctx context.Context, request ResolveRequest) ([]Target, error) {
	return (sourceResolver{}).Resolve(ctx, request)
}
