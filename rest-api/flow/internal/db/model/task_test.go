// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"strings"
	"testing"
	"time"

	"github.com/DATA-DOG/go-sqlmock"
	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"github.com/uptrace/bun"
	"github.com/uptrace/bun/dialect/pgdialect"

	dbquery "github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/query"
	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
)

// newOfflineBun returns a bun DB wired to the postgres dialect with no
// underlying connection. It is sufficient for generating SQL strings from
// query builders, which is what these tests exercise.
func newOfflineBun() *bun.DB {
	return bun.NewDB(nil, pgdialect.New())
}

func TestTaskListOptionsToFilterable_Nil(t *testing.T) {
	assert.Nil(t, taskListOptionsToFilterable(nil))
}

func TestListTasks_DefaultOrderBeforePagination(t *testing.T) {
	sqlDB, mock, err := sqlmock.New()
	require.NoError(t, err)
	defer sqlDB.Close()

	db := bun.NewDB(sqlDB, pgdialect.New())
	defer db.Close()

	mock.ExpectQuery(`ORDER BY "created_at" DESC, "id" DESC LIMIT 20`).
		WillReturnRows(sqlmock.NewRows([]string{"id"}))

	tasks, total, err := ListTasks(
		t.Context(),
		db,
		&taskcommon.TaskListOptions{TaskType: taskcommon.TaskTypeUnknown},
		&dbquery.Pagination{Offset: 0, Limit: 20, Total: 1},
	)

	require.NoError(t, err)
	assert.Empty(t, tasks)
	assert.Equal(t, int32(1), total)
	require.NoError(t, mock.ExpectationsWereMet())
}

func TestTask_UpdateTaskStatus(t *testing.T) {
	t.Run("persists a queue deadline", func(t *testing.T) {
		sqlDB, mock, err := sqlmock.New()
		require.NoError(t, err)
		defer sqlDB.Close()

		db := bun.NewDB(sqlDB, pgdialect.New())
		defer db.Close()

		deadline := time.Date(2030, time.January, 2, 3, 4, 5, 0, time.UTC)
		mock.ExpectExec(
			`UPDATE "task" AS "t" SET .*"queue_expires_at" = '2030-01-02 03:04:05\+00:00'.* WHERE \(id =`,
		).WillReturnResult(sqlmock.NewResult(0, 1))
		task := &Task{ID: uuid.New()}

		err = task.UpdateTaskStatus(
			t.Context(),
			db,
			taskcommon.TaskStatusWaiting,
			"Waiting for target linkage",
			nil,
			&deadline,
		)

		require.NoError(t, err)
		require.Equal(t, taskcommon.TaskStatusWaiting, task.Status)
		require.Equal(t, deadline, *task.QueueExpiresAt)
		require.NoError(t, mock.ExpectationsWereMet())
	})

	t.Run("clears the queue deadline when finished", func(t *testing.T) {
		sqlDB, mock, err := sqlmock.New()
		require.NoError(t, err)
		defer sqlDB.Close()

		db := bun.NewDB(sqlDB, pgdialect.New())
		defer db.Close()

		mock.ExpectExec(`UPDATE "task" AS "t" SET .*"queue_expires_at" = NULL.* WHERE \(id =`).
			WillReturnResult(sqlmock.NewResult(0, 1))
		deadline := time.Now().Add(time.Hour).UTC()
		task := &Task{ID: uuid.New(), QueueExpiresAt: &deadline}

		err = task.UpdateTaskStatus(
			t.Context(),
			db,
			taskcommon.TaskStatusTerminated,
			"Expired",
			nil,
			nil,
		)

		require.NoError(t, err)
		require.Nil(t, task.QueueExpiresAt)
		require.NotNil(t, task.FinishedAt)
		require.NoError(t, mock.ExpectationsWereMet())
	})
}

// All tests below set TaskType: TaskTypeUnknown explicitly, matching the
// production caller in server_impl.ListTasks. Leaving TaskType as its zero
// value triggers a pre-existing branch that appends an empty `type = ''`
// filter; that behaviour is not in this PR's scope.

func TestTaskListOptionsToFilterable_Empty(t *testing.T) {
	got := taskListOptionsToFilterable(&taskcommon.TaskListOptions{
		TaskType: taskcommon.TaskTypeUnknown,
	})
	group, ok := got.(*dbquery.FilterGroup)
	require.True(t, ok, "expected *FilterGroup, got %T", got)
	assert.Empty(t, group.Filters)
}

func TestTaskListOptionsToFilterable_RackOnly(t *testing.T) {
	rackID := uuid.New()

	got := taskListOptionsToFilterable(&taskcommon.TaskListOptions{
		TaskType: taskcommon.TaskTypeUnknown,
		RackID:   rackID,
	})

	group, ok := got.(*dbquery.FilterGroup)
	require.True(t, ok, "expected *FilterGroup, got %T", got)
	require.Len(t, group.Filters, 1)
	assert.Equal(t, "rack_id", group.Filters[0].Column)
	assert.Equal(t, dbquery.OperatorEqual, group.Filters[0].Operator)
	assert.Equal(t, rackID, group.Filters[0].Value)
}

