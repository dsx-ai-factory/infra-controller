// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Design note: see package taskschedule for why TaskSchedule has no internal
// domain type. The proto<->dbmodel conversion helpers in this file are kept
// local for the same reason: they are only used here, and placing them in
// internal/converter/ would introduce a dbmodel dependency in a package whose
// other converters deliberately avoid it.

package service

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"time"

	"github.com/google/uuid"
	"google.golang.org/protobuf/types/known/emptypb"
	"google.golang.org/protobuf/types/known/timestamppb"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/converter/protobuf"
	dbmodel "github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/model"
	dbquery "github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/query"
	inventoryresolver "github.com/NVIDIA/infra-controller/rest-api/flow/internal/inventory/resolver"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/operation"
	taskschedule "github.com/NVIDIA/infra-controller/rest-api/flow/internal/scheduler/taskschedule"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	pb "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/proto/v1"
)

// CreateTaskSchedule creates a new task schedule.
func (rs *FlowServerImpl) CreateTaskSchedule(
	ctx context.Context,
	req *pb.CreateTaskScheduleRequest,
) (*pb.TaskSchedule, error) {
	// Input validation.
	sched := req.GetSchedule()
	if sched == nil {
		return nil, errors.New("schedule config is required")
	}

	if sched.GetName() == "" {
		return nil, errors.New("schedule.name is required")
	}

	spec := sched.GetSpec()
	if spec == nil ||
		spec.GetType() == pb.ScheduleSpecType_SCHEDULE_SPEC_TYPE_UNSPECIFIED {
		return nil, errors.New("schedule.spec.type is required")
	}

	if spec.GetSpec() == "" {
		return nil, errors.New("schedule.spec.spec is required")
	}

	// Extract operation info and target_spec from the ScheduledOperation.
	// The target_spec serves as the initial scope for the schedule.
	opInfo, targetSpec, pbQueueOpts, pbRuleID, err := protobuf.ScheduledOperationFrom(req.GetOperation())
	if err != nil {
		return nil, err
	}
	firmwareInfo, isFirmware := opInfo.(*operations.FirmwareControlTaskInfo)
	if isFirmware {
		err = rs.encryptFirmwareAuthenticationData(
			firmwareInfo,
			req.GetOperation().GetUpgradeFirmware().GetAuthenticationData(),
		)
		if err != nil {
			return nil, firmwareAuthenticationStatusError(err)
		}
	}

	raw, err := opInfo.Marshal()
	if err != nil {
		return nil, fmt.Errorf("marshal operation: %w", err)
	}

	// Resolve initial scope from the operation's target_spec.
	scopes, err := rs.resolveScope(ctx, targetSpec)
	if err != nil {
		return nil, err
	}

	if len(scopes) == 0 {
		return nil, errors.New("target_spec resolved to no rack scopes; at least one rack target is required") //nolint:lll
	}

	// The overlap policy drives the conflict strategy stored in the template:
	// a QUEUE schedule must submit tasks with ConflictStrategyQueue, otherwise
	// the task manager can reject the overlap and break the schedule contract.
	// queue_options may still supply a timeout when the policy is QUEUE.
	overlapPolicy := protoOverlapPolicyToModel(sched.GetOverlapPolicy())
	_, queueTimeout := protobuf.QueueOptionsFrom(pbQueueOpts)
	conflictStrategy := operation.ConflictStrategyReject
	if overlapPolicy == dbmodel.OverlapPolicyQueue {
		conflictStrategy = operation.ConflictStrategyQueue
	}
	templateOpts := taskschedule.TemplateOptions{
		ConflictStrategy: int(conflictStrategy),
		QueueTimeoutSecs: int64(queueTimeout.Seconds()),
	}
	if ruleUUID := protobuf.UUIDFrom(pbRuleID); ruleUUID != uuid.Nil {
		templateOpts.RuleID = ruleUUID.String()
	}

	// Build operation_template JSON (target comes from scope rows at fire time).
	templateJSON, err := taskschedule.MarshalTemplate(
		opInfo.Type(),
		opInfo.CodeString(),
		raw,
		templateOpts,
	)
	if err != nil {
		return nil, fmt.Errorf("build operation template: %w", err)
	}

	// Determine spec type and timezone.
	specType := protoSpecTypeToModel(spec.GetType())
	tz := spec.GetTimezone()
	if tz == "" && specType == dbmodel.SpecTypeCron {
		tz = "UTC"
	}

	// Compute next_run_at.
	nextRunAt, err := taskschedule.ComputeFirstRunAt(
		specType, spec.GetSpec(), tz,
	)
	if err != nil {
		return nil, fmt.Errorf("compute next_run_at: %w", err)
	}

	row := &dbmodel.TaskSchedule{
		Name:              sched.GetName(),
		SpecType:          specType,
		Spec:              spec.GetSpec(),
		Timezone:          tz,
		OperationTemplate: templateJSON,
		OverlapPolicy:     overlapPolicy,
		Enabled:           true,
		NextRunAt:         &nextRunAt,
	}

	// Create schedule and scope rows in a single transaction.
	var scheduleID uuid.UUID
	if err := rs.taskScheduleStore.RunInTransaction(
		ctx,
		func(ctx context.Context) error {
			var err error
			scheduleID, err = rs.taskScheduleStore.Create(ctx, row)
			if err != nil {
				return fmt.Errorf("create task schedule: %w", err)
			}

			for i := range scopes {
				scopes[i].ScheduleID = scheduleID
			}

			return rs.taskScheduleStore.CreateScopes(ctx, scopes)
		},
	); err != nil {
		return nil, err
	}

	created, err := rs.taskScheduleStore.Get(ctx, scheduleID)
	if err != nil {
		return nil, err
	}

	return taskScheduleToProto(created)
}

// GetTaskSchedule retrieves a task schedule by ID.
func (rs *FlowServerImpl) GetTaskSchedule(
	ctx context.Context,
	req *pb.GetTaskScheduleRequest,
) (*pb.TaskSchedule, error) {
	id := protobuf.UUIDFrom(req.GetId())
	if id == uuid.Nil {
		return nil, errors.New("id is required")
	}

	row, err := rs.taskScheduleStore.Get(ctx, id)
	if err != nil {
		return nil, err
	}

	return taskScheduleToProto(row)
}

