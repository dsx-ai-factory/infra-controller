// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package db

import (
	"context"
	"database/sql"
	"errors"
	"os"
	"strconv"
	"strings"
	"testing"
	"time"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"

	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"github.com/uptrace/bun"
	"github.com/uptrace/bun/extra/bundebug"
)

func testTxGetTestSession(t *testing.T) *Session {
	host := "localhost"
	port := 30432
	if configured := os.Getenv("PGPORT"); configured != "" {
		var err error
		port, err = strconv.Atoi(configured)
		require.NoError(t, err)
	}
	if os.Getenv("CI") == "true" {
		host = "postgres"
		port = 5432
	}
	dbSession, err := NewSession(context.Background(), host, port, "postgres", "postgres", "postgres", "")
	assert.Nil(t, err)
	dbSession.DB.AddQueryHook(bundebug.NewQueryHook(
		bundebug.WithEnabled(false),
		bundebug.FromEnv(""),
	))
	return dbSession
}

type TestTable struct {
	bun.BaseModel `bun:"table:test_table,alias:tt"`

	ID   uuid.UUID `bun:"type:uuid,pk"`
	Name string    `bun:"name,notnull"`
}

func testTxSetupSchema(t *testing.T, dbSession *Session) {
	ctx := context.Background()
	tx, err := BeginTx(context.Background(), dbSession, &sql.TxOptions{Isolation: sql.LevelSerializable})
	assert.Nil(t, err)
	GetIDB(tx, dbSession).NewDropTable().Model(&TestTable{}).IfExists().Exec(ctx)
	GetIDB(tx, dbSession).NewCreateTable().Model(&TestTable{}).IfNotExists().Exec(ctx)
	err = tx.Commit()
	assert.Nil(t, err)
}

func TestTxBase(t *testing.T) {
	dbSession := testTxGetTestSession(t)
	defer dbSession.Close()
	testTxSetupSchema(t, dbSession)
	badSession, err := NewSession(context.Background(), "localhost", 1234, "postgres", "postgres", "postgres", "")
	assert.Nil(t, err)
	ctx := context.Background()
	txOpts := sql.TxOptions{Isolation: sql.LevelRepeatableRead}
	tests := []struct {
		name      string
		dbSession *Session
		expectErr bool
		commit    bool
		rollback  bool
	}{
		{
			name:      "error when bun's begintx fails",
			dbSession: badSession,
			expectErr: true,
		},
		{
			name:      "success case with rollback",
			dbSession: dbSession,
			expectErr: false,
			rollback:  true,
		},
		{
			name:      "success case with commit",
			dbSession: dbSession,
			expectErr: false,
			commit:    true,
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			tx, err := BeginTx(ctx, tc.dbSession, &txOpts)
			assert.Equal(t, tc.expectErr, err != nil)
			if !tc.expectErr {
				_, err := GetIDB(tx, tc.dbSession).NewInsert().Model(&TestTable{ID: uuid.New(), Name: "test"}).Exec(ctx)
				assert.Nil(t, err)
				if tc.commit {
					assert.Nil(t, tx.Commit())
					tt := &TestTable{}
					err := GetIDB(nil, tc.dbSession).NewSelect().Model(tt).Where("name = ?", "test").Scan(ctx)
					assert.Nil(t, err)
				}
				if tc.rollback {
					assert.Nil(t, tx.Rollback())
					tt := &TestTable{}
					err := GetIDB(nil, tc.dbSession).NewSelect().Model(tt).Where("name = ?", "test").Scan(ctx)
					assert.NotNil(t, err)
				}
			}
		})
	}
}