func TestTaskListOptionsToFilterable_ComponentOnly(t *testing.T) {
	compID := uuid.New()

	got := taskListOptionsToFilterable(&taskcommon.TaskListOptions{
		TaskType:    taskcommon.TaskTypeUnknown,
		ComponentID: compID,
	})

	fs, ok := got.(taskFilterables)
	require.True(t, ok, "expected taskFilterables, got %T", got)
	require.Len(t, fs, 2)

	group, ok := fs[0].(*dbquery.FilterGroup)
	require.True(t, ok)
	assert.Empty(t, group.Filters, "no other filters expected")

	cFilter, ok := fs[1].(taskComponentIDFilter)
	require.True(t, ok)
	assert.Equal(t, compID, cFilter.componentID)
}

func TestTaskListOptionsToFilterable_RackAndComponent(t *testing.T) {
	rackID := uuid.New()
	compID := uuid.New()

	got := taskListOptionsToFilterable(&taskcommon.TaskListOptions{
		TaskType:    taskcommon.TaskTypeUnknown,
		RackID:      rackID,
		ComponentID: compID,
		ActiveOnly:  true,
	})

	fs, ok := got.(taskFilterables)
	require.True(t, ok, "expected taskFilterables, got %T", got)
	require.Len(t, fs, 2)

	group, ok := fs[0].(*dbquery.FilterGroup)
	require.True(t, ok)
	require.Len(t, group.Filters, 2, "expected rack_id and status filters")
	assert.Equal(t, "rack_id", group.Filters[0].Column)
	assert.Equal(t, rackID, group.Filters[0].Value)
	assert.Equal(t, "status", group.Filters[1].Column)
	assert.Equal(t, dbquery.OperatorIn, group.Filters[1].Operator)

	cFilter, ok := fs[1].(taskComponentIDFilter)
	require.True(t, ok)
	assert.Equal(t, compID, cFilter.componentID)
}

// TestTaskComponentIDFilter_GeneratedSQL pins the jsonpath predicate emitted
// by taskComponentIDFilter. The exact form matters for two reasons: callers
// rely on the @? operator (not @>) for the contained-value semantics across
// all component types, and the GIN(attributes jsonb_path_ops) index in
// migration 20260517000000 is only used when the operator is @?.
func TestTaskComponentIDFilter_GeneratedSQL(t *testing.T) {
	compID := uuid.MustParse("11111111-2222-3333-4444-555555555555")
	db := newOfflineBun()

	q := db.NewSelect().TableExpr("task")
	q = taskComponentIDFilter{componentID: compID}.ApplyTo(q)

	sql, err := q.AppendQuery(db.Formatter(), nil)
	require.NoError(t, err)
	got := string(sql)

	assert.Contains(t, got, "attributes @?")
	assert.Contains(t, got, "::jsonpath")
	assert.Contains(t, got, "$.components_by_type.*[*]")
	assert.Contains(t, got, compID.String(),
		"the UUID must appear literally inside the jsonpath string so the planner can use the GIN index")
}

// TestTaskListOptionsToFilterable_FullQuerySQL exercises the composite
// Filterable end-to-end: rack_id + active_only + component_id must all
// AND together in the final query.
func TestTaskListOptionsToFilterable_FullQuerySQL(t *testing.T) {
	rackID := uuid.MustParse("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee")
	compID := uuid.MustParse("11111111-2222-3333-4444-555555555555")
	db := newOfflineBun()

	got := taskListOptionsToFilterable(&taskcommon.TaskListOptions{
		TaskType:    taskcommon.TaskTypeUnknown,
		RackID:      rackID,
		ComponentID: compID,
		ActiveOnly:  true,
	})
	require.NotNil(t, got)

	q := db.NewSelect().TableExpr("task")
	q = got.ApplyTo(q)

	sql, err := q.AppendQuery(db.Formatter(), nil)
	require.NoError(t, err)
	gotSQL := string(sql)

	// Each predicate must be present; combined under AND by bun's Where
	// chaining (no OR or aliasing surprises).
	for _, frag := range []string{
		"rack_id = ",
		rackID.String(),
		"status IN",
		"attributes @?",
		compID.String(),
	} {
		assert.Contains(t, gotSQL, frag, "missing fragment %q", frag)
	}
	assert.Equal(t, 0, strings.Count(gotSQL, " OR "),
		"filters must be AND-combined, got: %s", gotSQL)
}

func TestListTasksForRacksByStatus_GeneratedSQL(t *testing.T) {
	rackID := uuid.MustParse("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee")
	db := newOfflineBun()
	var tasks []Task

	q := listTasksForRacksByStatusQuery(
		db,
		&tasks,
		[]uuid.UUID{rackID},
		taskcommon.NonTerminalTaskStatuses(),
	)
	sql, err := q.AppendQuery(db.Formatter(), nil)
	require.NoError(t, err)
	got := string(sql)

	for _, fragment := range []string{
		`"t"."id"`,
		`"t"."rack_id"`,
		`"t"."attributes"`,
		`"t"."status"`,
		`rack_id IN`,
		rackID.String(),
		`status IN`,
	} {
		assert.Contains(t, got, fragment)
	}
	assert.NotContains(t, got, `"t"."report"`)
}

func TestListTasksForRacksByStatus_EmptyFilter(t *testing.T) {
	tasks, err := ListTasksForRacksByStatus(
		context.Background(),
		nil,
		nil,
		taskcommon.NonTerminalTaskStatuses(),
	)
	require.NoError(t, err)
	assert.Empty(t, tasks)
}
