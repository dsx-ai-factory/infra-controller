// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package service

import (
	"context"
	"fmt"

	"github.com/google/uuid"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/converter/protobuf"
	dbquery "github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/query"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/component"
	pb "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/proto/v1"
)

// validationCandidates preserves the distinction between parent rack filters
// and component filters. A resolved empty selection stays empty.
func (rs *FlowServerImpl) validationCandidates(
	ctx context.Context,
	targetSpec *pb.OperationTargetSpec,
	rackFilters []*pb.Filter,
) ([]*component.Component, error) {
	var candidates []*component.Component
	if targetSpec != nil {
		var err error
		candidates, err = rs.extractComponentsFromTargetSpec(ctx, targetSpec)
		if err != nil {
			return nil, err
		}
	}

	if len(rackFilters) > 0 {
		var name dbquery.StringQueryInfo
		var manufacturer, model *dbquery.StringQueryInfo
		for _, filter := range rackFilters {
			field, info, err := protobuf.FilterFrom(filter)
			if err != nil {
				return nil, err
			}
			if info == nil {
				continue
			}
			switch field {
			case "name":
				name = *info
			case "manufacturer":
				manufacturer = info
			case "description->>'model'":
				model = info
			default:
				return nil, fmt.Errorf("unsupported rack filter field: %s", field)
			}
		}

		var rackComponents []*component.Component
		// Selection must include every matching rack, independently of the
		// response's drift pagination and the inventory query's default limit.
		page := dbquery.DefaultPagination()
		for {
			racks, total, err := rs.inventoryManager.GetListOfRacks(ctx, name, manufacturer, model, page, nil, true, false)
			if err != nil {
				return nil, err
			}
			for _, r := range racks {
				rackComponents = append(rackComponents, extractComponentsByTypes(r, nil)...)
			}
			page.Offset += len(racks)
			if len(racks) == 0 || page.Offset >= int(total) {
				break
			}
		}
		if targetSpec == nil {
			return rackComponents, nil
		}
		allowed := make(map[uuid.UUID]struct{}, len(rackComponents))
		for _, comp := range rackComponents {
			allowed[comp.Info.ID] = struct{}{}
		}
		selected := make([]*component.Component, 0, len(candidates))
		for _, comp := range candidates {
			if _, ok := allowed[comp.Info.ID]; ok {
				selected = append(selected, comp)
			}
		}
		return selected, nil
	}

	if targetSpec != nil {
		return candidates, nil
	}
	// Component filters are applied by ValidateComponents using the same
	// matching rules as explicitly targeted requests.
	page := dbquery.DefaultPagination()
	for {
		components, total, err := rs.inventoryManager.GetListOfComponents(ctx, dbquery.StringQueryInfo{}, nil, nil, nil, page, nil)
		if err != nil {
			return nil, err
		}
		candidates = append(candidates, components...)
		page.Offset += len(components)
		if len(components) == 0 || page.Offset >= int(total) {
			break
		}
	}
	return candidates, nil
}