func TestTxGetIDB(t *testing.T) {
	dbSession := testTxGetTestSession(t)
	defer dbSession.Close()
	testTxSetupSchema(t, dbSession)
	tx1, err := BeginTx(context.Background(), dbSession, &sql.TxOptions{})
	assert.Nil(t, err)
	tx2, err := BeginTx(context.Background(), dbSession, &sql.TxOptions{})
	assert.Nil(t, err)
	ctx := context.Background()
	tests := []struct {
		name      string
		tx        *Tx
		dbSession *Session
		rname     string
		commit    bool
		rollback  bool
	}{
		{
			name:      "must return transaction if exists, can commit",
			tx:        tx1,
			dbSession: dbSession,
			rname:     "test1",
			commit:    true,
		},
		{
			name:      "must return transaction if exists, can rollback",
			tx:        tx2,
			dbSession: dbSession,
			rname:     "test2",
			commit:    false,
			rollback:  true,
		},
		{
			name:      "must return dbSession if tx is nil",
			tx:        nil,
			dbSession: dbSession,
			rname:     "test3",
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			idb := GetIDB(tc.tx, tc.dbSession)
			_, err := idb.NewInsert().Model(&TestTable{ID: uuid.New(), Name: tc.rname}).Exec(ctx)
			assert.Nil(t, err)
			if tc.tx != nil {
				if tc.commit {
					assert.Nil(t, tc.tx.Commit())
					tt := &TestTable{}
					err := GetIDB(nil, tc.dbSession).NewSelect().Model(tt).Where("name = ?", tc.rname).Scan(ctx)
					assert.Nil(t, err)
				}
				if tc.rollback {
					assert.Nil(t, tc.tx.Rollback())
					tt := &TestTable{}
					err := GetIDB(nil, tc.dbSession).NewSelect().Model(tt).Where("name = ?", tc.rname).Scan(ctx)
					assert.NotNil(t, err)
				}
			} else {
				tt := &TestTable{}
				err := GetIDB(nil, tc.dbSession).NewSelect().Model(tt).Where("name = ?", tc.rname).Scan(ctx)
				assert.Nil(t, err)
			}
		})
	}
}

func TestTxAcquireAdvisoryLock(t *testing.T) {
	dbSession := testTxGetTestSession(t)
	defer dbSession.Close()
	ctx := context.Background()
	tests := []struct {
		name      string
		expectErr bool
		testcase  int
	}{
		{
			name:     "success acquire lock",
			testcase: 1,
		},
		{
			name:     "failure to acquire lock because it is held by another tx",
			testcase: 2,
		},
		{
			name:     "success acquire another lock by different tx",
			testcase: 3,
		},
		{
			name:     "reacquire lock success after commit existing tx with lock",
			testcase: 4,
		},
		{
			name:     "reacquire lock success after rollback existing tx with lock",
			testcase: 5,
		},
	}
	var tx1, tx2, tx3 *Tx
	var err error
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			switch tc.testcase {
			case 1:
				// success acquire lock
				tx1, err = BeginTx(ctx, dbSession, &sql.TxOptions{})
				assert.Nil(t, err)
				assert.NotNil(t, tx1)
				err = tx1.AcquireAdvisoryLock(ctx, uint64(123), false)
				assert.Nil(t, err)
				// can reacquire same lock in same transaction
				err = tx1.AcquireAdvisoryLock(ctx, uint64(123), false)
				assert.Nil(t, err)
				// can reacquire same lock yet again in same transaction,
				// this time with blocking.
				err = tx1.AcquireAdvisoryLock(ctx, uint64(123), true)
				assert.Nil(t, err)
			case 2:
				// failure to acquire lock because it is held by another tx
				tx2, err = BeginTx(ctx, dbSession, &sql.TxOptions{})
				assert.Nil(t, err)
				assert.NotNil(t, tx2)
				err = tx2.AcquireAdvisoryLock(ctx, uint64(123), false)
				assert.NotNil(t, err)
			case 3:
				// success acquire another lock
				err = tx2.AcquireAdvisoryLock(ctx, uint64(456), false)
				assert.Nil(t, err)
			case 4:
				// reacquire lock success after commit existing tx with lock
				err = tx1.Commit()
				assert.Nil(t, err)
				tx3, err = BeginTx(ctx, dbSession, &sql.TxOptions{})
				assert.Nil(t, err)
				assert.NotNil(t, tx3)
				err = tx3.AcquireAdvisoryLock(ctx, uint64(123), false)
				assert.Nil(t, err)
				tx3.Rollback()
			case 5:
				// reacquire lock success after rollback existing tx with lock
				err = tx2.Rollback()
				assert.Nil(t, err)
				tx3, err = BeginTx(ctx, dbSession, &sql.TxOptions{})
				assert.Nil(t, err)
				assert.NotNil(t, tx3)
				err = tx3.AcquireAdvisoryLock(ctx, uint64(456), false)
				assert.Nil(t, err)
				tx3.Rollback()
			}
		})
	}
}

