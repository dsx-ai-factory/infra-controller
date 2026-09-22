// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package scheduler

import (
	"context"
	"errors"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/stretchr/testify/require"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule/executor"
)

func TestScheduler_Start(t *testing.T) {
	t.Run("dispatches a pending execution", func(t *testing.T) {
		store := newFakeStore()
		store.claims["pending"] = []eventrule.ClaimedExecution{
			newClaimedExecution(t, "pending_action", 1, testInstanceID),
		}
		var executorCalls atomic.Int32
		configured := newTestScheduler(t, store, executorRegistryFunc(
			func(eventrule.ActionType) (executor.Executor, error) {
				return executorFunc(func(context.Context, executor.ExecutionRequest) error {
					executorCalls.Add(1)

					return nil
				}), nil
			},
		))

		require.NoError(t, configured.Start(context.Background()))

		transition := receiveTransition(t, store.transitions)
		require.Equal(t, eventrule.ExecutionStatusCompleted, transition.result.Status)
		require.EqualValues(t, 1, executorCalls.Load())

		require.NoError(t, configured.Stop())
	})

	t.Run("processor signal wakes a sleeping scheduler", func(t *testing.T) {
		store := newFakeStore()
		configured := newTestScheduler(t, store, successfulExecutorRegistry())
		require.NoError(t, configured.Start(context.Background()))

		require.Eventually(t, func() bool { return store.requestCount() >= 2 }, time.Second, time.Millisecond)

		store.enqueue(
			"pending",
			newClaimedExecution(t, "signaled_action", 1, testInstanceID),
		)
		configured.Notify()

		transition := receiveTransition(t, store.transitions)
		require.Equal(t, "signaled_action", transition.actionName)

		require.NoError(t, configured.Stop())
	})

	t.Run("polling finds work after a dropped signal", func(t *testing.T) {
		store := newFakeStore()
		config := validConfig()
		config.Dependencies.Store = store
		config.Runtime.PollInterval = 10 * time.Millisecond
		configured, err := New(config)
		require.NoError(t, err)

		require.NoError(t, configured.Start(context.Background()))

		require.Eventually(t, func() bool { return store.requestCount() >= 2 }, time.Second, time.Millisecond)

		store.enqueue(
			"pending",
			newClaimedExecution(t, "polled_action", 1, testInstanceID),
		)

		transition := receiveTransition(t, store.transitions)
		require.Equal(t, "polled_action", transition.actionName)

		require.NoError(t, configured.Stop())
	})

	t.Run("worker completion schedules the next execution", func(t *testing.T) {
		store := newFakeStore()
		store.claims["pending"] = []eventrule.ClaimedExecution{
			newClaimedExecution(t, "first_action", 1, testInstanceID),
			newClaimedExecution(t, "second_action", 1, testInstanceID),
		}

		configured := newTestScheduler(t, store, successfulExecutorRegistry())
		require.NoError(t, configured.Start(context.Background()))

		first := receiveTransition(t, store.transitions)
		second := receiveTransition(t, store.transitions)
		require.ElementsMatch(t, []string{"first_action", "second_action"}, []string{
			first.actionName,
			second.actionName,
		})

		require.NoError(t, configured.Stop())
	})

	t.Run("keeps deferred capacity independent from blocked pending work", func(t *testing.T) {
		store := newFakeStore()
		store.claims["pending"] = []eventrule.ClaimedExecution{
			newClaimedExecution(t, "pending_action", 1, testInstanceID),
		}
		store.claims["deferred"] = []eventrule.ClaimedExecution{
			newClaimedExecution(t, "deferred_action", 2, testInstanceID),
		}
		pendingStarted := make(chan struct{})
		releasePending := make(chan struct{})
		configured := newTestScheduler(t, store, executorRegistryFunc(
			func(eventrule.ActionType) (executor.Executor, error) {
				return executorFunc(func(_ context.Context, request executor.ExecutionRequest) error {
					plan := request.Plan.(*eventrule.NoopPlan)
					if plan.Reason == "pending_action" {
						close(pendingStarted)
						<-releasePending
					}

					return nil
				}), nil
			},
		))

		require.NoError(t, configured.Start(context.Background()))

		receiveSignal(t, pendingStarted, "pending worker did not start")
		transition := receiveTransition(t, store.transitions)
		require.Equal(t, "deferred_action", transition.actionName)
		close(releasePending)
		transition = receiveTransition(t, store.transitions)
		require.Equal(t, "pending_action", transition.actionName)

		require.NoError(t, configured.Stop())
	})

	t.Run("continues after a claim failure", func(t *testing.T) {
		store := newFakeStore()
		store.enqueue(
			"pending",
			newClaimedExecution(t, "pending_action", 1, testInstanceID),
		)
		store.setClaimError("pending", errors.New("store unavailable"))
		configured := newTestScheduler(t, store, successfulExecutorRegistry())

		require.NoError(t, configured.Start(context.Background()))
		require.Eventually(
			t,
			func() bool { return store.requestCount() >= 1 },
			time.Second,
			time.Millisecond,
		)
		store.setClaimError("pending", nil)
		configured.Notify()

		transition := receiveTransition(t, store.transitions)

		require.Equal(t, "pending_action", transition.actionName)
		require.GreaterOrEqual(t, store.requestCount(), 2)
		require.NoError(t, configured.Stop())
	})

	t.Run("continues after a transition failure", func(t *testing.T) {
		tests := map[string]error{
			"store failure": errors.New("store unavailable"),
			"claim lost":    eventrule.ErrExecutionClaimLost,
		}

		for name, transitionErr := range tests {
			t.Run(name, func(t *testing.T) {
				store := newFakeStore()
				store.claims["pending"] = []eventrule.ClaimedExecution{
					newClaimedExecution(t, "first_action", 1, testInstanceID),
					newClaimedExecution(t, "second_action", 1, testInstanceID),
				}
				store.transitionErr = transitionErr
				configured := newTestScheduler(t, store, successfulExecutorRegistry())
				require.NoError(t, configured.Start(context.Background()))

				first := receiveTransition(t, store.transitions)
				second := receiveTransition(t, store.transitions)
				require.Equal(t, "first_action", first.actionName)
				require.Equal(t, "second_action", second.actionName)

				require.NoError(t, configured.Stop())
			})
		}
	})

	t.Run("rejects a second start", func(t *testing.T) {
		configured := newTestScheduler(t, newFakeStore(), successfulExecutorRegistry())

		require.NoError(t, configured.Start(context.Background()))
		require.EqualError(
			t,
			configured.Start(context.Background()),
			"scheduler can only be started once",
		)
		require.NoError(t, configured.Stop())
	})
}

