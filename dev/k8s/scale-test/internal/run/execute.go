// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package run

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"slices"
	"time"

	"github.com/google/uuid"

	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/config"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/journal"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/jsonx"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/lock"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/result"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/writer"
)

// releasePending is the reason of the pending record written before the lock release.
const releasePending = "terminal record pending: the site lock release is not confirmed yet"

// RunCtx is what a test runs with: the run context, the shared clock and the run's bounds. The context is cancelled
// by the control (a lost lock, a drain or the outside cancel).
type RunCtx struct {
	Ctx          context.Context
	Clock        Clock
	Deadline     time.Time
	EvidenceEnd  time.Time
	ProbeReadEnd time.Time
	DryRun       bool
}

// Test is a scale test the runner drives. Run returns the outcome it reached; the runner turns that into the
// published verdict. F2 and ingestion implement it.
type Test interface {
	Name() string
	LockKind() lock.HolderKind
	Writes() bool
	Run(ctx RunCtx, w *writer.Writer) (result.Outcome, error)
}

// Deps are what a run needs. The lower fields are seams tests set; a production run leaves them zero.
type Deps struct {
	Config *config.Config
	Lock   lock.Port
	CM     result.ConfigMaps
	Clock  Clock
	// Outside is the outside-cancel context, as from SIGTERM. Nil is background.
	Outside context.Context

	ResultBudget   uint64                              // 0 uses config.ResultMaxBytes
	ShutdownBudget time.Duration                       // 0 uses ShutdownBudget
	DrainBudget    time.Duration                       // 0 uses Drain
	OnWatchdog     func()                              // nil exits the process
	JournalFault   func(f *os.File, line []byte) error // nil leaves journal I/O alone; a test seam

	// NewJournal replaces journal.Create so a test can stall or fail the initial create; nil uses journal.Create.
	NewJournal func(dir string, h journal.Header) (*journal.Journal, error)
}

type claimState uint8

const (
	claimUntried claimState = iota
	claimOwned
	claimRefused
)

type execution struct {
	deps    Deps
	test    Test
	clock   Clock
	outside context.Context
	bounds  Bounds
	control *Control

	token   string
	claim   claimState
	uid     string
	held    lock.Held
	watch   *Watch
	writer  *writer.Writer
	journal *journal.Journal
	runDone chan struct{} // closed when the Test.Run callback returns

	reasons      []string
	detail       json.RawMessage // the test's findings, kept for result.json
	stages       []result.StageResult
	invariants   []result.InvariantResult
	knownFailure string
	timedOut     bool
	abandoned    bool // the Run callback did not stop within the drain
	lock         result.LockResult
}

// Execute runs test to its end and returns its complete result (the result ConfigMap holds the summary when it owns
// it). Three independent bounds (an absolute maximum, an outside-cancel bound, finish's teardown bound) keep a hang
// from leaving the Lease renewing; each only shortens the deadline, and expiry exits 2.
func Execute(d Deps, test Test) result.RunResult {
	e := newExecution(d, test)
	budget := e.shutdownBudget()
	abs := armWatchdog(time.Until(e.bounds.EvidenceEnd)+budget, e.watchdogAction)
	defer abs.disarm()
	onCancel := armOnCancel(e.outside, budget, e.watchdogAction)
	defer onCancel.disarm()
	verdict := e.drive()
	return e.finish(verdict)
}

func (e *execution) shutdownBudget() time.Duration {
	if e.deps.ShutdownBudget > 0 {
		return e.deps.ShutdownBudget
	}
	return ShutdownBudget
}

func newExecution(d Deps, test Test) *execution {
	if d.Clock == nil {
		d.Clock = RealClock{}
	}
	outside := d.Outside
	if outside == nil {
		outside = context.Background()
	}
	e := &execution{
		deps: d, test: test, clock: d.Clock, outside: outside, control: NewControl(),
		bounds: Derive(d.Config, d.Clock), token: uuid.NewString(),
		lock: result.LockResult{Name: d.Config.Site.LockName, Kind: string(test.LockKind())},
	}
	return e
}

func (e *execution) reason(r string) { e.reasons = append(e.reasons, r) }