func TestGetAdvisoryLockIDFromString(t *testing.T) {
	dbSession := testTxGetTestSession(t)
	defer dbSession.Close()
	ctx := context.Background()
	tests := []struct {
		name      string
		lockID    uint64
		expectErr bool
	}{
		{
			name:      "lockId is 0",
			lockID:    uint64(0),
			expectErr: false,
		},
		{
			name:      "lockId is 0x7fffffffffffffff",
			lockID:    uint64(0x7fffffffffffffff),
			expectErr: false,
		},
		{
			name:      "acquire lock fails when most significant bit is set",
			lockID:    uint64(0xafffffffffffffff),
			expectErr: true,
		},
		{
			name:      "acquire lock succeeds for random uuid strings",
			lockID:    GetAdvisoryLockIDFromString(uuid.New().String()),
			expectErr: false,
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			tx, err := BeginTx(ctx, dbSession, nil)
			assert.Nil(t, err)
			err = tx.AcquireAdvisoryLock(ctx, tc.lockID, false)
			assert.Equal(t, tc.expectErr, err != nil)
			err = tx.Rollback()
			assert.Nil(t, err)
		})
	}
}

func TestTxTryAcquireAdvisoryLock(t *testing.T) {
	dbSession := testTxGetTestSession(t)
	defer dbSession.Close()
	ctx := context.Background()
	tests := []struct {
		name      string
		testcase  int
		expectErr bool
	}{
		{
			name:      "success acquire lock when it is not already acquired",
			testcase:  1,
			expectErr: false,
		},
		{
			name:      "failure to acquire lock with retries because it is held by another tx",
			testcase:  2,
			expectErr: true,
		},
		{
			name:      "success acquire lock after retries when held by different tx and then released",
			testcase:  3,
			expectErr: false,
		},
	}
	lockID := GetAdvisoryLockIDFromString("test-lock")
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			switch tc.testcase {
			case 1:
				tx1, err := BeginTx(ctx, dbSession, &sql.TxOptions{})
				assert.Nil(t, err)
				err = tx1.TryAcquireAdvisoryLock(ctx, lockID, nil)
				assert.Nil(t, err)
				tx1.Rollback()
			case 2:
				tx1, err := BeginTx(ctx, dbSession, &sql.TxOptions{})
				assert.Nil(t, err)
				err = tx1.AcquireAdvisoryLock(ctx, lockID, false)
				assert.Nil(t, err)
				tx2, err := BeginTx(ctx, dbSession, &sql.TxOptions{})
				assert.Nil(t, err)
				err = tx2.TryAcquireAdvisoryLock(ctx, lockID, nil)
				assert.NotNil(t, err)
				tx1.Rollback()
				tx2.Rollback()
			case 3:
				tx1, err := BeginTx(ctx, dbSession, &sql.TxOptions{})
				assert.Nil(t, err)
				err = tx1.AcquireAdvisoryLock(ctx, lockID, false)
				assert.Nil(t, err)
				tx2, err := BeginTx(ctx, dbSession, &sql.TxOptions{})
				assert.Nil(t, err)
				go func() {
					time.Sleep(150 * time.Millisecond)
					tx1.Rollback()
				}()
				err = tx2.TryAcquireAdvisoryLock(ctx, lockID, &LockRetryOptions{Retries: cutil.GetPtr(5)})
				assert.Nil(t, err)
				tx2.Rollback()
			}
		})
	}
}

func TestWithTx_CommitsOnSuccess(t *testing.T) {
	dbSession := testTxGetTestSession(t)
	defer dbSession.Close()
	testTxSetupSchema(t, dbSession)
	ctx := context.Background()

	name := "withtx-commit"
	err := WithTx(ctx, dbSession, func(tx *Tx) error {
		_, err := GetIDB(tx, dbSession).NewInsert().Model(&TestTable{ID: uuid.New(), Name: name}).Exec(ctx)
		return err
	})
	assert.Nil(t, err)

	// Row should be visible: closure returned nil, so commit happened.
	tt := &TestTable{}
	err = GetIDB(nil, dbSession).NewSelect().Model(tt).Where("name = ?", name).Scan(ctx)
	assert.Nil(t, err)
	assert.Equal(t, name, tt.Name)
}

