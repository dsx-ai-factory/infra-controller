// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package store

import (
	"context"
	"database/sql"
	"encoding/json"
	"fmt"

	"github.com/google/uuid"
	"github.com/uptrace/bun"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/converter/dao"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/model"
	dbquery "github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/query"
	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operationrules"
	taskdef "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/task"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/errors"
)

// txKeyType is an unexported type for the transaction context key.
// Using a dedicated type prevents collisions with other context keys.
type txKeyType struct{}

var txKey = txKeyType{}

// PostgresStore implements the Store interface using PostgreSQL.
type PostgresStore struct {
	pg *cdb.Session
}

// NewPostgres creates a new PostgreSQL-backed task store.
func NewPostgres(pg *cdb.Session) *PostgresStore {
	return &PostgresStore{pg: pg}
}

// idb returns the bun.Tx stored in ctx by RunInTransaction, falling back to
// the underlying *bun.DB when called outside a transaction.
func (s *PostgresStore) idb(ctx context.Context) bun.IDB {
	if tx, ok := ctx.Value(txKey).(bun.Tx); ok {
		return tx
	}
	return s.pg.DB
}

// RunInTransaction executes fn within a database transaction, propagating
// the transaction through the context so nested store calls participate.
func (s *PostgresStore) RunInTransaction(
	ctx context.Context,
	fn func(ctx context.Context) error,
) error {
	return s.pg.RunInTx(ctx, func(ctx context.Context, tx bun.Tx) error {
		return fn(context.WithValue(ctx, txKey, tx))
	})
}

// CreateTask creates a new task record. Participates in a surrounding
// transaction if one was started by RunInTransaction.
func (s *PostgresStore) CreateTask(
	ctx context.Context,
	task *taskdef.Task,
) error {
	if err := dao.TaskTo(task).Create(ctx, s.idb(ctx)); err != nil {
		return errors.GRPCErrorInternal(err.Error())
	}

	return nil
}

// LockRack takes a transaction-scoped advisory lock for rack-level task
// admission. The lock is released automatically when the transaction ends.
func (s *PostgresStore) LockRack(ctx context.Context, rackID uuid.UUID) error {
	if rackID == uuid.Nil {
		return errors.GRPCErrorInvalidArgument("rack ID is required")
	}
	return s.lockAdvisoryKey(ctx, rackID.String())
}

// LockIdempotencyKey takes a transaction-scoped advisory lock for stable
// submission lookup and resume. The lock is released automatically when the
// transaction ends.
func (s *PostgresStore) LockIdempotencyKey(ctx context.Context, key string) error {
	if key == "" {
		return errors.GRPCErrorInvalidArgument("idempotency key is required")
	}
	return s.lockAdvisoryKey(ctx, key)
}

func (s *PostgresStore) lockAdvisoryKey(ctx context.Context, key string) error {
	tx, ok := ctx.Value(txKey).(bun.Tx)
	if !ok {
		return errors.GRPCErrorInternal("advisory lock requires an active transaction")
	}

	if _, err := tx.
		NewSelect().
		ColumnExpr("pg_advisory_xact_lock(hashtextextended(?, 0))", key).
		Exec(ctx); err != nil {
		return errors.GRPCErrorInternal(err.Error())
	}
	return nil
}

// GetTaskByIdempotencyKey retrieves the existing task for a stable submission
// key. A missing key returns nil so callers can continue normal creation.
func (s *PostgresStore) GetTaskByIdempotencyKey(
	ctx context.Context,
	key string,
) (*taskdef.Task, error) {
	taskDao, err := model.GetTaskByIdempotencyKey(ctx, s.idb(ctx), key)
	if err != nil {
		if s.pg.GetErrorChecker().IsErrNoRows(err) {
			return nil, nil
		}
		return nil, errors.GRPCErrorInternal(err.Error())
	}

	return taskFromDAO(taskDao)
}

// GetTask retrieves a single task by its ID.
func (s *PostgresStore) GetTask(
	ctx context.Context,
	id uuid.UUID,
) (*taskdef.Task, error) {
	taskDao, err := model.GetTask(ctx, s.idb(ctx), id)
	if err != nil {
		return nil, errors.GRPCErrorInternal(err.Error())
	}
	return taskFromDAO(taskDao)
}