// ListTaskSchedules lists task schedules, optionally filtered by rack.
func (rs *FlowServerImpl) ListTaskSchedules(
	ctx context.Context,
	req *pb.ListTaskSchedulesRequest,
) (*pb.ListTaskSchedulesResponse, error) {
	// Pagination is intentionally optional: a nil pg passes through to the
	// store, which skips LIMIT/OFFSET and returns all schedules. Callers
	// that omit pagination expect the full list, not a silently truncated
	// first page.
	var pg *dbquery.Pagination
	if req.GetPagination() != nil {
		pg = protobuf.PaginationFrom(req.GetPagination())
	}

	opts := taskschedule.ListOptions{
		EnabledOnly: req.GetEnabledOnly(),
		Pagination:  pg,
	}
	if rackID := protobuf.UUIDFrom(req.GetRackId()); rackID != uuid.Nil {
		opts.RackIDs = []uuid.UUID{rackID}
	}

	rows, total, err := rs.taskScheduleStore.List(ctx, opts)
	if err != nil {
		return nil, err
	}

	pbSchedules := make([]*pb.TaskSchedule, 0, len(rows))
	for _, r := range rows {
		t, err := taskScheduleToProto(r)
		if err != nil {
			return nil, err
		}

		pbSchedules = append(pbSchedules, t)
	}

	return &pb.ListTaskSchedulesResponse{
		TaskSchedules: pbSchedules,
		Total:         total,
	}, nil
}

// UpdateTaskSchedule updates the name and/or schedule config of a task
// schedule. update_mask is required and must list the paths to apply:
//   - "schedule.name"           – display name
//   - "schedule.overlap_policy" – overlap behaviour
//   - "schedule.spec"           – full spec block; recomputes next_run_at
//   - "schedule.spec.timezone"  – timezone only, spec type/string unchanged
func (rs *FlowServerImpl) UpdateTaskSchedule(
	ctx context.Context,
	req *pb.UpdateTaskScheduleRequest,
) (*pb.TaskSchedule, error) {
	id := protobuf.UUIDFrom(req.GetId())
	if id == uuid.Nil {
		return nil, errors.New("id is required")
	}

	paths := req.GetUpdateMask().GetPaths()

	// Determine upfront whether a spec or timezone change is requested. When it
	// is, next_run_at will be recomputed and we must hold a row lock while both
	// reading the existing row and writing the update so that a concurrent
	// fire() / FireNow() cannot advance next_run_at between the two operations.
	specOrTZChanging := false
	for _, p := range paths {
		if p == "schedule.spec" || p == "schedule.spec.timezone" {
			specOrTZChanging = true
			break
		}
	}

	if !specOrTZChanging {
		// Only name / overlap_policy — compute fields without any lock and write.
		fields, _, err := rs.buildUpdateFields(ctx, id, req.GetSchedule(), paths, nil)
		if err != nil {
			return nil, err
		}
		row, err := rs.taskScheduleStore.Update(ctx, id, fields)
		if err != nil {
			return nil, err
		}
		return taskScheduleToProto(row)
	}

	// spec or timezone in mask: lock the row first so that the existing-row
	// read (for the counterpart field) and the next_run_at write are atomic.
	var row *dbmodel.TaskSchedule
	if err := rs.taskScheduleStore.RunInTransaction(
		ctx,
		func(ctx context.Context) error {
			locked, err := rs.taskScheduleStore.LockForTrigger(ctx, id)
			if err != nil {
				return err
			}

			// Build update fields from the locked snapshot — no separate Get call.
			fields, nextRunAtChanging, err := rs.buildUpdateFields(
				ctx, id, req.GetSchedule(), paths, locked,
			)
			if err != nil {
				return err
			}

			// Guard against updating a one-time schedule that has already fired.
			// Only relevant when next_run_at would change; a timezone-only change
			// on an interval spec, for example, produces nextRunAtChanging=false
			// and does not need this check.
			if nextRunAtChanging &&
				locked.SpecType == dbmodel.SpecTypeOneTime &&
				locked.NextRunAt == nil {
				return errors.New(
					"cannot change the spec of a one-time schedule that has already fired; create a new one instead", //nolint:lll
				)
			}

			row, err = rs.taskScheduleStore.Update(ctx, id, fields)
			return err
		},
	); err != nil {
		return nil, err
	}

	return taskScheduleToProto(row)
}

// PauseTaskSchedule disables a task schedule so it will not fire until resumed.
// Returns an error if the schedule is a one-time type that has already fired
// (enabled=false with spec_type=one-time), since there is nothing left to pause.
// Returns the existing record unchanged if already paused.
func (rs *FlowServerImpl) PauseTaskSchedule(
	ctx context.Context,
	req *pb.PauseTaskScheduleRequest,
) (*pb.TaskSchedule, error) {
	id := protobuf.UUIDFrom(req.GetId())
	if id == uuid.Nil {
		return nil, errors.New("id is required")
	}

	existing, err := rs.taskScheduleStore.Get(ctx, id)
	if err != nil {
		return nil, err
	}

	if existing.SpecType == dbmodel.SpecTypeOneTime && existing.NextRunAt == nil {
		return nil, errors.New(
			"cannot pause a one-time schedule that has already fired",
		)
	}

	// No-op if already paused.
	if !existing.Enabled {
		return taskScheduleToProto(existing)
	}

	row, err := rs.taskScheduleStore.SetEnabled(ctx, id, false)
	if err != nil {
		return nil, err
	}

	return taskScheduleToProto(row)
}

// ResumeTaskSchedule re-enables a paused task schedule.
// Returns the existing record unchanged if already enabled.
// Returns an error if the schedule is a one-time type that has already fired
// (next_run_at=nil), since it cannot be re-armed. A one-time schedule that
// was paused before firing (next_run_at still set) can be resumed normally.
// For interval and cron schedules, next_run_at is recomputed from the current
// time so the schedule does not fire immediately on resume.
func (rs *FlowServerImpl) ResumeTaskSchedule(
	ctx context.Context,
	req *pb.ResumeTaskScheduleRequest,
) (*pb.TaskSchedule, error) {
	id := protobuf.UUIDFrom(req.GetId())
	if id == uuid.Nil {
		return nil, errors.New("id is required")
	}

	// Lock the row before reading state: a concurrent dispatcher firing could
	// consume a one-time schedule between an unlocked Get and the Resume write,
	// leaving the schedule in an invalid state (enabled=true, next_run_at=NULL).
	var row *dbmodel.TaskSchedule
	if err := rs.taskScheduleStore.RunInTransaction(
		ctx,
		func(ctx context.Context) error {
			locked, err := rs.taskScheduleStore.LockForTrigger(ctx, id)
			if err != nil {
				return err
			}

			// No-op if already enabled.
			if locked.Enabled {
				row = locked
				return nil
			}

			// A one-time schedule with no next_run_at has already fired and
			// cannot be re-armed.
			if locked.SpecType == dbmodel.SpecTypeOneTime &&
				locked.NextRunAt == nil {
				return errors.New(
					"cannot resume a one-time schedule that has already fired",
				)
			}

			// Recompute next_run_at for interval/cron schedules so the schedule
			// does not fire immediately if next_run_at is still in the past from
			// before the pause. enabled and next_run_at are written atomically.
			var nextRunAt *time.Time
			switch locked.SpecType {
			case dbmodel.SpecTypeInterval, dbmodel.SpecTypeCron:
				next, err := taskschedule.ComputeFirstRunAt(
					locked.SpecType, locked.Spec, locked.Timezone,
				)
				if err != nil {
					return fmt.Errorf("compute next_run_at: %w", err)
				}
				nextRunAt = &next
			}

			row, err = rs.taskScheduleStore.Resume(ctx, id, nextRunAt)
			return err
		},
	); err != nil {
		return nil, err
	}

	return taskScheduleToProto(row)
}