func TestScheduler_Stop(t *testing.T) {
	t.Run("waits for active workers", func(t *testing.T) {
		store := newFakeStore()
		store.claims["pending"] = []eventrule.ClaimedExecution{
			newClaimedExecution(t, "pending_action", 4, testInstanceID),
		}
		executionStarted := make(chan struct{})
		executionStopped := make(chan struct{})
		configured := newTestScheduler(t, store, executorRegistryFunc(
			func(eventrule.ActionType) (executor.Executor, error) {
				return executorFunc(func(ctx context.Context, _ executor.ExecutionRequest) error {
					close(executionStarted)
					<-ctx.Done()
					close(executionStopped)

					return ctx.Err()
				}), nil
			},
		))

		require.NoError(t, configured.Start(context.Background()))
		receiveSignal(t, executionStarted, "execution did not start")
		require.NoError(t, configured.Stop())
		receiveSignal(t, executionStopped, "execution did not stop")

		transition := receiveTransition(t, store.transitions)
		require.Equal(t, eventrule.ExecutionStatusDeferred, transition.result.Status)
		require.Equal(t, eventrule.ExecutionReasonAttemptInterrupted, transition.result.Reason)
	})

	t.Run("rejects stop before start", func(t *testing.T) {
		configured := newTestScheduler(t, newFakeStore(), successfulExecutorRegistry())

		require.EqualError(
			t,
			configured.Stop(),
			"scheduler cannot be stopped before it is started",
		)
	})

	t.Run("rejects a second stop", func(t *testing.T) {
		configured := newTestScheduler(t, newFakeStore(), successfulExecutorRegistry())

		require.NoError(t, configured.Start(context.Background()))
		require.NoError(t, configured.Stop())
		require.EqualError(
			t,
			configured.Stop(),
			"scheduler can only be stopped once",
		)
	})
}