// GetTasks retrieves tasks by their IDs.
func (s *PostgresStore) GetTasks(
	ctx context.Context,
	ids []uuid.UUID,
) ([]*taskdef.Task, error) {
	results := make([]*taskdef.Task, 0, len(ids))
	for _, id := range ids {
		if id == uuid.Nil {
			results = append(results, nil)
			continue
		}

		taskDao, err := model.GetTask(ctx, s.pg.DB, id)
		if err != nil {
			return nil, errors.GRPCErrorInternal(err.Error())
		}

		converted, err := taskFromDAO(taskDao)
		if err != nil {
			return nil, err
		}
		results = append(results, converted)
	}

	return results, nil
}

// ListTasks lists tasks matching the given criteria.
func (s *PostgresStore) ListTasks(
	ctx context.Context,
	options *taskcommon.TaskListOptions,
	pagination *dbquery.Pagination,
) ([]*taskdef.Task, int32, error) {
	taskDaos, total, err := model.ListTasks(ctx, s.pg.DB, options, pagination)
	if err != nil {
		return nil, 0, errors.GRPCErrorInternal(err.Error())
	}

	results := make([]*taskdef.Task, 0, len(taskDaos))
	for _, taskDao := range taskDaos {
		converted, err := taskFromDAO(&taskDao)
		if err != nil {
			return nil, 0, err
		}
		results = append(results, converted)
	}

	return results, total, nil
}

// ListNonTerminalTasksForRacks returns Waiting, Pending, and Running tasks for
// the requested racks.
func (s *PostgresStore) ListNonTerminalTasksForRacks(
	ctx context.Context,
	rackIDs []uuid.UUID,
) ([]*taskdef.Task, error) {
	taskDaos, err := model.ListTasksForRacksByStatus(
		ctx,
		s.pg.DB,
		rackIDs,
		taskcommon.NonTerminalTaskStatuses(),
	)
	if err != nil {
		return nil, errors.GRPCErrorInternal(err.Error())
	}

	results := make([]*taskdef.Task, 0, len(taskDaos))
	for i := range taskDaos {
		converted, err := taskFromDAO(&taskDaos[i])
		if err != nil {
			return nil, err
		}
		results = append(results, converted)
	}
	return results, nil
}

// UpdateScheduledTask updates task scheduling information.
func (s *PostgresStore) UpdateScheduledTask(
	ctx context.Context,
	task *taskdef.Task,
) error {
	taskDao := dao.TaskTo(task)
	if err := taskDao.UpdateScheduledTask(ctx, s.idb(ctx)); err != nil {
		return errors.GRPCErrorInternal(err.Error())
	}

	return nil
}

// UpdateTaskStatus persists status and message, plus optional report and queue
// deadline changes. Finished statuses clear the queue deadline; otherwise nil
// optional values leave their stored columns untouched. No read-modify-write is
// performed, so concurrent transitions cannot lose updates.
func (s *PostgresStore) UpdateTaskStatus(
	ctx context.Context,
	arg *taskdef.TaskStatusUpdate,
) error {
	taskDao := &model.Task{ID: arg.ID}
	err := taskDao.UpdateTaskStatus(
		ctx,
		s.idb(ctx),
		arg.Status,
		arg.Message,
		arg.Report,
		arg.QueueExpiresAt,
	)
	if err != nil {
		return errors.GRPCErrorInternal(err.Error())
	}

	return nil
}

// UpdateTaskReport replaces the stored report with the snapshot in arg.
// Empty snapshots are dropped rather than written so a malformed caller
// cannot clear the stored report by accident.
func (s *PostgresStore) UpdateTaskReport(
	ctx context.Context,
	arg *taskdef.TaskReportUpdate,
) error {
	if len(arg.Report) == 0 {
		return nil
	}

	taskDao := &model.Task{ID: arg.ID}
	err := taskDao.UpdateTaskReport(ctx, s.idb(ctx), arg.Report)
	if err != nil {
		return errors.GRPCErrorInternal(err.Error())
	}

	return nil
}

