// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package conflict

import (
	"context"

	"github.com/google/uuid"

	dbquery "github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/query"
	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operationrules"
	taskdef "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/task"
)

// mockStore is a minimal in-memory implementation of taskstore.Store used in
// conflict package tests. Only the methods exercised by this package are
// functional; the rest panic to catch unexpected calls.
type mockStore struct {
	activeTasks      map[uuid.UUID][]*taskdef.Task
	waitingTasks     map[uuid.UUID][]*taskdef.Task
	racksWithWaiting []uuid.UUID
	statusUpdates    []*taskdef.TaskStatusUpdate

	listActiveErr   error
	listWaitingErr  error
	listRacksErr    error
	updateStatusErr error
}

func newMockStore() *mockStore {
	return &mockStore{
		activeTasks:  make(map[uuid.UUID][]*taskdef.Task),
		waitingTasks: make(map[uuid.UUID][]*taskdef.Task),
	}
}

func (m *mockStore) RunInTransaction(ctx context.Context, fn func(ctx context.Context) error) error {
	return fn(ctx)
}

func (m *mockStore) ListActiveTasksForRack(ctx context.Context, rackID uuid.UUID) ([]*taskdef.Task, error) {
	if m.listActiveErr != nil {
		return nil, m.listActiveErr
	}
	return m.activeTasks[rackID], nil
}

func (m *mockStore) ListWaitingTasksForRack(ctx context.Context, rackID uuid.UUID) ([]*taskdef.Task, error) {
	if m.listWaitingErr != nil {
		return nil, m.listWaitingErr
	}
	return m.waitingTasks[rackID], nil
}

func (m *mockStore) CountWaitingTasksForRack(ctx context.Context, rackID uuid.UUID) (int, error) {
	return len(m.waitingTasks[rackID]), nil
}

func (m *mockStore) ListRacksWithWaitingTasks(ctx context.Context) ([]uuid.UUID, error) {
	if m.listRacksErr != nil {
		return nil, m.listRacksErr
	}
	return m.racksWithWaiting, nil
}

func (m *mockStore) UpdateTaskReport(_ context.Context, _ *taskdef.TaskReportUpdate) error {
	return nil
}

func (m *mockStore) UpdateTaskStatus(ctx context.Context, arg *taskdef.TaskStatusUpdate) error {
	if m.updateStatusErr != nil {
		return m.updateStatusErr
	}
	m.statusUpdates = append(m.statusUpdates, arg)
	return nil
}

// --- methods not used by the conflict package ---

func (m *mockStore) CreateTask(_ context.Context, _ *taskdef.Task) error {
	panic("mockStore.CreateTask: not implemented")
}

func (m *mockStore) LockRack(_ context.Context, _ uuid.UUID) error {
	panic("mockStore.LockRack: not implemented")
}

func (m *mockStore) LockIdempotencyKey(_ context.Context, _ string) error {
	panic("mockStore.LockIdempotencyKey: not implemented")
}

func (m *mockStore) GetTaskByIdempotencyKey(_ context.Context, _ string) (*taskdef.Task, error) {
	panic("mockStore.GetTaskByIdempotencyKey: not implemented")
}

func (m *mockStore) GetTask(_ context.Context, _ uuid.UUID) (*taskdef.Task, error) {
	panic("mockStore.GetTask: not implemented")
}

func (m *mockStore) GetTasks(_ context.Context, _ []uuid.UUID) ([]*taskdef.Task, error) {
	panic("mockStore.GetTasks: not implemented")
}

func (m *mockStore) ListTasks(_ context.Context, _ *taskcommon.TaskListOptions, _ *dbquery.Pagination) ([]*taskdef.Task, int32, error) {
	panic("mockStore.ListTasks: not implemented")
}

func (m *mockStore) ListNonTerminalTasksForRacks(_ context.Context, _ []uuid.UUID) ([]*taskdef.Task, error) {
	panic("mockStore.ListNonTerminalTasksForRacks: not implemented")
}

func (m *mockStore) UpdateScheduledTask(_ context.Context, _ *taskdef.Task) error {
	panic("mockStore.UpdateScheduledTask: not implemented")
}

func (m *mockStore) CreateRule(_ context.Context, _ *operationrules.OperationRule) error {
	panic("mockStore.CreateRule: not implemented")
}

func (m *mockStore) UpdateRule(_ context.Context, _ uuid.UUID, _ map[string]interface{}) error {
	panic("mockStore.UpdateRule: not implemented")
}

func (m *mockStore) DeleteRule(_ context.Context, _ uuid.UUID) error {
	panic("mockStore.DeleteRule: not implemented")
}

func (m *mockStore) SetRuleAsDefault(_ context.Context, _ uuid.UUID) error {
	panic("mockStore.SetRuleAsDefault: not implemented")
}

func (m *mockStore) GetRule(_ context.Context, _ uuid.UUID) (*operationrules.OperationRule, error) {
	panic("mockStore.GetRule: not implemented")
}

func (m *mockStore) GetRuleByName(_ context.Context, _ string) (*operationrules.OperationRule, error) {
	panic("mockStore.GetRuleByName: not implemented")
}

func (m *mockStore) GetDefaultRule(_ context.Context, _ taskcommon.TaskType, _ string) (*operationrules.OperationRule, error) {
	panic("mockStore.GetDefaultRule: not implemented")
}

func (m *mockStore) GetRuleByOperationAndRack(_ context.Context, _ taskcommon.TaskType, _ string, _ *uuid.UUID) (*operationrules.OperationRule, error) {
	panic("mockStore.GetRuleByOperationAndRack: not implemented")
}

func (m *mockStore) ListRules(_ context.Context, _ *taskcommon.OperationRuleListOptions, _ *dbquery.Pagination) ([]*operationrules.OperationRule, int32, error) {
	panic("mockStore.ListRules: not implemented")
}

func (m *mockStore) AssociateRuleWithRack(_ context.Context, _ uuid.UUID, _ uuid.UUID) error {
	panic("mockStore.AssociateRuleWithRack: not implemented")
}

func (m *mockStore) DisassociateRuleFromRack(_ context.Context, _ uuid.UUID, _ taskcommon.TaskType, _ string) error {
	panic("mockStore.DisassociateRuleFromRack: not implemented")
}

func (m *mockStore) GetRackRuleAssociation(_ context.Context, _ uuid.UUID, _ taskcommon.TaskType, _ string) (*uuid.UUID, error) {
	panic("mockStore.GetRackRuleAssociation: not implemented")
}

func (m *mockStore) ListRackRuleAssociations(_ context.Context, _ uuid.UUID) ([]*operationrules.RackRuleAssociation, error) {
	panic("mockStore.ListRackRuleAssociations: not implemented")
}