func TestScheduler_Notify(t *testing.T) {
	configured := newTestScheduler(t, newFakeStore(), successfulExecutorRegistry())

	for range 20 {
		configured.Notify()
	}

	require.Len(t, configured.runtime.wakeCh, 1)
}

func TestScheduler_refill(t *testing.T) {
	t.Run("refills lanes in priority order", func(t *testing.T) {
		store := newFakeStore()
		configured := newTestScheduler(t, store, successfulExecutorRegistry())

		require.NoError(t, configured.refill(context.Background()))
		require.Equal(t, []string{"pending", "deferred"}, store.requestLanes)
	})

	t.Run("stops after reserved capacity cannot be handed off", func(t *testing.T) {
		store := newFakeStore()
		store.claims["pending"] = []eventrule.ClaimedExecution{
			newClaimedExecution(t, "claimed_action", 1, testInstanceID),
		}
		configured := newTestScheduler(t, store, successfulExecutorRegistry())
		pending := configured.lanes[0]
		pending.jobs <- newClaimedExecution(t, "occupying_action", 1, testInstanceID)

		err := configured.refill(context.Background())

		require.EqualError(
			t,
			err,
			"pending work channel has no capacity despite reserved worker slots",
		)
		require.Empty(t, pending.slots)
		require.Empty(t, store.transitions)
	})

	t.Run("wakes after a full scan produces no claims", func(t *testing.T) {
		store := newFakeStore()
		store.scanLimitReached["deferred"] = true
		configured := newTestScheduler(t, store, successfulExecutorRegistry())

		require.NoError(t, configured.refill(context.Background()))
		require.Len(t, configured.runtime.wakeCh, 1)
	})
}

const testInstanceID = "scheduler-test"

type transitionRecord struct {
	executionID uuid.UUID
	actionName  string
	token       uuid.UUID
	result      eventrule.ExecutionResult
	contextErr  error
}

type fakeStore struct {
	mu               sync.Mutex
	claims           map[string][]eventrule.ClaimedExecution
	scanLimitReached map[string]bool
	claimErrors      map[string]error
	requests         []eventrule.ExecutionClaimRequest
	requestLanes     []string
	actionNames      map[uuid.UUID]string
	transitionErr    error
	transitions      chan transitionRecord
}

func newFakeStore() *fakeStore {
	return &fakeStore{
		claims:           make(map[string][]eventrule.ClaimedExecution),
		scanLimitReached: make(map[string]bool),
		claimErrors:      make(map[string]error),
		actionNames:      make(map[uuid.UUID]string),
		transitions:      make(chan transitionRecord, 10),
	}
}

func (s *fakeStore) enqueue(
	lane string,
	claims ...eventrule.ClaimedExecution,
) {
	s.mu.Lock()
	defer s.mu.Unlock()

	s.claims[lane] = append(s.claims[lane], claims...)
}

func (s *fakeStore) setClaimError(lane string, err error) {
	s.mu.Lock()
	defer s.mu.Unlock()

	s.claimErrors[lane] = err
}

func (s *fakeStore) requestCount() int {
	s.mu.Lock()
	defer s.mu.Unlock()

	return len(s.requests)
}

func (s *fakeStore) ClaimPendingExecutions(
	ctx context.Context,
	request eventrule.ExecutionClaimRequest,
) (eventrule.ExecutionClaimBatch, error) {
	return s.claim(ctx, request, "pending")
}

func (s *fakeStore) ClaimRetryExecutions(
	ctx context.Context,
	request eventrule.ExecutionClaimRequest,
) (eventrule.ExecutionClaimBatch, error) {
	return s.claim(ctx, request, "deferred")
}