// ListActiveTasksForRack returns pending and running tasks for the given rack.
func (s *PostgresStore) ListActiveTasksForRack(
	ctx context.Context,
	rackID uuid.UUID,
) ([]*taskdef.Task, error) {
	tasks, err := model.ListTasksForRackByStatus(
		ctx, s.idb(ctx), rackID,
		[]taskcommon.TaskStatus{
			taskcommon.TaskStatusPending,
			taskcommon.TaskStatusRunning,
		},
	)
	if err != nil {
		return nil, errors.GRPCErrorInternal(err.Error())
	}

	result := make([]*taskdef.Task, len(tasks))
	for i := range tasks {
		converted, err := taskFromDAO(&tasks[i])
		if err != nil {
			return nil, err
		}
		result[i] = converted
	}
	return result, nil
}

// ListWaitingTasksForRack returns waiting tasks for the rack, oldest first.
func (s *PostgresStore) ListWaitingTasksForRack(
	ctx context.Context,
	rackID uuid.UUID,
) ([]*taskdef.Task, error) {
	tasks, err := model.ListTasksForRackByStatus(
		ctx, s.idb(ctx), rackID,
		[]taskcommon.TaskStatus{taskcommon.TaskStatusWaiting},
	)
	if err != nil {
		return nil, errors.GRPCErrorInternal(err.Error())
	}

	result := make([]*taskdef.Task, len(tasks))
	for i := range tasks {
		converted, err := taskFromDAO(&tasks[i])
		if err != nil {
			return nil, err
		}
		result[i] = converted
	}
	return result, nil
}

func taskFromDAO(taskDAO *model.Task) (*taskdef.Task, error) {
	converted, err := dao.TaskFrom(taskDAO)
	if err != nil {
		return nil, errors.GRPCErrorInternal(err.Error())
	}

	return converted, nil
}

// ListRacksWithWaitingTasks returns distinct rack IDs with waiting tasks.
func (s *PostgresStore) ListRacksWithWaitingTasks(
	ctx context.Context,
) ([]uuid.UUID, error) {
	rackIDs, err := model.ListRacksWithWaitingTasks(ctx, s.idb(ctx))
	if err != nil {
		return nil, errors.GRPCErrorInternal(err.Error())
	}
	return rackIDs, nil
}

// CountWaitingTasksForRack returns the count of waiting tasks for the rack.
func (s *PostgresStore) CountWaitingTasksForRack(
	ctx context.Context,
	rackID uuid.UUID,
) (int, error) {
	count, err := model.CountWaitingTasksForRack(ctx, s.idb(ctx), rackID)
	if err != nil {
		return 0, errors.GRPCErrorInternal(err.Error())
	}
	return count, nil
}

// ========================================
// Operation Rule Methods
// ========================================

// CreateRule creates a new operation rule
func (s *PostgresStore) CreateRule(
	ctx context.Context,
	rule *operationrules.OperationRule,
) error {
	dbModel, err := dao.OperationRuleTo(rule)
	if err != nil {
		return errors.GRPCErrorInvalidArgument(err.Error())
	}

	if err := dbModel.Create(ctx, s.pg.DB); err != nil {
		if s.pg.GetErrorChecker().IsUniqueConstraintError(err) {
			return errors.GRPCErrorAlreadyExists(
				"a default rule already exists for this operation type",
			)
		}
		return errors.GRPCErrorInternal(err.Error())
	}

	rule.CreatedAt = dbModel.CreatedAt
	rule.UpdatedAt = dbModel.UpdatedAt

	return nil
}