// DeleteTaskSchedule hard-deletes a task schedule and its scopes (cascade).
func (rs *FlowServerImpl) DeleteTaskSchedule(
	ctx context.Context,
	req *pb.DeleteTaskScheduleRequest,
) (*emptypb.Empty, error) {
	id := protobuf.UUIDFrom(req.GetId())
	if id == uuid.Nil {
		return nil, errors.New("id is required")
	}

	if err := rs.taskScheduleStore.Delete(ctx, id); err != nil {
		return nil, err
	}

	return &emptypb.Empty{}, nil
}

// TriggerTaskSchedule fires a task schedule immediately, regardless of next_run_at
// or enabled state. The overlap policy is not consulted — all scopes are submitted
// unconditionally. Returns an error if called on a one-time schedule that has already fired.
func (rs *FlowServerImpl) TriggerTaskSchedule(
	ctx context.Context,
	req *pb.TriggerTaskScheduleRequest,
) (*pb.SubmitTaskResponse, error) {
	id := protobuf.UUIDFrom(req.GetId())
	if id == uuid.Nil {
		return nil, errors.New("id is required")
	}

	taskIDs, err := rs.taskScheduleDispatcher.FireNow(ctx, id)
	if err != nil {
		return nil, err
	}

	return &pb.SubmitTaskResponse{TaskIds: protobuf.UUIDsTo(taskIDs)}, nil
}

// AddTaskScheduleScope is the additive variant of scope management:
//   - incoming racks are merged into the existing scope.
//   - a rack already present has its component filter unioned with the
//     incoming filter.
//   - a rack not yet present is added.
//   - existing racks are never removed.
func (rs *FlowServerImpl) AddTaskScheduleScope(
	ctx context.Context,
	req *pb.AddTaskScheduleScopeRequest,
) (*pb.AddTaskScheduleScopeResponse, error) {
	scheduleID := protobuf.UUIDFrom(req.GetScheduleId())
	if scheduleID == uuid.Nil {
		return nil, errors.New("schedule_id is required")
	}

	incoming, err := rs.resolveScheduleScope(ctx, scheduleID, req.GetTargetSpec())
	if err != nil {
		return nil, err
	}

	diffFunc := func(
		incoming, existing []*dbmodel.TaskScheduleScope,
	) ([]*dbmodel.TaskScheduleScope, []*dbmodel.TaskScheduleScope, []*dbmodel.TaskScheduleScope, error) { //nolint:lll
		toCreate, toMerge, err := partitionScopeChanges(incoming, existing)
		return toCreate, nil, toMerge, err
	}

	toCreate, _, toMerge, _, err := rs.applyScopeChanges(
		ctx, scheduleID, incoming, diffFunc,
	)
	if err != nil {
		return nil, err
	}

	// Return the newly created and merged scopes.
	affected := make([]*pb.TaskScheduleScope, 0, len(toCreate)+len(toMerge))
	for _, s := range append(toCreate, toMerge...) {
		pbScope, err := taskScheduleScopeToProto(s)
		if err != nil {
			return nil, err
		}
		affected = append(affected, pbScope)
	}

	return &pb.AddTaskScheduleScopeResponse{Scopes: affected}, nil
}

// UpdateTaskScheduleScope is the reconciling variant of scope management: the
// schedule's scope is replaced to match the desired target spec exactly. Racks
// absent from the desired spec are removed; racks present in the desired spec
// but not in the current scope are added; racks in both have their component
// filter replaced if it changed.
//
// See AddTaskScheduleScope for the additive variant that merges without removing.
func (rs *FlowServerImpl) UpdateTaskScheduleScope(
	ctx context.Context,
	req *pb.UpdateTaskScheduleScopeRequest,
) (*pb.UpdateTaskScheduleScopeResponse, error) {
	scheduleID := protobuf.UUIDFrom(req.GetScheduleId())
	if scheduleID == uuid.Nil {
		return nil, errors.New("schedule_id is required")
	}

	desired, err := rs.resolveScheduleScope(ctx, scheduleID, req.GetDesiredScope())
	if err != nil {
		return nil, err
	}

	toAdd, toRemove, toUpdate, final, err := rs.applyScopeChanges(
		ctx, scheduleID, desired, diffScopeChanges,
	)
	if err != nil {
		return nil, err
	}

	pbScopes := make([]*pb.TaskScheduleScope, len(final))
	for i, s := range final {
		pbScope, err := taskScheduleScopeToProto(s)
		if err != nil {
			return nil, err
		}
		pbScopes[i] = pbScope
	}

	return &pb.UpdateTaskScheduleScopeResponse{
		Scopes:  pbScopes,
		Added:   int32(len(toAdd)),
		Removed: int32(len(toRemove)),
		Updated: int32(len(toUpdate)),
	}, nil
}

// RemoveTaskScheduleScope removes a rack scope entry from a task schedule.
func (rs *FlowServerImpl) RemoveTaskScheduleScope(
	ctx context.Context,
	req *pb.RemoveTaskScheduleScopeRequest,
) (*emptypb.Empty, error) {
	scopeID := protobuf.UUIDFrom(req.GetScopeId())
	if scopeID == uuid.Nil {
		return nil, errors.New("scope_id is required")
	}

	// Resolve the parent schedule ID so we can acquire the same row lock used
	// by Add/Update. Without this lock a concurrent Add/Update could snapshot
	// stale scope state (still containing this row) and re-create or retain it
	// after our delete commits.
	scope, err := rs.taskScheduleStore.GetScope(ctx, scopeID)
	if err != nil {
		return nil, err
	}

	err = rs.taskScheduleStore.RunInTransaction(
		ctx,
		func(ctx context.Context) error {
			// Block until any concurrent Add/Update (or another Remove) on
			// this schedule releases its lock, then hold the lock for the
			// duration of the delete so no other mutation can observe a
			// stale snapshot.
			_, err := rs.taskScheduleStore.LockForTrigger(ctx, scope.ScheduleID)
			if err != nil {
				return fmt.Errorf("lock schedule: %w", err)
			}

			// Re-read the current scope list under the lock and verify that
			// removing this entry would not leave the schedule with zero scopes.
			// The create path enforces that a schedule must have at least one
			// rack target; removing the last scope would violate that invariant
			// and produce an enabled schedule the dispatcher can never fire.
			existing, err := rs.taskScheduleStore.ListScopes(ctx, scope.ScheduleID)
			if err != nil {
				return fmt.Errorf("list scopes: %w", err)
			}
			if len(existing) <= 1 {
				return errors.New(
					"cannot remove the last scope: a schedule must have at least one rack target; delete the schedule instead",
				)
			}

			return rs.taskScheduleStore.DeleteScope(ctx, scopeID)
		},
	)
	if err != nil {
		return nil, err
	}

	return &emptypb.Empty{}, nil
}

