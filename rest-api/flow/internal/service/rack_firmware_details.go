// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package service

import (
	"cmp"
	"slices"
	"strings"

	"github.com/google/uuid"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/converter/protobuf"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/component"
	pb "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/proto/v1"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// rackFirmwareDetails holds each component's firmware evidence and shared tasks.
// Actions stay with their component; tasks appear once, with references from all
// selected components. Any later task-list replacement must also replace those
// references and their errors.
type rackFirmwareDetails struct {
	components []rackFirmwareComponentDetails
	actual     rackFirmwareCoverage
	desired    rackFirmwareCoverage
	tasks      *pb.FirmwareTaskDetailsList
}

// firmwareCoreDetails keeps the three independently available Core sections
// together after the caller has checked response identity and cardinality and
// applied coreFirmwareDetailsSections' section-presence rules. Present wrappers must
// contain a value or read error; Core values require installed/expected results
// and both stored/backend action sections. Known read failures belong in explicit
// error wrappers, so assembly never has to infer a deadline or configuration error.
type firmwareCoreDetails struct {
	actual  *pb.FirmwareActualDetails
	desired *pb.FirmwareDesiredDetails
	actions *pb.FirmwareActionDetails
}

// rackFirmwareComponentDetails retains an inventory member even before linking.
// internalComponentID associates reads and saved tasks; it must never be used as
// a fallback for the public hardware identity in componentID.
type rackFirmwareComponentDetails struct {
	internalComponentID uuid.UUID
	componentID         string
	componentType       devicetypes.ComponentType
	firmwareCoreDetails
	taskIDs    []uuid.UUID
	tasksError *pb.FirmwareDetailsReadError
}

// rackFirmwareCoverage counts one entry per inventory member. Reporting includes
// empty collections and inventories with failed entries; it does not mean update
// success. Unsupported means the member kind or Core backend lacks the capability.
// Unavailable includes other missing collections, including Flow Unsupported for
// a missing RPC or required section. Full entries and errors stay on each member.
type rackFirmwareCoverage struct {
	total       int
	reporting   int
	unsupported int
	unavailable int
}

// assembleRackFirmwareDetails uses the full inventory as its membership source.
// Members have unique inventory UUIDs, used to join Core results even when
// hardware IDs are empty. A missing linked result becomes ReadFailed; nil sections
// or task data become Unsupported. Callers supply more specific errors explicitly.
// Tasks come from one rack query, converted by firmwareTaskDetailsToProto with
// waiting context and external identities attached. Duplicate copies must agree;
// they are not observations from separate reads. IDs use the converter's canonical
// spelling. Output containers are new, but nested messages and error pointers are
// shared: callers must clone them before mutation, including response bounding.
func assembleRackFirmwareDetails(members []component.Component, core map[uuid.UUID]firmwareCoreDetails, tasks *pb.FirmwareTaskDetailsList) rackFirmwareDetails {
	members = slices.Clone(members)
	slices.SortFunc(members, func(a, b component.Component) int {
		if a.Type != b.Type {
			return cmp.Compare(a.Type, b.Type)
		}
		return strings.Compare(a.Info.ID.String(), b.Info.ID.String())
	})
	result := rackFirmwareDetails{tasks: &pb.FirmwareTaskDetailsList{}}
	if tasks == nil {
		result.tasks.Error = pb.FirmwareDetailsReadError_FIRMWARE_DETAILS_READ_ERROR_UNSUPPORTED.Enum()
	} else {
		result.tasks.Error = tasks.Error
	}
	// Reject unusable task identities before building either root records or
	// references. A corrupt record must not become a zero-UUID "duplicate".
	if result.tasks.Error == nil {
		for _, task := range tasks.Entries {
			id, err := uuid.Parse(task.GetId().GetId())
			if err != nil || id == uuid.Nil || slices.Contains(task.GetTargets().GetGroups(), nil) {
				result.tasks.Error = pb.FirmwareDetailsReadError_FIRMWARE_DETAILS_READ_ERROR_INVALID_RESPONSE.Enum()
				break
			}
		}
	}
	for _, member := range members {
		sections, found := core[member.Info.ID]
		code := pb.FirmwareDetailsReadError_FIRMWARE_DETAILS_READ_ERROR_UNSUPPORTED
		switch {
		case !supportsComponentDetails(member.Type):
			found = false
		case member.ComponentID == "":
			// Without a hardware identity, an accidentally supplied read is not attributable.
			code = pb.FirmwareDetailsReadError_FIRMWARE_DETAILS_READ_ERROR_NOT_LINKED
			found = false
		case !found:
			code = pb.FirmwareDetailsReadError_FIRMWARE_DETAILS_READ_ERROR_READ_FAILED
		}
		actual, desired, actions := unavailableCoreFirmwareDetails(code)
		// Missing individual sections follow coreFirmwareDetailsSections' Unsupported
		// contract, while the other sections keep their successful evidence.
		if !found || sections.actual == nil {
			sections.actual = actual
		}
		if !found || sections.desired == nil {
			sections.desired = desired
		}
		if !found || sections.actions == nil {
			sections.actions = actions
		}
		entry := rackFirmwareComponentDetails{
			internalComponentID: member.Info.ID, componentID: member.ComponentID,
			componentType: member.Type, firmwareCoreDetails: sections,
			tasksError: result.tasks.Error,
		}
		result.components = append(result.components, entry)
	}

	// Keep root tasks and member references in creation-time/UUID order, even
	// when equivalent copies from the same query arrive in a different order.
	if result.tasks.Error == nil {
		ordered := slices.Clone(tasks.Entries)
		slices.SortStableFunc(ordered, func(a, b *pb.FirmwareTaskDetails) int {
			order := a.GetCreatedAt().AsTime().Compare(b.GetCreatedAt().AsTime())
			if order != 0 {
				return order
			}
			return strings.Compare(a.GetId().GetId(), b.GetId().GetId())
		})
		seen := make(map[uuid.UUID]bool, len(ordered))
		for _, task := range ordered {
			id := protobuf.UUIDFrom(task.Id)
			if seen[id] {
				continue
			}
			seen[id] = true
			result.tasks.Entries = append(result.tasks.Entries, task)
			for i := range result.components {
				member := &result.components[i]
				// Saved selection is independent of passive Core support or linkage.
				for _, group := range task.GetTargets().GetGroups() {
					selected := group.Type == protobuf.ComponentTypeTo(member.componentType) &&
						slices.ContainsFunc(group.ComponentIds, func(target *pb.UUID) bool {
							return protobuf.UUIDFrom(target) == member.internalComponentID
						})
					if selected {
						member.taskIDs = append(member.taskIDs, id)
						break
					}
				}
			}
		}
	}
	result.actual, result.desired = summarizeRackFirmware(result.components)
	return result
}

// summarizeRackFirmware counts installed and expected evidence independently.
// A received collection can contain failed inventory entries or non-matching
// comparisons; neither changes collection availability or invents one rack version.
func summarizeRackFirmware(members []rackFirmwareComponentDetails) (rackFirmwareCoverage, rackFirmwareCoverage) {
	var actual, desired rackFirmwareCoverage
	for _, member := range members {
		unsupportedType := !supportsComponentDetails(member.componentType)
		installed := member.actual.GetValue()
		actual.total++
		switch {
		case unsupportedType || installed.GetError().GetCode() == corev1.ComponentFirmwareErrorCode_COMPONENT_FIRMWARE_ERROR_CODE_UNSUPPORTED:
			actual.unsupported++
		case installed.GetInventory() != nil:
			actual.reporting++
		default:
			actual.unavailable++
		}
		expected := member.desired.GetValue()
		desired.total++
		switch {
		case unsupportedType || expected.GetError().GetCode() == corev1.ComponentFirmwareErrorCode_COMPONENT_FIRMWARE_ERROR_CODE_UNSUPPORTED:
			desired.unsupported++
		case expected.GetHost() != nil || expected.GetComparisons() != nil:
			desired.reporting++
		default:
			desired.unavailable++
		}
	}
	return actual, desired
}