// UpdateRule updates specific fields of an operation rule
func (s *PostgresStore) UpdateRule(
	ctx context.Context,
	id uuid.UUID,
	updates map[string]interface{},
) error {
	if len(updates) == 0 {
		return nil
	}

	dbModel, err := model.GetOperationRule(ctx, s.pg.DB, id)
	if err != nil {
		if s.pg.GetErrorChecker().IsErrNoRows(err) {
			return errors.GRPCErrorNotFound("operation rule not found")
		}
		return errors.GRPCErrorInternal(err.Error())
	}

	columns := make([]string, 0, len(updates))
	for key, value := range updates {
		switch key {
		case "name":
			if v, ok := value.(string); ok {
				dbModel.Name = v
				columns = append(columns, "name")
			}
		case "description":
			if v, ok := value.(string); ok {
				dbModel.Description = sql.NullString{String: v, Valid: true}
				columns = append(columns, "description")
			}
		case "rule_definition":
			if ruleDef, ok := value.(operationrules.RuleDefinition); ok {
				ruleDefJSON, err := operationrules.MarshalRuleDefinition(ruleDef)
				if err != nil {
					return errors.GRPCErrorInvalidArgument(fmt.Sprintf("invalid rule definition: %v", err))
				}
				dbModel.RuleDefinition = json.RawMessage(ruleDefJSON)
				columns = append(columns, "rule_definition")
			}
		case "is_default":
			if v, ok := value.(bool); ok {
				dbModel.IsDefault = v
				columns = append(columns, "is_default")
			}
		}
	}

	if len(columns) == 0 {
		return nil
	}

	if err := dbModel.Update(ctx, s.pg.DB, columns...); err != nil {
		return errors.GRPCErrorInternal(fmt.Sprintf("failed to update operation rule: %v", err))
	}

	return nil
}

// DeleteRule deletes an operation rule by ID
func (s *PostgresStore) DeleteRule(
	ctx context.Context,
	id uuid.UUID,
) error {
	dbModel := &model.OperationRule{ID: id}
	if err := dbModel.Delete(ctx, s.pg.DB); err != nil {
		return errors.GRPCErrorInternal(
			fmt.Sprintf("failed to delete operation rule: %v", err),
		)
	}
	return nil
}

// SetRuleAsDefault sets a rule as the default for its operation.
// Automatically unsets any existing default for the same (operation_type, operation_code).
func (s *PostgresStore) SetRuleAsDefault(
	ctx context.Context,
	id uuid.UUID,
) error {
	return s.pg.RunInTx(ctx, func(ctx context.Context, tx bun.Tx) error {
		// Get the rule to find its operation_type and operation_code
		rule, err := model.GetOperationRule(ctx, tx, id)
		if err != nil {
			return errors.GRPCErrorNotFound(
				fmt.Sprintf("operation rule not found: %v", err),
			)
		}
		if rule == nil {
			return errors.GRPCErrorNotFound(
				fmt.Sprintf("operation rule not found: %v", id),
			)
		}

		// Unset all defaults for this (operation_type, operation_code)
		_, err = tx.NewUpdate().
			TableExpr("operation_rules").
			Set("is_default = ?", false).
			Where("operation_type = ?", rule.OperationType).
			Where("operation_code = ?", rule.OperationCode).
			Where("is_default = ?", true).
			Exec(ctx)
		if err != nil {
			return errors.GRPCErrorInternal(
				fmt.Sprintf("failed to unset existing defaults: %v", err),
			)
		}

		// Set this rule as default
		_, err = tx.NewUpdate().
			TableExpr("operation_rules").
			Set("is_default = ?", true).
			Where("id = ?", id).
			Exec(ctx)
		if err != nil {
			return errors.GRPCErrorInternal(
				fmt.Sprintf("failed to set rule as default: %v", err),
			)
		}

		return nil
	})
}

// GetRule retrieves an operation rule by ID
func (s *PostgresStore) GetRule(
	ctx context.Context,
	id uuid.UUID,
) (*operationrules.OperationRule, error) {
	dbModel, err := model.GetOperationRule(ctx, s.pg.DB, id)
	if err != nil {
		if s.pg.GetErrorChecker().IsErrNoRows(err) {
			return nil, errors.GRPCErrorNotFound("operation rule not found")
		}
		return nil, errors.GRPCErrorInternal(fmt.Sprintf("failed to get operation rule: %v", err))
	}

	return dao.OperationRuleFrom(dbModel)
}