// CheckScheduleConflicts is an advisory RPC that identifies which existing
// enabled schedules may conflict with a proposed operation at execution time.
// It does not block creation — callers may proceed even when conflicts are
// returned. Execution-time conflict detection remains the authoritative
// backstop.
//
// The check is deliberately coarse: it compares only the operation type and
// code of the proposed operation against those of each existing schedule on
// the same racks. It does not intersect component-type filters or explicit
// component UUID lists, so two schedules that target disjoint component sets
// on the same rack will still be flagged as conflicting here. Callers should
// treat a non-empty response as a prompt for human review rather than a
// definitive statement that tasks will collide at runtime.
func (rs *FlowServerImpl) CheckScheduleConflicts(
	ctx context.Context,
	req *pb.CheckScheduleConflictsRequest,
) (*pb.CheckScheduleConflictsResponse, error) {
	opInfo, targetSpec, _, _, err := protobuf.ScheduledOperationFrom(
		req.GetOperation(),
	)
	if err != nil {
		return nil, err
	}

	scopes, err := rs.resolveScope(ctx, targetSpec)
	if err != nil {
		return nil, err
	}

	if len(scopes) == 0 {
		return nil, errors.New("target_spec resolved to no rack scopes; at least one rack target is required")
	}

	excludeID := protobuf.UUIDFrom(req.GetExcludeScheduleId())
	proposedOp := operation.Wrapper{
		Type: opInfo.Type(),
		Code: opInfo.CodeString(),
	}

	// Collect the rack IDs from all resolved scopes and fetch all enabled
	// schedules across those racks in a single query instead of one per rack.
	rackIDs := make([]uuid.UUID, 0, len(scopes))
	for _, scope := range scopes {
		rackIDs = append(rackIDs, scope.RackID)
	}

	existingRows, _, err := rs.taskScheduleStore.List(
		ctx,
		taskschedule.ListOptions{
			RackIDs:     rackIDs,
			EnabledOnly: true,
		},
	)
	if err != nil {
		return nil, fmt.Errorf("list schedules for conflict check: %w", err)
	}

	// Pre-seed excludeID so the dedup check below doubles as the exclude check,
	// avoiding a separate branch per iteration.
	seenScheduleIDs := make(map[uuid.UUID]struct{})
	if excludeID != uuid.Nil {
		seenScheduleIDs[excludeID] = struct{}{}
	}
	var conflicts []*pb.TaskSchedule

	for _, s := range existingRows {
		if _, seen := seenScheduleIDs[s.ID]; seen {
			continue
		}

		seenScheduleIDs[s.ID] = struct{}{}

		scheduleOp, err := taskschedule.WrapperFromTemplate(
			s.OperationTemplate,
		)
		if err != nil {
			return nil, fmt.Errorf(
				"parse operation template for schedule %s: %w", s.ID, err,
			)
		}

		if rs.conflictResolver.HasScheduleConflict(
			proposedOp,
			[]operation.Wrapper{scheduleOp},
		) {
			pbSched, err := taskScheduleToProto(s)
			if err != nil {
				return nil, fmt.Errorf("convert schedule %s: %w", s.ID, err)
			}
			conflicts = append(conflicts, pbSched)
		}
	}

	if conflicts == nil {
		conflicts = []*pb.TaskSchedule{}
	}

	return &pb.CheckScheduleConflictsResponse{Conflicts: conflicts}, nil
}

// ListTaskScheduleScopes lists all scope entries for a task schedule.
func (rs *FlowServerImpl) ListTaskScheduleScopes(
	ctx context.Context,
	req *pb.ListTaskScheduleScopesRequest,
) (*pb.ListTaskScheduleScopesResponse, error) {
	scheduleID := protobuf.UUIDFrom(req.GetScheduleId())
	if scheduleID == uuid.Nil {
		return nil, errors.New("schedule_id is required")
	}

	scopes, err := rs.taskScheduleStore.ListScopes(ctx, scheduleID)
	if err != nil {
		return nil, err
	}

	// The create path enforces at least one scope, so an empty result
	// indicates the schedule ID is invalid.
	if len(scopes) == 0 {
		if _, err := rs.taskScheduleStore.Get(ctx, scheduleID); err != nil {
			return nil, err
		}
	}

	pbScopes := make([]*pb.TaskScheduleScope, 0, len(scopes))
	for _, s := range scopes {
		pbScope, err := taskScheduleScopeToProto(s)
		if err != nil {
			return nil, err
		}
		pbScopes = append(pbScopes, pbScope)
	}

	return &pb.ListTaskScheduleScopesResponse{Scopes: pbScopes}, nil
}

// ─── helpers ─────────────────────────────────────────────────────────────────