func (e *execution) hasReason(r string) bool { return slices.Contains(e.reasons, r) }

func (e *execution) dropReason(r string) {
	e.reasons = slices.DeleteFunc(e.reasons, func(s string) bool { return s == r })
}

func (e *execution) owner() string { return e.deps.Config.ExecutionID + "/" + e.token }

func (e *execution) resultMaxBytes() uint64 {
	if e.deps.ResultBudget > 0 {
		return e.deps.ResultBudget
	}
	return uint64(e.deps.Config.ResultMaxBytes)
}

// drive runs the steps up to the verdict: claim, lock, journal, writer, watch, the test, then the run-level verdict.
func (e *execution) drive() result.Verdict {
	cfg := e.deps.Config
	// Before the lock: a duplicate attempt never writes over the owner's result.
	if err := e.claimResult(e.outside); err != nil {
		e.reason(err.Error())
		return result.Refused
	}
	// Before the lock and the writer: a writing command needs dryRun set and a non-writing one needs it absent, so a
	// writing test with dryRun missing cannot send real writes and still pass.
	if err := cfg.CheckDryRun(e.test.Writes()); err != nil {
		e.reason(err.Error())
		return result.Refused
	}
	me := lock.HolderRecord{Kind: e.test.LockKind(), RunID: cfg.RunID, ExecutionID: cfg.ExecutionID, State: lock.Running, Test: ptr(e.test.Name())}
	held, err := e.deps.Lock.Acquire(e.outside, me)
	if err != nil {
		e.reason(err.Error())
		return result.Refused
	}
	e.held = held
	e.lock.AcquiredAt = jsonxTime(e.clock.Now())
	gate := held.Gate()
	create := journal.Create
	if e.deps.NewJournal != nil { // a test seam to stall or fail the initial create
		create = e.deps.NewJournal
	}
	j, err := create(cfg.Journal.Dir, journal.Header{RunID: cfg.RunID, Command: e.test.Name()})
	if err != nil {
		e.reason(err.Error())
		return result.Refused
	}
	e.journal = j
	if e.deps.JournalFault != nil { // a test seam to stall or fail journal I/O
		j.Inject(e.deps.JournalFault)
	}
	// The held lock's gate and the soft-stop channel are wired into the writer: a lost lock or a stop halts the next dispatch.
	e.writer = writer.New(j, gate, e.control.Stopper(), e.bounds.Deadline)
	e.watch = SpawnWatch(e.control, gate, e.clock, e.outside)
	ctx := RunCtx{Ctx: e.control.Context(), Clock: e.clock, Deadline: e.bounds.Deadline,
		EvidenceEnd: e.bounds.EvidenceEnd, ProbeReadEnd: e.bounds.ProbeReadEnd, DryRun: cfg.DryRun != nil && *cfg.DryRun}
	outcome, runErr := e.runTest(ctx)
	return e.verdictOf(outcome, runErr)
}

type ran struct {
	out result.Outcome
	err error
}

// runTest runs the test bounded by the evidence end, the run's own cancel and the outside cancel; a run still going is
// drained. runDone closes when the callback returns, so finish can still join a dropped one under the watchdog.
func (e *execution) runTest(ctx RunCtx) (result.Outcome, error) {
	e.runDone = make(chan struct{})
	res := make(chan ran, 1)
	go func() {
		defer close(e.runDone)
		o, err := e.test.Run(ctx, e.writer)
		res <- ran{o, err}
	}()
	bound := time.NewTimer(time.Until(e.bounds.EvidenceEnd))
	defer bound.Stop()
	select {
	case r := <-res:
		if e.clock.Instant().After(e.bounds.EvidenceEnd) {
			e.timedOut = true
			e.reason("the run returned after its evidence end")
		}
		return r.out, r.err
	case <-bound.C:
		e.timedOut = true
		e.reason("the run passed its evidence end, the deadline plus the finalize budget, and was cancelled")
	case <-e.outside.Done():
		e.reason("the run was cancelled from outside")
	case <-e.control.Context().Done():
	}
	// Cancel the run and wait the drain for it to stop. A run that does not stop is not forgotten: it is marked
	// abandoned (its writes may still be unsettled) and joined later under the watchdog.
	e.control.Cancel()
	drain := e.deps.DrainBudget
	if drain == 0 {
		drain = Drain
	}
	select {
	case r := <-res:
		return r.out, r.err
	case <-time.After(drain):
		e.abandoned = true
		return result.Outcome{}, fmt.Errorf("the run did not stop within %d s of its cancel and was dropped", int(drain/time.Second))
	}
}