// GetRuleByOperationAndRack retrieves the appropriate rule for an operation code and rack.
// Resolution order: rack association > default rule
func (s *PostgresStore) GetRuleByOperationAndRack(
	ctx context.Context,
	opType taskcommon.TaskType,
	operationCode string,
	rackID *uuid.UUID,
) (*operationrules.OperationRule, error) {
	// Check rack association first if rackID is provided
	if rackID != nil && *rackID != uuid.Nil {
		assoc, err := model.GetRackRuleAssociation(ctx, s.pg.DB, *rackID, opType, operationCode)
		if err != nil && !s.pg.GetErrorChecker().IsErrNoRows(err) {
			return nil, errors.GRPCErrorInternal(
				fmt.Sprintf("failed to query rack rule association: %v", err),
			)
		}

		// If association found, get the rule by ID
		if assoc != nil {
			dbModel, err := model.GetOperationRule(ctx, s.pg.DB, assoc.RuleID)
			if err != nil {
				if s.pg.GetErrorChecker().IsErrNoRows(err) {
					return nil, errors.GRPCErrorNotFound(
						fmt.Sprintf("associated rule %s not found", assoc.RuleID),
					)
				}
				return nil, errors.GRPCErrorInternal(
					fmt.Sprintf("failed to get associated rule: %v", err),
				)
			}
			return dao.OperationRuleFrom(dbModel)
		}
	}

	// Fall back to default rule
	return s.GetDefaultRule(ctx, opType, operationCode)
}

// ListRules lists operation rules matching the given criteria with pagination
func (s *PostgresStore) ListRules(
	ctx context.Context,
	options *taskcommon.OperationRuleListOptions,
	pagination *dbquery.Pagination,
) ([]*operationrules.OperationRule, int32, error) {
	dbModels, total, err := model.ListOperationRules(ctx, s.pg.DB, options, pagination)
	if err != nil {
		return nil, 0, errors.GRPCErrorInternal(
			fmt.Sprintf("failed to list operation rules: %v", err),
		)
	}

	rules := make([]*operationrules.OperationRule, 0, len(dbModels))
	for i := range dbModels {
		rule, err := dao.OperationRuleFrom(&dbModels[i])
		if err != nil {
			return nil, 0, err
		}
		rules = append(rules, rule)
	}

	return rules, total, nil
}

// GetRuleByName retrieves an operation rule by its name
func (s *PostgresStore) GetRuleByName(
	ctx context.Context,
	name string,
) (*operationrules.OperationRule, error) {
	var dbModel model.OperationRule
	err := s.pg.DB.NewSelect().
		Model(&dbModel).
		Where("name = ?", name).
		Limit(1).
		Scan(ctx)

	if err != nil {
		if s.pg.GetErrorChecker().IsErrNoRows(err) {
			return nil, errors.GRPCErrorNotFound(fmt.Sprintf("rule with name '%s' not found", name))
		}
		return nil, errors.GRPCErrorInternal(fmt.Sprintf("failed to get rule by name: %v", err))
	}

	return dao.OperationRuleFrom(&dbModel)
}

// GetDefaultRule retrieves the default rule for an operation type and code
func (s *PostgresStore) GetDefaultRule(
	ctx context.Context,
	opType taskcommon.TaskType,
	operationCode string,
) (*operationrules.OperationRule, error) {
	var dbModel model.OperationRule
	err := s.pg.DB.NewSelect().
		Model(&dbModel).
		Where("operation_type = ?", string(opType)).
		Where("operation_code = ?", operationCode).
		Where("is_default = ?", true).
		Limit(1).
		Scan(ctx)

	if err != nil {
		if s.pg.GetErrorChecker().IsErrNoRows(err) {
			return nil, nil // No default rule exists
		}
		return nil, errors.GRPCErrorInternal(
			fmt.Sprintf("failed to get default rule: %v", err),
		)
	}

	return dao.OperationRuleFrom(&dbModel)
}

// ========================================
// Rack Rule Association Methods
// ========================================