// buildUpdateFields validates the update_mask, converts the masked proto fields
// into an UpdateFields value, and recomputes NextRunAt when the spec or timezone
// changes.
//
// existing is the locked schedule row the caller has already fetched under a
// row lock. When present it is used to fill in the counterpart field for
// single-sided spec/timezone updates (e.g. spec-only change needs existing
// timezone, and vice-versa). When nil and a counterpart value is needed,
// buildUpdateFields falls back to a plain Get.
//
// The second return value reports whether next_run_at will change as a result
// of this update.
func (rs *FlowServerImpl) buildUpdateFields(
	ctx context.Context,
	id uuid.UUID,
	sched *pb.ScheduleConfig,
	paths []string,
	existing *dbmodel.TaskSchedule,
) (taskschedule.UpdateFields, bool, error) {
	var fields taskschedule.UpdateFields

	if len(paths) == 0 {
		return fields, false, errors.New("update_mask is required")
	}

	if sched == nil {
		return fields, false, errors.New("schedule is required")
	}

	spec := sched.GetSpec()

	for _, path := range paths {
		switch path {
		case "schedule.name":
			if len(sched.GetName()) == 0 {
				return fields, false, errors.New(
					`update_mask path "schedule.name" requires a non-empty name`,
				)
			}
			fields.Name = sched.GetName()
		case "schedule.overlap_policy":
			fields.OverlapPolicy = protoOverlapPolicyToModel(
				sched.GetOverlapPolicy(),
			)
		case "schedule.spec.timezone":
			if spec == nil {
				return fields, false, errors.New(
					`update_mask path "schedule.spec.timezone" requires schedule.spec to be set`,
				)
			}
			tz := spec.GetTimezone()
			if len(tz) == 0 {
				tz = "UTC"
			}
			fields.Timezone = tz
		case "schedule.spec":
			if spec == nil {
				return fields, false, errors.New(
					`update_mask path "schedule.spec" requires schedule.spec to be set`,
				)
			}
			if spec.GetType() ==
				pb.ScheduleSpecType_SCHEDULE_SPEC_TYPE_UNSPECIFIED {
				return fields, false, errors.New(
					`update_mask path "schedule.spec" requires spec.type to be set`,
				)
			}
			if len(spec.GetSpec()) == 0 {
				return fields, false, errors.New(
					`update_mask path "schedule.spec" requires a non-empty spec.spec`,
				)
			}
			// Format validity (valid duration / cron expression / RFC3339 timestamp)
			// is not checked here. It is caught as a side effect of ComputeFirstRunAt
			// in the NextRunAt recomputation block below, which calls the appropriate
			// parser and returns an error if the string is malformed.
			fields.SpecType = protoSpecTypeToModel(spec.GetType())
			fields.Spec = spec.GetSpec()
		default:
			return fields, false, fmt.Errorf("unsupported update_mask path: %q", path)
		}
	}

	// recompute holds the three inputs needed by ComputeFirstRunAt. It is nil
	// when no NextRunAt recomputation is required (neither spec nor timezone
	// is changing, or the timezone-only change does not affect a non-cron schedule).
	var recompute *struct {
		specType dbmodel.SpecType
		spec     string
		timezone string
	}

	if (len(fields.SpecType) == 0) != (len(fields.Timezone) == 0) {
		// Only one of the two time-related fields is changing — need the other
		// from the existing row. Use the pre-fetched locked row if available;
		// otherwise fall back to a plain DB read.
		if existing == nil {
			var err error
			existing, err = rs.taskScheduleStore.Get(ctx, id)
			if err != nil {
				return fields, false, err
			}
		}

		if len(fields.SpecType) > 0 {
			// "schedule.spec" masked only: use new spec values with existing timezone.
			recompute = &struct {
				specType dbmodel.SpecType
				spec     string
				timezone string
			}{fields.SpecType, fields.Spec, existing.Timezone}
		} else if existing.SpecType == dbmodel.SpecTypeCron {
			// "schedule.spec.timezone" masked only: timezone is irrelevant for
			// interval and one-time specs; only recompute for cron.
			recompute = &struct {
				specType dbmodel.SpecType
				spec     string
				timezone string
			}{existing.SpecType, existing.Spec, fields.Timezone}
		} else {
			// Non-cron + timezone-only change: timezone is meaningless, don't store it.
			fields.Timezone = ""
		}
	} else if len(fields.SpecType) > 0 {
		// Both "schedule.spec" and "schedule.spec.timezone" masked: all inputs are from the request.
		if fields.SpecType != dbmodel.SpecTypeCron {
			// Timezone is irrelevant for non-cron specs, don't store it.
			fields.Timezone = ""
		}
		recompute = &struct {
			specType dbmodel.SpecType
			spec     string
			timezone string
		}{fields.SpecType, fields.Spec, fields.Timezone}
	}

	if recompute != nil {
		next, err := taskschedule.ComputeFirstRunAt(
			recompute.specType,
			recompute.spec,
			recompute.timezone,
		)
		if err != nil {
			return fields, false, fmt.Errorf("compute next_run_at: %w", err)
		}

		fields.NextRunAt = &next

		return fields, true, nil
	}

	return fields, false, nil
}

// resolveScope converts an internal TargetSpec into DB scope rows ready for
// insertion (ScheduleID is not yet set). Rack and NVLink domain targets may
// include a component-type filter; component targets identify specific
// components. Domain membership is materialized to rack scopes when this method
// runs, so later membership changes do not silently change an existing schedule.
func (rs *FlowServerImpl) resolveScope(
	ctx context.Context,
	ts operation.TargetSpec,
) ([]*dbmodel.TaskScheduleScope, error) {
	if ts.IsRackTargeting() {
		return rs.resolveRackScope(ctx, ts.Racks)
	}
	if ts.IsNVLDomainTargeting() {
		return rs.resolveNVLDomainScope(ctx, ts.NVLDomains)
	}

	return rs.resolveComponentScope(ctx, ts.Components)
}

func (rs *FlowServerImpl) resolveNVLDomainScope(
	ctx context.Context,
	domains []operation.NVLDomainTarget,
) ([]*dbmodel.TaskScheduleScope, error) {
	rackTargets, err := inventoryresolver.ResolveNVLDomainRackTargets(
		ctx,
		rs.inventoryManager,
		domains,
	)
	if err != nil {
		return nil, fmt.Errorf("target_spec.nvl_domains: %w", err)
	}

	return rs.resolveRackScope(ctx, rackTargets)
}

// resolveScheduleScope is the shared prologue for AddTaskScheduleScope and
// UpdateTaskScheduleScope. It verifies the schedule exists and resolves the
// target spec into scope rows with ScheduleID stamped.
//
// It does NOT read the current scope list — that read happens inside
// applyScopeChanges under a row lock to prevent concurrent requests from
// diffing against a stale snapshot.
func (rs *FlowServerImpl) resolveScheduleScope(
	ctx context.Context,
	scheduleID uuid.UUID,
	targetSpec *pb.OperationTargetSpec,
) ([]*dbmodel.TaskScheduleScope, error) {
	if _, err := rs.taskScheduleStore.Get(ctx, scheduleID); err != nil {
		return nil, fmt.Errorf("get schedule: %w", err)
	}

	ts, err := protobuf.TargetSpecFrom(targetSpec)
	if err != nil {
		return nil, fmt.Errorf("invalid target_spec: %w", err)
	}

	resolved, err := rs.resolveScope(ctx, ts)
	if err != nil {
		return nil, err
	}

	for i := range resolved {
		resolved[i].ScheduleID = scheduleID
	}

	return resolved, nil
}

// scopeDiffFn computes which scope rows to create, remove, and update given
// the incoming desired rows and the current DB state read under a row lock.
type scopeDiffFn func(
	incoming, existing []*dbmodel.TaskScheduleScope,
) (toCreate, toRemove, toUpdate []*dbmodel.TaskScheduleScope, _ error)