// verdictOf is the run-level verdict: a lost lock or a hard cancel outranks the outcome; a soft stop is soft stopped.
// The test's reasons, findings, stages and invariants are kept whatever the verdict.
func (e *execution) verdictOf(outcome result.Outcome, runErr error) result.Verdict {
	e.reasons = append(e.reasons, outcome.Reasons...)
	e.detail, e.stages, e.invariants = outcome.Detail, outcome.Stages, outcome.Invariants
	hard := e.timedOut || e.outside.Err() != nil || e.control.Cancelled()
	if runErr != nil {
		e.reason(runErr.Error())
		if !e.lockLost() && !hard && errors.Is(runErr, writer.ErrSoftStop) {
			return result.SoftStopped
		}
		return result.Aborted
	}
	switch {
	case e.lockLost():
		e.reason("site lock lost during the run")
		return result.Aborted
	case hard:
		return result.Aborted
	case e.control.StopRequested():
		e.reason("soft stop requested")
		return result.SoftStopped
	default:
		return outcome.Verdict
	}
}

// finish tears the run down under its own teardown watchdog (the shutdown budget from teardown entry), so a blocked
// join or fsync after a quick run cannot wait out the whole-run window. Publication runs on the detached cleanup
// context, so an outside cancel is the abort reason, not a reason to skip recording it.
func (e *execution) finish(verdict result.Verdict) result.RunResult {
	wd := armWatchdog(e.shutdownBudget(), e.watchdogAction)
	defer wd.disarm()
	ctx, cancel := context.WithTimeout(context.WithoutCancel(e.outside), CleanupBudget)
	defer cancel()

	if e.held != nil {
		e.held.Gate().Seal() // no further dispatch during teardown
	}
	if e.watch != nil {
		e.watch.Stop()
	}
	if e.journal != nil {
		e.journal.Freeze()
	}
	e.joinRun()          // join the Run callback; a stuck one is bounded by the watchdog
	if e.writer != nil { // join the attempt goroutines
		e.writer.Close()
	}
	// drive always attempts the claim first, so by here it is owned or refused, never untried.
	var written result.RunResult
	if e.claim == claimOwned {
		written = e.publish(ctx, &verdict)
	} else {
		written, _, _ = e.fit(verdict)
	}
	if e.held != nil {
		e.held.Close() // join renewal, even on a lost gate; no renewal runs after this returns
	}
	if e.journal != nil {
		_ = e.journal.Close()
	}
	e.control.Cancel() // release the run context and stopper
	return written
}

// joinRun waits for the Test.Run callback to return. One that stopped (or never started) returns at once; a dropped
// one that ignores cancel blocks here, bounded by the absolute watchdog, so none is left running after Execute returns.
func (e *execution) joinRun() {
	if e.runDone != nil {
		<-e.runDone
	}
}

func (e *execution) watchdogAction() {
	if e.deps.OnWatchdog != nil {
		e.deps.OnWatchdog()
		return
	}
	os.Exit(result.ExitNotRun)
}

// unsettled is true when this run's writes cannot be proved settled: an abandoned callback, or a journal with an open
// op or a failed append. Such a run must not release the site clean, and must not publish a clean verdict.
func (e *execution) unsettled() bool {
	return e.abandoned || (e.journal != nil && e.journal.Unsettled())
}

// releaseState is the holder state this run releases with: an unsettled run stays needsCleanup, so the next run is
// refused while a write it recorded may still land, and a settled run releases final.
func (e *execution) releaseState() lock.HolderState {
	if e.unsettled() {
		return lock.NeedsCleanup
	}
	return lock.Final
}