func TestWithTx_RollsBackOnError(t *testing.T) {
	dbSession := testTxGetTestSession(t)
	defer dbSession.Close()
	testTxSetupSchema(t, dbSession)
	ctx := context.Background()

	name := "withtx-rollback"
	sentinel := errors.New("simulated failure")
	err := WithTx(ctx, dbSession, func(tx *Tx) error {
		_, ierr := GetIDB(tx, dbSession).NewInsert().Model(&TestTable{ID: uuid.New(), Name: name}).Exec(ctx)
		assert.Nil(t, ierr)
		// Closure returns error -> tx must roll back.
		return sentinel
	})
	assert.ErrorIs(t, err, sentinel)

	// Row should NOT be visible: closure returned err, so rollback happened.
	tt := &TestTable{}
	err = GetIDB(nil, dbSession).NewSelect().Model(tt).Where("name = ?", name).Scan(ctx)
	assert.NotNil(t, err)
}

func TestWithTx_PropagatesBeginTxError(t *testing.T) {
	// Fail fast if session creation itself errors -- otherwise we'd proceed
	// with a nil/bad session and the actual assertion below would be
	// dominated by an unrelated nil-pointer panic.
	badSession, err := NewSession(context.Background(), "localhost", 1234, "postgres", "postgres", "postgres", "")
	require.NoError(t, err)
	defer badSession.Close()
	ctx := context.Background()

	called := false
	err = WithTx(ctx, badSession, func(tx *Tx) error {
		called = true
		return nil
	})
	assert.NotNil(t, err)
	assert.True(t, errors.Is(err, ErrTransactionInitiation), "BeginTx failures should be tagged with ErrTransactionInitiation")
	assert.False(t, called, "closure should not be invoked when BeginTx fails")
}

func TestWithTxResult_CommitsAndReturnsValue(t *testing.T) {
	dbSession := testTxGetTestSession(t)
	defer dbSession.Close()
	testTxSetupSchema(t, dbSession)
	ctx := context.Background()

	name := "withtxresult-commit"
	id, err := WithTxResult(ctx, dbSession, func(tx *Tx) (uuid.UUID, error) {
		newID := uuid.New()
		_, ierr := GetIDB(tx, dbSession).NewInsert().Model(&TestTable{ID: newID, Name: name}).Exec(ctx)
		return newID, ierr
	})
	assert.Nil(t, err)
	assert.NotEqual(t, uuid.Nil, id)

	tt := &TestTable{}
	err = GetIDB(nil, dbSession).NewSelect().Model(tt).Where("id = ?", id).Scan(ctx)
	assert.Nil(t, err)
	assert.Equal(t, name, tt.Name)
}

func TestWithTxResult_TagsCommitFailure(t *testing.T) {
	dbSession := testTxGetTestSession(t)
	defer dbSession.Close()
	ctx := context.Background()

	result, err := WithTxResult(ctx, dbSession, func(tx *Tx) (string, error) {
		_, execErr := GetIDB(tx, dbSession).ExecContext(ctx, "CREATE TEMP TABLE expected_inventory_commit_failure (id integer PRIMARY KEY, parent_id integer REFERENCES expected_inventory_commit_failure(id) DEFERRABLE INITIALLY DEFERRED) ON COMMIT DROP")
		require.NoError(t, execErr)
		_, execErr = GetIDB(tx, dbSession).ExecContext(ctx, "INSERT INTO expected_inventory_commit_failure VALUES (1, 2)")
		require.NoError(t, execErr)
		return "Core succeeded", nil
	})
	require.ErrorIs(t, err, ErrTransactionCommit)
	assert.Empty(t, result)
}