// applyScopeChanges locks the schedule row, re-reads the current scope set
// under the lock, runs diff to compute mutations, and applies them — all in a
// single transaction. Locking the schedule row serializes concurrent scope
// mutations and prevents the read-diff-write window that would otherwise allow
// two concurrent requests to diff against the same stale snapshot.
func (rs *FlowServerImpl) applyScopeChanges(
	ctx context.Context,
	scheduleID uuid.UUID,
	incoming []*dbmodel.TaskScheduleScope,
	diff scopeDiffFn,
) (toCreate, toRemove, toUpdate, finalScopes []*dbmodel.TaskScheduleScope, _ error) {
	err := rs.taskScheduleStore.RunInTransaction(
		ctx,
		func(ctx context.Context) error {
			// Lock the schedule row to serialize concurrent scope mutations.
			// LockForTrigger uses SELECT ... FOR UPDATE (blocking, no SKIP LOCKED),
			// so a concurrent Add/Update/Remove waits here rather than diffing
			// against a stale snapshot.
			_, err := rs.taskScheduleStore.LockForTrigger(ctx, scheduleID)
			if err != nil {
				return fmt.Errorf("lock schedule: %w", err)
			}

			// Re-read scopes under the lock — this is the authoritative current state.
			existing, err := rs.taskScheduleStore.ListScopes(ctx, scheduleID)
			if err != nil {
				return fmt.Errorf("list scopes: %w", err)
			}

			toCreate, toRemove, toUpdate, err = diff(incoming, existing)
			if err != nil {
				return err
			}

			// Enforce the non-empty-scope invariant before any writes: a diff
			// that removes all remaining scopes would leave the schedule in the
			// same invalid state the create path already rejects.
			if remaining := len(existing) - len(toRemove) + len(toCreate); remaining == 0 {
				return errors.New("target_spec resolved to no rack scopes; at least one rack target is required")
			}

			if len(toCreate) > 0 {
				if err := rs.taskScheduleStore.CreateScopes(ctx, toCreate); err != nil {
					return fmt.Errorf("create scopes: %w", err)
				}
			}

			for _, s := range toRemove {
				if err := rs.taskScheduleStore.DeleteScope(ctx, s.ID); err != nil {
					return fmt.Errorf("delete scope %s: %w", s.ID, err)
				}
			}

			for _, s := range toUpdate {
				if err := rs.taskScheduleStore.UpdateScopeComponentFilter(
					ctx, s.ID, s.ComponentFilter,
				); err != nil {
					return fmt.Errorf("update scope %s: %w", s.ID, err)
				}
			}

			// Read the final scope list inside the transaction so the returned
			// snapshot is consistent with the change counts. Reading after commit
			// would allow a concurrent mutation to slip in between, making Scopes
			// disagree with Added/Removed/Updated.
			finalScopes, err = rs.taskScheduleStore.ListScopes(ctx, scheduleID)
			if err != nil {
				return fmt.Errorf("list final scopes: %w", err)
			}

			return nil
		},
	)

	if err != nil {
		return nil, nil, nil, nil, err
	}

	return toCreate, toRemove, toUpdate, finalScopes, nil
}

// resolveRackScope converts rack-level targets into scope rows.
// Each RackTarget becomes one scope row; an optional ComponentTypes filter is
// stored as a kind="types" component_filter.
func (rs *FlowServerImpl) resolveRackScope(
	ctx context.Context,
	racks []operation.RackTarget,
) ([]*dbmodel.TaskScheduleScope, error) {
	seen := make(map[uuid.UUID]struct{}, len(racks))
	scopes := make([]*dbmodel.TaskScheduleScope, 0, len(racks))

	for i, rt := range racks {
		r, err := rs.inventoryManager.GetRackByIdentifier(ctx, rt.Identifier, false)
		if err != nil {
			return nil, fmt.Errorf("target_spec.racks[%d]: resolve rack: %w", i, err)
		}

		scope := &dbmodel.TaskScheduleScope{
			RackID: r.Info.ID,
		}

		if _, ok := seen[scope.RackID]; ok {
			// Each scope row maps one-to-one with a rack. A duplicate would
			// produce two scope rows for the same rack, firing two tasks per
			// trigger — almost certainly a caller mistake.
			return nil, fmt.Errorf("target_spec: duplicate rack %s", scope.RackID)
		}
		seen[scope.RackID] = struct{}{}

		if len(rt.ComponentTypes) > 0 {
			types := make([]string, len(rt.ComponentTypes))
			for j, ct := range rt.ComponentTypes {
				types[j] = devicetypes.ComponentTypeToString(ct)
			}

			cfRaw, err := dbmodel.MarshalComponentFilter(
				&dbmodel.ComponentFilter{
					Kind:  dbmodel.ComponentFilterKindTypes,
					Types: types,
				},
			)

			if err != nil {
				return nil, fmt.Errorf("target_spec.racks[%d]: marshal component filter: %w", i, err)
			}

			scope.ComponentFilter = cfRaw
		}

		scopes = append(scopes, scope)
	}

	return scopes, nil
}

// resolveComponentTarget resolves a single ComponentTarget to its component
// UUID and rack UUID via inventory lookup.
func (rs *FlowServerImpl) resolveComponentTarget(
	ctx context.Context,
	ct operation.ComponentTarget,
) (uuid.UUID, uuid.UUID, error) {
	if ct.UUID == uuid.Nil && ct.External == nil {
		return uuid.Nil, uuid.Nil, fmt.Errorf(
			"component target has neither UUID nor external ref",
		)
	}

	// UUID targeting.
	if ct.UUID != uuid.Nil {
		comp, err := rs.inventoryManager.GetComponentByID(ctx, ct.UUID)
		if err != nil {
			return uuid.Nil, uuid.Nil, fmt.Errorf(
				"resolve component %s: %w",
				ct.UUID, err,
			)
		}

		return ct.UUID, comp.RackID, nil
	}

	resolved, err := inventoryresolver.ResolveComponentIdentifier(
		ctx,
		rs.inventoryManager,
		ct.External.ID,
		ct.External.Type,
	)
	if err != nil {
		return uuid.Nil, uuid.Nil, err
	}
	return resolved.Info.ID, resolved.RackID, nil
}