// AssociateRuleWithRack associates a rule with a rack.
// The operation type and operation code are extracted from the rule.
func (s *PostgresStore) AssociateRuleWithRack(
	ctx context.Context,
	rackID uuid.UUID,
	ruleID uuid.UUID,
) error {
	// Get the rule to extract operation_type and operation_code (also verifies it exists)
	dbModel, err := model.GetOperationRule(ctx, s.pg.DB, ruleID)
	if err != nil {
		if s.pg.GetErrorChecker().IsErrNoRows(err) {
			return errors.GRPCErrorNotFound(fmt.Sprintf("rule %s not found", ruleID))
		}
		return errors.GRPCErrorInternal(fmt.Sprintf("failed to get rule: %v", err))
	}

	operationCode := dbModel.OperationCode
	opType := taskcommon.TaskTypeFromString(dbModel.OperationType)

	// Check if association already exists
	existing, err := model.GetRackRuleAssociation(ctx, s.pg.DB, rackID, opType, operationCode)
	if err != nil && !s.pg.GetErrorChecker().IsErrNoRows(err) {
		return errors.GRPCErrorInternal(fmt.Sprintf("failed to check existing association: %v", err))
	}

	if existing != nil {
		// Update existing association
		_, err := s.pg.DB.NewUpdate().
			Model((*model.RackRuleAssociation)(nil)).
			Set("rule_id = ?", ruleID).
			Set("updated_at = NOW()").
			Where("rack_id = ?", rackID).
			Where("operation_type = ?", string(opType)).
			Where("operation_code = ?", operationCode).
			Exec(ctx)
		if err != nil {
			return errors.GRPCErrorInternal(fmt.Sprintf("failed to update association: %v", err))
		}
		return nil
	}

	// Create new association
	assoc := &model.RackRuleAssociation{
		RackID:        rackID,
		OperationType: string(opType),
		OperationCode: operationCode,
		RuleID:        ruleID,
	}

	if err := assoc.Create(ctx, s.pg.DB); err != nil {
		return errors.GRPCErrorInternal(fmt.Sprintf("failed to create association: %v", err))
	}

	return nil
}

// DisassociateRuleFromRack removes the rule association for a rack, operation type, and operation code
func (s *PostgresStore) DisassociateRuleFromRack(
	ctx context.Context,
	rackID uuid.UUID,
	opType taskcommon.TaskType,
	operationCode string,
) error {
	assoc := &model.RackRuleAssociation{
		RackID:        rackID,
		OperationType: string(opType),
		OperationCode: operationCode,
	}

	if err := assoc.Delete(ctx, s.pg.DB); err != nil {
		// Don't treat "not found" as an error
		if s.pg.GetErrorChecker().IsErrNoRows(err) {
			return nil
		}
		return errors.GRPCErrorInternal(fmt.Sprintf("failed to delete association: %v", err))
	}

	return nil
}

// GetRackRuleAssociation retrieves the rule ID associated with a rack for an operation type and operation code
func (s *PostgresStore) GetRackRuleAssociation(
	ctx context.Context,
	rackID uuid.UUID,
	opType taskcommon.TaskType,
	operationCode string,
) (*uuid.UUID, error) {
	assoc, err := model.GetRackRuleAssociation(ctx, s.pg.DB, rackID, opType, operationCode)
	if err != nil {
		if s.pg.GetErrorChecker().IsErrNoRows(err) {
			return nil, nil // No association exists
		}
		return nil, errors.GRPCErrorInternal(fmt.Sprintf("failed to get association: %v", err))
	}

	if assoc == nil {
		return nil, nil
	}

	return &assoc.RuleID, nil
}

// ListRackRuleAssociations retrieves all rule associations for a rack
func (s *PostgresStore) ListRackRuleAssociations(
	ctx context.Context,
	rackID uuid.UUID,
) ([]*operationrules.RackRuleAssociation, error) {
	assocs, err := model.ListRackRuleAssociations(ctx, s.pg.DB, rackID)
	if err != nil {
		return nil, errors.GRPCErrorInternal(
			fmt.Sprintf("failed to list rack associations: %v", err),
		)
	}

	result := make([]*operationrules.RackRuleAssociation, 0, len(assocs))
	for i := range assocs {
		result = append(result, dao.RackRuleAssociationFrom(&assocs[i]))
	}

	return result, nil
}
