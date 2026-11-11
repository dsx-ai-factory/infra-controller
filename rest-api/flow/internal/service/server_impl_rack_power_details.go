// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package service

import (
	"context"
	"slices"
	"strings"
	"sync"

	"github.com/google/uuid"
	"github.com/rs/zerolog/log"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/converter/protobuf"
	taskdef "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/task"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/component"
	pb "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/proto/v1"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// corePowerDetailsBatchSize follows GetComponentPowerDetails' fixed 1–64 target
// contract, which is independent of Core's configurable FindByIds limit.
const corePowerDetailsBatchSize = 64

// GetRackPowerDetails expands membership once, then independently reads Core
// facts and outstanding tasks. Unlinked members stay visible in both coverage
// and the member list; task targets remain the recorded selections.
func (rs *FlowServerImpl) GetRackPowerDetails(ctx context.Context, req *pb.GetRackPowerDetailsRequest) (*pb.GetRackPowerDetailsResponse, error) {
	if req.GetId() == "" {
		return nil, status.Error(codes.InvalidArgument, "rack identifier is required")
	}
	readCtx, cancel := powerDetailsReadContext(ctx)
	defer cancel()
	if readCtx.Err() != nil {
		return nil, status.FromContextError(readCtx.Err()).Err()
	}
	rack, err := rs.inventoryManager.GetRackByIdentifier(readCtx, externalRackIdentifier(req.GetId()), true)
	if readCtx.Err() != nil {
		return nil, status.FromContextError(readCtx.Err()).Err()
	}
	if err != nil {
		code := status.Code(err)
		if code == codes.NotFound {
			return nil, status.Error(codes.NotFound, "rack not found")
		}
		log.Warn().Str("grpc_status_code", code.String()).Msg("Power details rack inventory lookup failed")
		if code != codes.Internal {
			code = codes.Unavailable
		}
		return nil, status.Error(code, "rack inventory could not be read")
	}
	if rack == nil {
		return nil, status.Error(codes.Internal, "rack inventory returned no rack")
	}

	// Inventory ownership is not transferred by the interface; sort a copy so
	// the returned component slice keeps its original order.
	members := slices.Clone(rack.Components)
	slices.SortFunc(members, func(a, b component.Component) int {
		if a.Type != b.Type {
			return int(a.Type) - int(b.Type)
		}
		return strings.Compare(a.Info.ID.String(), b.Info.ID.String())
	})
	details := &pb.RackPowerDetails{
		RackId: protobuf.UUIDTo(rack.Info.ID), RackExternalId: rack.ExternalID,
		Operations: &pb.RackPowerOperations{},
	}
	for _, member := range members {
		tray := &pb.RackTrayPowerDetails{
			InternalComponentId: protobuf.UUIDTo(member.Info.ID), ComponentType: protobuf.ComponentTypeTo(member.Type),
			Operations: &pb.RackTrayPowerOperations{},
		}
		if member.ComponentID != "" {
			tray.ComponentId = &member.ComponentID
		}
		details.Trays = append(details.Trays, tray)
	}

	var tasks []taskdef.PowerTaskDetails
	var reads sync.WaitGroup
	reads.Add(2)
	// Core writes tray sections; the task branch writes rack task metadata.
	go func() {
		defer reads.Done()
		rs.readRackCorePowerDetails(readCtx, rack.Info.ID, members, details.Trays)
	}()
	go func() {
		defer reads.Done()
		tasks, details.Operations.Tasks = rs.readRackPowerTasks(readCtx, rack.Info.ID)
		// Resolve saved target UUIDs alongside Core reads, using the same
		// remaining deadline without waiting for slow Core batches to finish.
		// A missing identity marks only its own task.
		rs.addPowerTaskExternalTargets(readCtx, rack.ExternalID, details.Operations.Tasks.Entries)
	}()
	reads.Wait()

	// Compile each task once, keeping all of its saved targets at rack scope.
	// A task selecting a removed member still belongs to this rack's work.
	uniqueTasks := make(map[string]bool, len(tasks))
	fullTasks := details.Operations.Tasks.Entries
	details.Operations.Tasks.Entries = nil
	for i, task := range tasks {
		id := task.ID.String()
		if uniqueTasks[id] {
			continue
		}
		uniqueTasks[id] = true
		details.Operations.Tasks.Entries = append(details.Operations.Tasks.Entries, fullTasks[i])
		for j, member := range members {
			if !supportsPowerDetails(member.Type) {
				continue
			}
			relevance := task.RelevanceFor(member.Info.ID, member.Type)
			if relevance == taskdef.PowerTaskUnrelated {
				continue
			}
			tray := details.Trays[j]
			if tray.Operations.Tasks == nil {
				tray.Operations.Tasks = &pb.PowerTaskReferenceList{}
			}
			reference := &pb.PowerTaskReference{Id: fullTasks[i].Id, Relevance: pb.PowerTaskRelevance_POWER_TASK_RELEVANCE_SELECTED}
			if relevance == taskdef.PowerTaskRackAffected {
				reference.Relevance = pb.PowerTaskRelevance_POWER_TASK_RELEVANCE_RACK_AFFECTED
			}
			tray.Operations.Tasks.Entries = append(tray.Operations.Tasks.Entries, reference)
		}
	}
	for i, tray := range details.Trays {
		if tray.Operations.Tasks == nil {
			tray.Operations.Tasks = &pb.PowerTaskReferenceList{Error: details.Operations.Tasks.Error}
		}
		kind := members[i].Type
		if !supportsPowerDetails(kind) {
			code := pb.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_UNSUPPORTED
			tray.Operations.Tasks = &pb.PowerTaskReferenceList{Error: &code}
		}
		details.Operations.Actions = append(details.Operations.Actions, &pb.RackMemberPowerActions{
			InternalComponentId: tray.InternalComponentId, ComponentId: tray.ComponentId,
			ComponentType: tray.ComponentType, Details: tray.Operations.Actions,
		})
	}
	details.Actual, details.Desired = summarizeRackPower(details.Trays)
	response := &pb.GetRackPowerDetailsResponse{Details: details}
	err = boundRackPowerDetailsResponse(response)
	if ctx.Err() != nil {
		return nil, status.FromContextError(ctx.Err()).Err()
	}
	if err != nil {
		return nil, err
	}
	return response, nil
}

// boundRackPowerDetailsResponse clears waiting context before failing an
// oversized rack read. Each Waiting task lists every earlier Waiting task, so
// those lists grow quadratically and can push a busy rack past the proxy
// budget. Member task references are never removed: the response keeps them
// all or fails with ResourceExhausted.
func boundRackPowerDetailsResponse(response *pb.GetRackPowerDetailsResponse) error {
	fits, err := powerDetailsResponseFits(response)
	if err != nil || fits {
		return err
	}
	limitPowerTaskWaitingContexts(response.GetDetails().GetOperations().GetTasks().GetEntries())
	fits, err = powerDetailsResponseFits(response)
	if err != nil || fits {
		return err
	}
	return status.Error(codes.ResourceExhausted, "rack power details exceed the response size limit")
}

// corePowerTarget identifies one target within a Core batch. Several inventory
// members can store differently cased versions of the same typed Core ID.
type corePowerTarget struct {
	kind devicetypes.ComponentType
	id   string
}

// readRackCorePowerDetails groups only supported managed members. Each batch is
// checked against Core's ordered response contract before any member uses it;
// a failed batch does not erase completed batches or prevent the next one while
// the shared read budget remains.
func (rs *FlowServerImpl) readRackCorePowerDetails(ctx context.Context, rackID uuid.UUID, members []component.Component, trays []*pb.RackTrayPowerDetails) {
	groups := make(map[devicetypes.ComponentType][]corePowerTarget)
	membersByTarget := make(map[corePowerTarget][]int)
	for i, member := range members {
		code := pb.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_NOT_LINKED
		switch {
		case supportsPowerDetails(member.Type):
			if member.ComponentID != "" {
				if rs.powerReader == nil {
					code = pb.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_NOT_CONFIGURED
				} else {
					id, valid := normalizeCorePowerTarget(member.Type, member.ComponentID)
					if !valid {
						trays[i].Actual, trays[i].Desired, trays[i].Operations.Actions = unavailableCorePowerDetails(pb.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_READ_FAILED)
						continue
					}
					target := corePowerTarget{kind: member.Type, id: id}
					if len(membersByTarget[target]) == 0 {
						groups[member.Type] = append(groups[member.Type], target)
					}
					membersByTarget[target] = append(membersByTarget[target], i)
				}
			}
		default:
			code = pb.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_UNSUPPORTED
		}
		trays[i].Actual, trays[i].Desired, trays[i].Operations.Actions = unavailableCorePowerDetails(code)
	}
	for _, kind := range []devicetypes.ComponentType{devicetypes.ComponentTypeCompute, devicetypes.ComponentTypeNVSwitch, devicetypes.ComponentTypePowerShelf} {
		for batch := range slices.Chunk(groups[kind], corePowerDetailsBatchSize) {
			ids := make([]string, 0, len(batch))
			for _, target := range batch {
				ids = append(ids, target.id)
			}
			var response *corev1.GetComponentPowerDetailsResponse
			err := ctx.Err()
			dispatched := err == nil
			grpcCode := codes.OK
			if err == nil {
				response, err = rs.powerReader.GetComponentPowerDetails(ctx, corePowerDetailsRequest(kind, ids))
				grpcCode = status.Code(err)
			}
			if ctx.Err() != nil {
				err = ctx.Err()
			}
			code := pb.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_INVALID_RESPONSE
			valid := err == nil && len(response.GetEntries()) == len(batch)
			if valid {
				for i, entry := range response.Entries {
					if !sameCorePowerIdentity(ids[i], entry.GetComponentId()) {
						valid = false
						break
					}
				}
			}
			if err != nil {
				code = powerDetailsReadError(err)
			}
			// An expired or cancelled read is routine. Log dependency failures with
			// the gRPC code, which the bounded read error category collapses.
			if !valid && dispatched && ctx.Err() == nil {
				log.Warn().Str("rack_id", rackID.String()).Int("target_count", len(batch)).
					Str("grpc_status_code", grpcCode.String()).Str("error_category", code.String()).Msg("Power details Core batch lookup failed")
			}
			for j, target := range batch {
				for _, i := range membersByTarget[target] {
					if valid {
						trays[i].Actual, trays[i].Desired, trays[i].Operations.Actions = corePowerDetailsSections(response.Entries[j])
					} else {
						trays[i].Actual, trays[i].Desired, trays[i].Operations.Actions = unavailableCorePowerDetails(code)
					}
				}
			}
		}
	}
}

// summarizeRackPower reports a shared state only with complete applicable
// evidence. Missing reads leave it Unknown, while disagreement between known
// states remains Mixed. Coverage preserves the evidence behind either result.
func summarizeRackPower(trays []*pb.RackTrayPowerDetails) (*pb.RackPowerSummary, *pb.RackPowerSummary) {
	actual := &pb.RackPowerSummary{Coverage: &pb.RackPowerCoverage{}}
	desired := &pb.RackPowerSummary{Coverage: &pb.RackPowerCoverage{}}
	for _, tray := range trays {
		actual.Coverage.Total++
		unsupportedType := !supportsPowerDetails(protobuf.ComponentTypeFrom(tray.ComponentType))
		observation := tray.GetActual().GetValue()
		state, observed := observation.GetOutcome().(*corev1.ComponentPowerObservation_State)
		switch {
		case unsupportedType && tray.GetActual().GetError() == pb.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_UNSUPPORTED,
			observation.GetError().GetCode() == corev1.ComponentPowerErrorCode_COMPONENT_POWER_ERROR_CODE_UNSUPPORTED:
			// ToR switches, UMS and CDUs have no power reads, and some Core
			// backends, such as NSM and PSM, can't provide a passive one. These
			// members will never report, so they can't leave a rack incomplete.
			actual.Coverage.Unsupported++
		case !observed:
			// A failed or absent observation leaves coverage incomplete unless
			// the member explicitly reports Unsupported above.
			actual.Coverage.Unavailable++
		case state.State == corev1.ComponentPowerState_COMPONENT_POWER_STATE_UNKNOWN:
			actual.Coverage.Unknown++
		case state.State < corev1.ComponentPowerState_COMPONENT_POWER_STATE_ON || state.State > corev1.ComponentPowerState_COMPONENT_POWER_STATE_RESET:
			actual.Coverage.Unavailable++
		default:
			// RackPowerState preserves numeric values for known Core states; Mixed is
			// added by aggregation only and is never a hardware observation.
			addKnownRackPowerState(actual, pb.RackPowerState(state.State))
		}

		desired.Coverage.Total++
		policy := tray.GetDesired().GetValue()
		switch {
		case unsupportedType && tray.GetDesired().GetError() == pb.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_UNSUPPORTED:
			desired.Coverage.Unsupported++
		case policy == nil:
			desired.Coverage.Unavailable++
		case policy.Status == corev1.ComponentPowerPolicyStatus_COMPONENT_POWER_POLICY_STATUS_NOT_APPLICABLE:
			desired.Coverage.NotApplicable++
		case policy.Status == corev1.ComponentPowerPolicyStatus_COMPONENT_POWER_POLICY_STATUS_UNAVAILABLE:
			// Core uses Unavailable for both absent policy and a missing
			// managed target. NotFound distinguishes the missing target, so
			// its stale Flow link still leaves desired incomplete.
			missingTarget := observation.GetError().GetCode() == corev1.ComponentPowerErrorCode_COMPONENT_POWER_ERROR_CODE_NOT_FOUND ||
				tray.GetOperations().GetActions().GetValue().GetError().GetCode() == corev1.ComponentPowerErrorCode_COMPONENT_POWER_ERROR_CODE_NOT_FOUND
			if missingTarget {
				desired.Coverage.Unavailable++
			} else {
				desired.Coverage.NoPolicy++
			}
		case policy.Status != corev1.ComponentPowerPolicyStatus_COMPONENT_POWER_POLICY_STATUS_AVAILABLE || policy.State == nil:
			desired.Coverage.Unavailable++
		case *policy.State == corev1.PowerState_PowerManagerDisabled:
			desired.Coverage.DisabledPolicy++
		case *policy.State == corev1.PowerState_On:
			addKnownRackPowerState(desired, pb.RackPowerState_RACK_POWER_STATE_ON)
		case *policy.State == corev1.PowerState_Off:
			addKnownRackPowerState(desired, pb.RackPowerState_RACK_POWER_STATE_OFF)
		default:
			desired.Coverage.Unavailable++
		}
	}
	// Unknown and unavailable members are the evidence we're missing; every
	// other member either voted or isn't expected to.
	for _, summary := range []*pb.RackPowerSummary{actual, desired} {
		summary.Complete = summary.Coverage.Unknown == 0 && summary.Coverage.Unavailable == 0
		if !summary.Complete && summary.State != pb.RackPowerState_RACK_POWER_STATE_MIXED {
			summary.State = pb.RackPowerState_RACK_POWER_STATE_UNKNOWN
		}
	}
	return actual, desired
}

// addKnownRackPowerState counts a member's known vote. Once two members
// disagree, later agreeing members cannot change the summary from Mixed.
func addKnownRackPowerState(summary *pb.RackPowerSummary, state pb.RackPowerState) {
	if summary.Coverage.Known == 0 {
		summary.State = state
	} else if summary.State != state {
		summary.State = pb.RackPowerState_RACK_POWER_STATE_MIXED
	}
	summary.Coverage.Known++
}