func TestWithTx_RollsBackOnPanicAndRepanics(t *testing.T) {
	dbSession := testTxGetTestSession(t)
	defer dbSession.Close()
	testTxSetupSchema(t, dbSession)
	ctx := context.Background()

	name := "withtx-panic"
	defer func() {
		// The original panic value must propagate to the caller.
		p := recover()
		assert.Equal(t, "boom", p)

		// Row should NOT be visible: closure panicked, so rollback happened.
		tt := &TestTable{}
		serr := GetIDB(nil, dbSession).NewSelect().Model(tt).Where("name = ?", name).Scan(ctx)
		assert.NotNil(t, serr)
	}()

	_ = WithTx(ctx, dbSession, func(tx *Tx) error {
		_, ierr := GetIDB(tx, dbSession).NewInsert().Model(&TestTable{ID: uuid.New(), Name: name}).Exec(ctx)
		assert.Nil(t, ierr)
		panic("boom")
	})

	t.Fatal("panic should have propagated past WithTx")
}

func TestWithTxResult_RollsBackOnPanicAndRepanics(t *testing.T) {
	dbSession := testTxGetTestSession(t)
	defer dbSession.Close()
	testTxSetupSchema(t, dbSession)
	ctx := context.Background()

	name := "withtxresult-panic"
	defer func() {
		p := recover()
		assert.Equal(t, "boom", p)

		tt := &TestTable{}
		serr := GetIDB(nil, dbSession).NewSelect().Model(tt).Where("name = ?", name).Scan(ctx)
		assert.NotNil(t, serr)
	}()

	_, _ = WithTxResult(ctx, dbSession, func(tx *Tx) (uuid.UUID, error) {
		_, ierr := GetIDB(tx, dbSession).NewInsert().Model(&TestTable{ID: uuid.New(), Name: name}).Exec(ctx)
		assert.Nil(t, ierr)
		panic("boom")
	})

	t.Fatal("panic should have propagated past WithTxResult")
}

func TestWithTxResult_ReturnsZeroValueOnError(t *testing.T) {
	dbSession := testTxGetTestSession(t)
	defer dbSession.Close()
	testTxSetupSchema(t, dbSession)
	ctx := context.Background()

	sentinel := errors.New("simulated failure")
	id, err := WithTxResult(ctx, dbSession, func(tx *Tx) (uuid.UUID, error) {
		return uuid.New(), sentinel
	})
	assert.ErrorIs(t, err, sentinel)
	// Caller gets the zero value when fn returns an error.
	assert.Equal(t, uuid.Nil, id)
}

// testTxFailReleaseHook cancels RELEASE SAVEPOINT statements while armed, so a
// rollback to the savepoint can succeed and its release fail.
type testTxFailReleaseHook struct {
	armed bool
}

func (h *testTxFailReleaseHook) BeforeQuery(ctx context.Context, event *bun.QueryEvent) context.Context {
	if !h.armed || !strings.HasPrefix(event.Query, "RELEASE SAVEPOINT") {
		return ctx
	}
	canceled, cancel := context.WithCancel(ctx)
	cancel()
	return canceled
}

func (h *testTxFailReleaseHook) AfterQuery(context.Context, *bun.QueryEvent) {}