// publish writes a non-success pending record, releases the lock, then writes the final record, so a crash between
// the release and the final write leaves the pending record, never a success. ctx is the detached cleanup context.
func (e *execution) publish(ctx context.Context, published *result.Verdict) result.RunResult {
	verdict := *published
	var lostAt *time.Time
	if e.watch != nil {
		lostAt = e.watch.LostAt()
	}
	if lostAt == nil && e.lockLost() {
		now := e.clock.Now()
		lostAt = &now
	}
	e.lock.LostAt = jsonxPtr(lostAt)
	if lostAt != nil && result.Lost(verdict) != verdict {
		e.reason("site lock lost before the run could finish")
		verdict = result.Lost(verdict)
	}
	// An unsettled run is never published clean: a success becomes Unresolved, and the site is held for cleanup.
	if e.unsettled() {
		e.reason("site retained for cleanup: a write this run recorded may still be unsettled")
		verdict = result.NotClean(verdict)
	}
	releasing := e.held != nil && lostAt == nil
	if releasing {
		state := e.releaseState()
		e.lock.FinalState = nil
		e.reason(releasePending)
		pending := result.NotClean(verdict)
		if _, first := e.persist(ctx, &pending); first != nil {
			e.dropReason(releasePending)
			e.reason(first.Error())
			verdict = result.NotClean(verdict)
		} else {
			e.dropReason(releasePending)
		}
		// Re-check the Lease before the release: a lost gate is Aborted. LeaseWritable, not Check, so the teardown
		// seal is not read as a loss.
		var relErr error
		if !e.held.Gate().LeaseWritable() {
			relErr = lock.ErrLost
		} else {
			relErr = e.held.Release(ctx, state)
		}
		switch {
		case relErr == nil:
			s := string(state)
			e.lock.FinalState = &s
		case isLockLost(relErr):
			e.lock.LostAt = jsonxTime(e.clock.Now())
			e.reason("site lock lost before its release")
			verdict = result.Lost(verdict)
		default:
			e.reason("site lock release not confirmed: " + relErr.Error())
			verdict = result.NotClean(verdict)
		}
	}
	written, failure := e.persist(ctx, &verdict)
	if failure != nil && !releasing {
		e.reason(failure.Error())
		verdict = result.NotClean(verdict)
		written, failure = e.persist(ctx, &verdict)
	}
	if failure != nil {
		if !e.hasReason(failure.Error()) {
			e.reason(failure.Error())
		}
		verdict = result.NotClean(verdict)
		written, _, _ = e.fit(verdict)
	}
	*published = verdict
	return written
}

// fit builds the complete result of verdict, writes it atomically to result.json on the PVC, and returns the small
// ConfigMap status referencing it, plus the failure known before the status write: the file was unwritten or too big.
func (e *execution) fit(verdict result.Verdict) (result.RunResult, result.Status, error) {
	full := e.result(verdict)
	var known string
	data, err := jsonx.Encode(&full)
	switch {
	case err != nil:
		known = "result not serialized: " + err.Error()
	case uint64(len(data)) > e.resultMaxBytes():
		known = fmt.Sprintf("result of %d bytes is over resultMaxBytes %d", len(data), e.resultMaxBytes())
	default:
		if werr := e.resultFile().WriteBytes(data); werr != nil {
			known = "result file not written: " + werr.Error()
		}
	}
	if known != "" {
		full.Result = result.NoRef()
		return full, result.StatusOf(&full), errors.New(known)
	}
	return full, result.StatusOf(&full), nil
}

// persist writes the authoritative result.json and the ConfigMap status. A failure known before the status is written
// (the result file) is folded into the verdict and reasons of that status, so a published success is one the run can back.
func (e *execution) persist(ctx context.Context, verdict *result.Verdict) (result.RunResult, error) {
	if e.knownFailure != "" {
		e.dropReason(e.knownFailure)
		e.knownFailure = ""
	}
	full, status, known := e.fit(*verdict)
	if known != nil {
		if !e.hasReason(known.Error()) {
			e.reason(known.Error())
			e.knownFailure = known.Error()
		}
		*verdict = result.NotClean(*verdict)
		full, status, _ = e.fit(*verdict)
	}
	return full, e.writeStatus(ctx, status)
}