// resolveComponentScope resolves component-level targets to their racks, groups
// components by rack, and returns one scope row per rack with a kind="components"
// filter.
func (rs *FlowServerImpl) resolveComponentScope(
	ctx context.Context,
	targets []operation.ComponentTarget,
) ([]*dbmodel.TaskScheduleScope, error) {
	// Map rack_id → ordered list of component UUIDs for that rack.
	rackOrder := make([]uuid.UUID, 0) // preserves insertion order for deterministic output
	rackComponents := make(map[uuid.UUID][]uuid.UUID)

	// Duplicate tracking by resolved component UUID. A single map covers all
	// three duplication forms: same UUID twice, same external ID twice, and a
	// UUID and an external ref that resolve to the same component.
	seenComponentIDs := make(map[uuid.UUID]struct{})

	for _, ct := range targets {
		compID, rackID, err := rs.resolveComponentTarget(ctx, ct)
		if err != nil {
			return nil, err
		}

		if _, ok := seenComponentIDs[compID]; ok {
			return nil, fmt.Errorf(
				"target_spec: duplicate component %s",
				compID,
			)
		}

		seenComponentIDs[compID] = struct{}{}

		if _, exists := rackComponents[rackID]; !exists {
			rackOrder = append(rackOrder, rackID)
		}

		rackComponents[rackID] = append(rackComponents[rackID], compID)
	}

	scopes := make([]*dbmodel.TaskScheduleScope, 0, len(rackOrder))
	for _, rackID := range rackOrder {
		cfRaw, err := dbmodel.MarshalComponentFilter(
			&dbmodel.ComponentFilter{
				Kind:       dbmodel.ComponentFilterKindComponents,
				Components: rackComponents[rackID],
			},
		)
		if err != nil {
			return nil, fmt.Errorf(
				"marshal component filter for rack %s: %w",
				rackID, err,
			)
		}

		scopes = append(scopes, &dbmodel.TaskScheduleScope{
			RackID:          rackID,
			ComponentFilter: cfRaw,
		},
		)
	}

	return scopes, nil
}

// taskScheduleToProto converts a DB row to its proto representation.
// Scopes are not embedded; use ListTaskScheduleScopes to query them separately.
func taskScheduleToProto(row *dbmodel.TaskSchedule) (*pb.TaskSchedule, error) {
	pbSched := &pb.TaskSchedule{
		Id:            protobuf.UUIDTo(row.ID),
		Name:          row.Name,
		OverlapPolicy: modelOverlapPolicyToProto(row.OverlapPolicy),
		Enabled:       row.Enabled,
		Spec: &pb.ScheduleSpec{
			Type:     modelSpecTypeToProto(row.SpecType),
			Spec:     row.Spec,
			Timezone: row.Timezone,
		},
		CreatedAt: timestamppb.New(row.CreatedAt),
		UpdatedAt: timestamppb.New(row.UpdatedAt),
	}

	if row.NextRunAt != nil {
		pbSched.NextRunAt = timestamppb.New(*row.NextRunAt)
	}
	if row.LastRunAt != nil {
		pbSched.LastRunAt = timestamppb.New(*row.LastRunAt)
	}

	if len(row.OperationTemplate) > 0 {
		opType, desc, err := taskschedule.SummaryFromTemplate(row.OperationTemplate)
		if err != nil {
			return nil, fmt.Errorf("build operation summary: %w", err)
		}
		pbSched.OperationType = opType
		pbSched.Description = desc
	}

	return pbSched, nil
}

// partitionScopeChanges implements additive (merge) semantics for
// AddTaskScheduleScope: incoming rows are folded into the existing scope
// without removing any rack.
//   - toCreate: racks not yet present in existing
//   - toMerge: racks already present whose component filter expands after
//     merging; each returned scope has ComponentFilter set to the merged value
//
// Incoming rows whose rack already has an identical (or broader) filter are
// silently dropped — they are no-ops.
//
// For reconciling (replace) semantics see diffScopeChanges.
func partitionScopeChanges(
	incoming []*dbmodel.TaskScheduleScope,
	existing []*dbmodel.TaskScheduleScope,
) (toCreate, toMerge []*dbmodel.TaskScheduleScope, _ error) {
	existingByRack := make(map[uuid.UUID]*dbmodel.TaskScheduleScope, len(existing)) //nolint:lll
	for _, s := range existing {
		existingByRack[s.RackID] = s
	}

	for _, ns := range incoming {
		cur, exists := existingByRack[ns.RackID]
		if !exists {
			// New scope entry, add it to the create list.
			toCreate = append(toCreate, ns)
			continue
		}

		// Existing scope entry, merge the filters.
		merged, changed, err := mergeComponentFilters(
			cur.ComponentFilter, ns.ComponentFilter,
		)
		if err != nil {
			return nil, nil, fmt.Errorf(
				"merge scope for rack %s: %w", ns.RackID, err,
			)
		}

		if changed {
			cur.ComponentFilter = merged
			toMerge = append(toMerge, cur)
		}
	}

	return toCreate, toMerge, nil
}

// diffScopeChanges implements reconciling (replace) semantics for
// UpdateTaskScheduleScope: the current scope is driven to exactly match the
// desired scope.
//   - toAdd: desired entries whose rack has no current row
//   - toRemove: current entries whose rack is absent from desired (rack removed)
//   - toUpdate: current entries whose ComponentFilter differs from desired;
//     each returned scope has ComponentFilter set to the desired value
//
// For additive (merge) semantics see partitionScopeChanges.
func diffScopeChanges(
	desired, current []*dbmodel.TaskScheduleScope,
) (toAdd, toRemove, toUpdate []*dbmodel.TaskScheduleScope, _ error) {
	desiredByRack := make(map[uuid.UUID]*dbmodel.TaskScheduleScope, len(desired))
	for _, s := range desired {
		desiredByRack[s.RackID] = s
	}

	currentByRack := make(map[uuid.UUID]*dbmodel.TaskScheduleScope, len(current))
	for _, s := range current {
		currentByRack[s.RackID] = s
	}

	for _, d := range desired {
		cur, exists := currentByRack[d.RackID]
		if !exists {
			toAdd = append(toAdd, d)
			continue
		}

		equal, err := dbmodel.ComponentFilterEqual(
			cur.ComponentFilter, d.ComponentFilter,
		)
		if err != nil {
			return nil, nil, nil, fmt.Errorf(
				"compare scope for rack %s: %w", d.RackID, err,
			)
		}

		if !equal {
			cur.ComponentFilter = d.ComponentFilter
			toUpdate = append(toUpdate, cur)
		}
	}

	for _, cur := range current {
		if _, keep := desiredByRack[cur.RackID]; !keep {
			toRemove = append(toRemove, cur)
		}
	}

	return toAdd, toRemove, toUpdate, nil
}

