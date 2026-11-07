// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package resolver

import (
	"context"
	"fmt"

	"github.com/google/uuid"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/operation"
	identifier "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/Identifier"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/rack"
)

// NVLDomainRackReader provides the inventory lookup needed to expand NVLink
// domain targets into rack targets.
type NVLDomainRackReader interface {
	GetRacksForNVLDomain(context.Context, identifier.Identifier, bool) ([]*rack.Rack, error)
}

// ResolveNVLDomainRackTargets expands NVLink domain targets into rack targets
// while validating that every returned rack has a canonical Flow ID.
func ResolveNVLDomainRackTargets(
	ctx context.Context,
	inventory NVLDomainRackReader,
	domains []operation.NVLDomainTarget,
) ([]operation.RackTarget, error) {
	rackTargets := make([]operation.RackTarget, 0)
	for domainIndex, domain := range domains {
		domainRacks, err := inventory.GetRacksForNVLDomain(ctx, domain.Identifier, true)
		if err != nil {
			return nil, fmt.Errorf(
				"NVLink domain target %d: %w",
				domainIndex,
				err,
			)
		}

		for rackIndex, domainRack := range domainRacks {
			if domainRack == nil || domainRack.Info.ID == uuid.Nil {
				return nil, fmt.Errorf(
					"NVLink domain target %d rack %d has no ID",
					domainIndex,
					rackIndex,
				)
			}

			rackTargets = append(rackTargets, operation.RackTarget{
				Identifier:     identifier.Identifier{ID: domainRack.Info.ID},
				ComponentTypes: domain.ComponentTypes,
			})
		}
	}

	return rackTargets, nil
}