func (s *fakeStore) claim(
	ctx context.Context,
	request eventrule.ExecutionClaimRequest,
	lane string,
) (eventrule.ExecutionClaimBatch, error) {
	s.mu.Lock()
	defer s.mu.Unlock()

	if err := ctx.Err(); err != nil {
		return eventrule.ExecutionClaimBatch{}, err
	}

	s.requests = append(s.requests, request)
	s.requestLanes = append(s.requestLanes, lane)

	if err := s.claimErrors[lane]; err != nil {
		return eventrule.ExecutionClaimBatch{}, err
	}

	available := s.claims[lane]
	count := min(request.Limit, len(available))
	claimed := make([]eventrule.ClaimedExecution, count)
	copy(claimed, available[:count])
	for i := range claimed {
		s.actionNames[claimed[i].Execution.ID] = claimed[i].Execution.ActionName
	}

	s.claims[lane] = available[count:]

	return eventrule.ExecutionClaimBatch{
		Claims:           claimed,
		ScanLimitReached: s.scanLimitReached[lane] || len(available) >= request.Limit,
	}, nil
}

func (s *fakeStore) TransitionClaimedExecution(
	ctx context.Context,
	executionID uuid.UUID,
	token uuid.UUID,
	result eventrule.ExecutionResult,
) error {
	s.mu.Lock()
	transitionErr := s.transitionErr
	actionName := s.actionNames[executionID]
	s.mu.Unlock()

	record := transitionRecord{
		executionID: executionID,
		actionName:  actionName,
		token:       token,
		result:      result,
		contextErr:  ctx.Err(),
	}

	select {
	case s.transitions <- record:
	case <-ctx.Done():
		return ctx.Err()
	}

	return transitionErr
}

func newClaimedExecution(
	t *testing.T,
	actionName string,
	attempts int,
	owner string,
) eventrule.ClaimedExecution {
	t.Helper()

	createdAt := time.Date(2026, 8, 24, 12, 0, 0, 0, time.UTC)
	execution, err := eventrule.NewExecution(
		uuid.New(),
		actionName,
		&eventrule.NoopPlan{Reason: actionName},
		createdAt,
	)
	require.NoError(t, err)

	token := uuid.New()
	claimAt := createdAt.Add(time.Second)
	disposition, err := execution.AcquireClaim(
		owner,
		token,
		claimAt,
		time.Now().Add(time.Minute),
		4,
	)
	require.NoError(t, err)
	require.Equal(t, eventrule.ClaimAcquired, disposition)

	execution.Attempts = attempts
	require.NoError(t, execution.Validate())

	return eventrule.ClaimedExecution{Execution: *execution, Token: token}
}

type executorRegistryFunc func(eventrule.ActionType) (executor.Executor, error)

func (f executorRegistryFunc) Executor(actionType eventrule.ActionType) (executor.Executor, error) {
	return f(actionType)
}

type executorFunc func(context.Context, executor.ExecutionRequest) error

func (f executorFunc) Execute(ctx context.Context, request executor.ExecutionRequest) error {
	return f(ctx, request)
}

func validConfig() Config {
	return Config{
		InstanceID: testInstanceID,
		Dependencies: Dependencies{
			Store:     newFakeStore(),
			Executors: successfulExecutorRegistry(),
		},
		Runtime: DefaultRuntimeConfig(),
		Policy:  DefaultPolicyConfig(),
	}
}

func newTestScheduler(
	t *testing.T,
	store eventrule.ExecutionStore,
	executors ExecutorRegistry,
) *Scheduler {
	t.Helper()

	config := validConfig()
	config.Dependencies.Store = store
	config.Dependencies.Executors = executors

	configured, err := New(config)
	require.NoError(t, err)

	return configured
}

func successfulExecutorRegistry() ExecutorRegistry {
	return executorRegistryFunc(func(eventrule.ActionType) (executor.Executor, error) {
		return executorFunc(func(context.Context, executor.ExecutionRequest) error {
			return nil
		}), nil
	})
}

func receiveTransition(t *testing.T, transitions <-chan transitionRecord) transitionRecord {
	t.Helper()

	select {
	case transition := <-transitions:
		return transition
	case <-time.After(5 * time.Second):
		t.Fatal("scheduler did not persist an execution transition")
		return transitionRecord{}
	}
}

func receiveSignal(t *testing.T, signal <-chan struct{}, message string) {
	t.Helper()

	select {
	case <-signal:
	case <-time.After(5 * time.Second):
		t.Fatal(message)
	}
}

var _ eventrule.ExecutionStore = (*fakeStore)(nil)
var _ ExecutorRegistry = executorRegistryFunc(nil)
var _ executor.Executor = executorFunc(nil)