// unionSlice returns the union of a and b, preserving the order of a followed
// by any elements of b not already in a. changed is true if b contributed at
// least one new element.
func unionSlice[T comparable](a, b []T) (union []T, changed bool) {
	seen := make(map[T]struct{}, len(a))
	union = make([]T, 0, len(a)+len(b))

	for _, v := range a {
		seen[v] = struct{}{}
		union = append(union, v)
	}

	for _, v := range b {
		if _, ok := seen[v]; !ok {
			seen[v] = struct{}{}
			union = append(union, v)
			changed = true
		}
	}

	return union, changed
}

// mergeComponentFilters returns the union of two component_filter JSONB
// values and whether the result differs from a (the existing filter).
// A nil filter (meaning "all components") absorbs any other filter.
// Returns an error when the filters have incompatible kinds.
func mergeComponentFilters(
	a, b json.RawMessage,
) (json.RawMessage, bool, error) {
	cfA, err := dbmodel.UnmarshalComponentFilter(a)
	if err != nil {
		return nil, false, fmt.Errorf("unmarshal existing filter: %w", err)
	}

	cfB, err := dbmodel.UnmarshalComponentFilter(b)
	if err != nil {
		return nil, false, fmt.Errorf("unmarshal new filter: %w", err)
	}

	// nil means "all components" — absorbs any narrower filter.
	// changed only when cfA was not already nil.
	if cfA == nil || cfB == nil {
		return nil, cfA != nil, nil
	}

	if cfA.Kind != cfB.Kind {
		// Callers receive this error and surface it directly to the client,
		// so no further handling is needed here.
		return nil, false, fmt.Errorf(
			"cannot merge filters of different kinds (%s vs %s)",
			cfA.Kind, cfB.Kind,
		)
	}

	var (
		changed bool
		merged  *dbmodel.ComponentFilter
	)

	switch cfA.Kind {
	case dbmodel.ComponentFilterKindTypes:
		types, c := unionSlice(cfA.Types, cfB.Types)
		changed = c
		merged = &dbmodel.ComponentFilter{
			Kind:  dbmodel.ComponentFilterKindTypes,
			Types: types,
		}

	case dbmodel.ComponentFilterKindComponents:
		components, c := unionSlice(cfA.Components, cfB.Components)
		changed = c
		merged = &dbmodel.ComponentFilter{
			Kind:       dbmodel.ComponentFilterKindComponents,
			Components: components,
		}

	default:
		return nil, false, fmt.Errorf("unknown component filter kind: %s", cfA.Kind)
	}

	raw, err := dbmodel.MarshalComponentFilter(merged)
	if err != nil {
		return nil, false, fmt.Errorf("marshal merged filter: %w", err)
	}

	return raw, changed, nil
}

// taskScheduleScopeToProto converts a DB scope row to its proto representation.
func taskScheduleScopeToProto(
	s *dbmodel.TaskScheduleScope,
) (*pb.TaskScheduleScope, error) {
	pbScope := &pb.TaskScheduleScope{
		Id:         protobuf.UUIDTo(s.ID),
		ScheduleId: protobuf.UUIDTo(s.ScheduleID),
		RackId:     protobuf.UUIDTo(s.RackID),
		CreatedAt:  timestamppb.New(s.CreatedAt),
	}
	if s.LastTaskID != nil {
		pbScope.LastTaskId = protobuf.UUIDTo(*s.LastTaskID)
	}

	// Populate component_filter oneof.
	cf, err := dbmodel.UnmarshalComponentFilter(s.ComponentFilter)
	if err != nil {
		return nil, fmt.Errorf(
			"unmarshal component filter for scope %s: %w", s.ID, err,
		)
	}

	if cf != nil {
		switch cf.Kind {
		case dbmodel.ComponentFilterKindTypes:
			pbTypes := make([]pb.ComponentType, 0, len(cf.Types))
			for _, t := range cf.Types {
				ct := devicetypes.ComponentTypeFromString(t)
				pbTypes = append(pbTypes, protobuf.ComponentTypeTo(ct))
			}
			pbScope.ComponentFilter = &pb.TaskScheduleScope_Types{
				Types: &pb.ComponentTypes{Types: pbTypes},
			}
		case dbmodel.ComponentFilterKindComponents:
			targets := make([]*pb.ComponentTarget, 0, len(cf.Components))
			for _, id := range cf.Components {
				targets = append(
					targets,
					&pb.ComponentTarget{
						Identifier: &pb.ComponentTarget_Id{
							Id: protobuf.UUIDTo(id),
						},
					},
				)
			}

			pbScope.ComponentFilter = &pb.TaskScheduleScope_Components{
				Components: &pb.ComponentTargets{Targets: targets},
			}
		default:
			return nil, fmt.Errorf(
				"scope %s has unrecognised component_filter kind %q",
				s.ID, cf.Kind,
			)
		}
	}

	return pbScope, nil
}

// ─── enum converters ─────────────────────────────────────────────────────────

func protoSpecTypeToModel(t pb.ScheduleSpecType) dbmodel.SpecType {
	switch t {
	case pb.ScheduleSpecType_SCHEDULE_SPEC_TYPE_INTERVAL:
		return dbmodel.SpecTypeInterval
	case pb.ScheduleSpecType_SCHEDULE_SPEC_TYPE_CRON:
		return dbmodel.SpecTypeCron
	case pb.ScheduleSpecType_SCHEDULE_SPEC_TYPE_ONE_TIME:
		return dbmodel.SpecTypeOneTime
	default:
		return ""
	}
}

func modelSpecTypeToProto(t dbmodel.SpecType) pb.ScheduleSpecType {
	switch t {
	case dbmodel.SpecTypeInterval:
		return pb.ScheduleSpecType_SCHEDULE_SPEC_TYPE_INTERVAL
	case dbmodel.SpecTypeCron:
		return pb.ScheduleSpecType_SCHEDULE_SPEC_TYPE_CRON
	case dbmodel.SpecTypeOneTime:
		return pb.ScheduleSpecType_SCHEDULE_SPEC_TYPE_ONE_TIME
	default:
		return pb.ScheduleSpecType_SCHEDULE_SPEC_TYPE_UNSPECIFIED
	}
}

func protoOverlapPolicyToModel(p pb.OverlapPolicy) dbmodel.OverlapPolicy {
	switch p {
	case pb.OverlapPolicy_OVERLAP_POLICY_QUEUE:
		return dbmodel.OverlapPolicyQueue
	default:
		return dbmodel.OverlapPolicySkip
	}
}

func modelOverlapPolicyToProto(p dbmodel.OverlapPolicy) pb.OverlapPolicy {
	switch p {
	case dbmodel.OverlapPolicyQueue:
		return pb.OverlapPolicy_OVERLAP_POLICY_QUEUE
	default:
		return pb.OverlapPolicy_OVERLAP_POLICY_SKIP
	}
}