func TestTx_WithSavepoint(t *testing.T) {
	dbSession := testTxGetTestSession(t)
	defer dbSession.Close()
	testTxSetupSchema(t, dbSession)
	ctx := context.Background()
	errRejected := errors.New("candidate rejected")
	releaseHook := &testTxFailReleaseHook{}
	dbSession.DB.AddQueryHook(releaseHook)

	tests := []struct {
		name string
		// calls is how many times WithSavepoint runs in the same transaction, each fn taking the same write and lock
		calls int
		// endTx rolls back the outer transaction first, so the SAVEPOINT statement fails
		endTx bool
		// failStatement runs a failing statement in fn, which aborts the outer transaction unless it rolls back to the savepoint
		failStatement bool
		// cancelCtx cancels the context passed to WithSavepoint inside fn, so ROLLBACK TO SAVEPOINT fails
		cancelCtx bool
		// failRelease makes RELEASE SAVEPOINT fail after ROLLBACK TO SAVEPOINT succeeds
		failRelease      bool
		fnErr            error
		wantFnCalled     bool
		wantSavepointErr bool
		// wantKept reports whether the write and advisory lock taken in fn outlive WithSavepoint
		wantKept bool
	}{
		{
			name:          "repeated fn errors roll back their writes and locks, leave no savepoint open",
			calls:         3,
			failStatement: true,
			fnErr:         errRejected,
			wantFnCalled:  true,
		},
		{
			name:         "fn success keeps its write and lock",
			wantFnCalled: true,
			wantKept:     true,
		},
		{
			name:             "savepoint creation failure is tagged, fn is skipped",
			endTx:            true,
			wantSavepointErr: true,
		},
		{
			name:             "rollback failure is tagged alongside fn error",
			cancelCtx:        true,
			fnErr:            errRejected,
			wantFnCalled:     true,
			wantSavepointErr: true,
		},
		{
			name:             "release failure after rollback is tagged alongside fn error",
			failRelease:      true,
			fnErr:            errRejected,
			wantFnCalled:     true,
			wantSavepointErr: true,
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			lockID := GetAdvisoryLockIDFromString(tc.name)
			tx, err := BeginTx(ctx, dbSession, nil)
			require.NoError(t, err)
			defer func() { _ = tx.Rollback() }()
			if tc.endTx {
				require.NoError(t, tx.Rollback())
			}
			releaseHook.armed = tc.failRelease
			defer func() { releaseHook.armed = false }()

			spCtx, cancel := context.WithCancel(ctx)
			defer cancel()
			fnCalled := false
			for range max(tc.calls, 1) {
				err = tx.WithSavepoint(spCtx, func(sp *Tx) error {
					fnCalled = true
					_, ierr := GetIDB(sp, dbSession).NewInsert().Model(&TestTable{ID: uuid.New(), Name: tc.name}).Exec(ctx)
					require.NoError(t, ierr)
					require.NoError(t, sp.AcquireAdvisoryLock(ctx, lockID, false))
					if tc.failStatement {
						_, serr := GetIDB(sp, dbSession).ExecContext(ctx, "SELECT 1/0")
						require.Error(t, serr)
					}
					if tc.cancelCtx {
						cancel()
					}
					return tc.fnErr
				})

				assert.Equal(t, tc.wantSavepointErr, errors.Is(err, ErrTransactionSavepoint))
				if tc.fnErr != nil {
					assert.ErrorIs(t, err, tc.fnErr)
				}
			}
			assert.Equal(t, tc.wantFnCalled, fnCalled)
			if tc.wantSavepointErr {
				return
			}
			if tc.fnErr == nil {
				require.NoError(t, err)
			}

			// This query fails if the failed statement in fn left the outer transaction aborted.
			count, cerr := GetIDB(tx, dbSession).NewSelect().Model((*TestTable)(nil)).Where("name = ?", tc.name).Count(ctx)
			require.NoError(t, cerr)
			assert.Equal(t, tc.wantKept, count == 1)

			other, oerr := BeginTx(ctx, dbSession, nil)
			require.NoError(t, oerr)
			defer func() { _ = other.Rollback() }()
			lerr := other.AcquireAdvisoryLock(ctx, lockID, false)
			if tc.wantKept {
				assert.ErrorIs(t, lerr, ErrXactAdvisoryLockFailed)
			} else {
				assert.NoError(t, lerr)
			}

			if tc.fnErr != nil {
				// Each savepoint left open after its rollback gets its own transaction ID once tx writes.
				_, ierr := GetIDB(tx, dbSession).NewInsert().Model(&TestTable{ID: uuid.New(), Name: tc.name + "-outer"}).Exec(ctx)
				require.NoError(t, ierr)
				xids, xerr := GetIDB(tx, dbSession).NewSelect().TableExpr("pg_locks").
					Where("locktype = 'transactionid'").Where("pid = pg_backend_pid()").Count(ctx)
				require.NoError(t, xerr)
				assert.Equal(t, 1, xids, "every rolled back savepoint must also be released")
			}
		})
	}
}

func TestGetBunTx(t *testing.T) {
	dbSession := testTxGetTestSession(t)
	defer dbSession.Close()
	ctx := context.Background()
	tx1, err := BeginTx(ctx, dbSession, &sql.TxOptions{})
	defer tx1.Rollback()
	assert.Nil(t, err)
	buntxp := tx1.GetBunTx()
	assert.NotNil(t, buntxp)
}