func (e *execution) writeStatus(ctx context.Context, status result.Status) error {
	if e.claim != claimOwned {
		return errors.New("this attempt does not own the result ConfigMap")
	}
	json, err := jsonx.Encode(status)
	if err != nil {
		return fmt.Errorf("result status did not serialize: %w", err)
	}
	name := e.deps.Config.ConfigMaps.Result
	if err := result.WriteResult(ctx, e.deps.CM, name, e.owner(), e.uid, string(json)); err != nil {
		return fmt.Errorf("result status not written to %s: %w", name, err)
	}
	return nil
}

func (e *execution) result(verdict result.Verdict) result.RunResult {
	cfg := e.deps.Config
	return result.RunResult{
		SchemaVersion: result.SchemaVersion, Test: e.test.Name(), RunID: cfg.RunID, ExecutionID: cfg.ExecutionID,
		DryRun: cfg.DryRun, Outcome: result.OutcomeOf(verdict), Reasons: append([]string{}, e.reasons...),
		Clock: result.RunClock{StartedAt: jsonx.Time{Time: e.bounds.Started}, Deadline: jsonxTime(e.bounds.Deadline),
			EndedAt: jsonx.Time{Time: e.clock.Now()}, ClockSyncAssumption: result.ClockSyncAssumption},
		Lock: e.lock, Stages: append([]result.StageResult{}, e.stages...), Invariants: append([]result.InvariantResult{}, e.invariants...),
		Journal: e.journalCounts(), Result: e.resultRef(), Detail: e.detail,
	}
}

func (e *execution) resultRef() result.ResultRef {
	return result.ResultRef{Claim: e.deps.Config.Journal.Claim, Path: e.resultFile().Path}
}

// resultFile is the run's authoritative result.json once this attempt owns the result, else this attempt's own file.
func (e *execution) resultFile() result.ResultFile {
	cfg := e.deps.Config
	if e.claim == claimOwned {
		return result.ResultFor(cfg.Journal.Dir, cfg.RunID)
	}
	return result.ResultFile{Path: filepath.Join(cfg.Journal.Dir, cfg.RunID+"."+e.token+".result.json")}
}

func (e *execution) claimResult(ctx context.Context) error {
	cfg := e.deps.Config
	uid, err := result.Claim(ctx, e.deps.CM, cfg.ConfigMaps.Result, e.owner())
	if err != nil {
		e.claim = claimRefused
		return fmt.Errorf("result ConfigMap %s: %w", cfg.ConfigMaps.Result, err)
	}
	e.claim, e.uid = claimOwned, uid
	return nil
}

// journalCounts is none_written when there is no journal, else the journal's own snapshot (see journal.Counts: attempts
// are RPCs that went out or may have, so a dry run reports none), or unknown (nil) once a journal write has failed.
func (e *execution) journalCounts() result.JournalSummary {
	if e.journal == nil {
		return result.NoneWritten()
	}
	if c := e.journal.Counts(); c.Known {
		return result.JournalSummary{Ops: &c.Ops, Attempts: &c.Attempts, Unclear: &c.Unclear}
	}
	return result.JournalSummary{}
}

// lockLost is true when the watch saw the gate close, or the Lease is no longer writable now. It reads LeaseWritable,
// not Check, so a teardown seal — which Check reports as lost — is never mistaken for a genuine loss.
func (e *execution) lockLost() bool {
	if e.watch != nil && e.watch.LostAt() != nil {
		return true
	}
	return e.held != nil && !e.held.Gate().LeaseWritable()
}

func isLockLost(err error) bool {
	var le lock.Error
	return errors.As(err, &le) && le.Kind == lock.KindLost
}

func ptr[T any](v T) *T { return &v }

func jsonxTime(t time.Time) *jsonx.Time { return &jsonx.Time{Time: t} }

func jsonxPtr(t *time.Time) *jsonx.Time {
	if t == nil {
		return nil
	}
	return &jsonx.Time{Time: *t}
}
