// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package run

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"os"
	"os/exec"
	"path/filepath"
	"testing"
	"time"

	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/journal"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/result"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/writer"
)

type firmwareTest struct {
	fakeTest
	budget  time.Duration
	cleanup func(context.Context, *writer.Writer) (json.RawMessage, error)
}

func (firmwareTest) Name() string                           { return "firmware" }
func (f firmwareTest) FirmwareCleanupBudget() time.Duration { return f.budget }
func (f firmwareTest) CloseFirmwareWindows(ctx context.Context, w *writer.Writer) (json.RawMessage, error) {
	return f.cleanup(ctx, w)
}

func TestFirmwareFinalizerIsolation(t *testing.T) {
	for _, mode := range []string{"normal", "cancel", "soft stop", "lost Lease", "journal failure", "cleanup failure", "cleanup deadline", "cleanup Lease loss", "invalid budget"} {
		t.Run(mode, func(t *testing.T) {
			seq := &sequence{}
			h := held(seq)
			d := deps(t, seq, h)
			ctx, cancel := context.WithCancel(context.Background())
			defer cancel()
			d.Outside = ctx
			called, sent := false, false
			f := firmwareTest{budget: time.Minute}
			if mode == "invalid budget" {
				f.budget = 0
			}
			f.run = func(rc RunCtx, w *writer.Writer) (result.Outcome, error) {
				if err := w.Pending(rc.Ctx, true); err != nil {
					return result.Outcome{}, err
				}
				switch mode {
				case "cancel":
					cancel()
					return result.OutcomeOf(result.Aborted), context.Canceled
				case "soft stop":
					return result.OutcomeOf(result.SoftStopped), writer.ErrSoftStop
				case "lost Lease":
					h.gate.Close()
				case "journal failure":
					return result.OutcomeOf(result.Aborted), journal.ErrIO
				}
				return result.OutcomeOf(result.Passed), nil
			}
			// Fail a durable append, not merely the callback's return value.
			if mode == "journal failure" {
				d.JournalFault = func(_ *os.File, b []byte) error {
					if bytes.Contains(b, []byte("pendingWork")) {
						return errors.New("sync failed")
					}
					return nil
				}
			}
			f.cleanup = func(ctx context.Context, w *writer.Writer) (json.RawMessage, error) {
				called = true
				if mode == "cleanup deadline" {
					return nil, context.DeadlineExceeded
				}
				if mode == "cleanup Lease loss" {
					h.gate.Close()
				}
				if mode == "cleanup failure" {
					return nil, errors.New("unsettled")
				}
				_, err := writer.Send(ctx, w, writer.Call{WriteCall: journal.WriteCall{Scope: "firmware", Kind: "firmware-window-close", Key: "close"}, Retry: writer.RetryPolicy{MaxAttempts: 1, AttemptTimeout: time.Second}}, func(context.Context) writer.Attempt { sent = true; return writer.Attempt{} })
				if err == nil {
					err = w.Pending(ctx, false)
				}
				return json.RawMessage(`{"status":"closed"}`), err
			}
			got := Execute(d, f)
			wantCleanup := mode != "lost Lease" && mode != "journal failure" && mode != "invalid budget"
			if called != wantCleanup {
				t.Fatalf("cleanup=%v verdict=%s", called, got.Verdict)
			}
			if wantCleanup && mode != "cleanup failure" && mode != "cleanup deadline" && mode != "cleanup Lease loss" && !sent {
				t.Fatal("cleanup write was stopped or journal frozen")
			}
			if mode == "cleanup deadline" && (!bytes.Contains(got.Detail, []byte(`"reason":"deadline"`)) || got.Verdict != result.Unresolved) {
				t.Fatal("cleanup lost typed deadline", string(got.Detail))
			}
			if mode == "cleanup Lease loss" && (sent || got.Verdict != result.Aborted) {
				t.Fatal("cleanup dispatched after Lease loss", got.Verdict)
			}
			if mode == "normal" && got.Verdict != result.Passed {
				t.Fatalf("normal=%+v", got)
			}
			if mode == "cleanup failure" && got.Verdict != result.Unresolved {
				t.Fatalf("failure=%+v", got)
			}
		})
	}
}

func TestFirmwareFinalizerHangingJoin(t *testing.T) {
	marker := os.Getenv("FIRMWARE_JOIN_MARKER")
	if marker == "" {
		marker = filepath.Join(t.TempDir(), "cleanup-ran")
		ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
		defer cancel()
		cmd := exec.CommandContext(ctx, os.Args[0], "-test.run=^TestFirmwareFinalizerHangingJoin$")
		cmd.Env = append(os.Environ(), "FIRMWARE_JOIN_MARKER="+marker)
		err := cmd.Run()
		var exited *exec.ExitError
		if !errors.As(err, &exited) || exited.ExitCode() != result.ExitNotRun {
			t.Fatalf("watchdog=%v", err)
		}
		if _, err = os.Stat(marker); !os.IsNotExist(err) {
			t.Fatal("cleanup ran before the attempt joined")
		}
		return
	}
	seq := &sequence{}
	d := deps(t, seq, held(seq))
	d.ShutdownBudget = 30 * time.Millisecond
	f := firmwareTest{budget: time.Second}
	f.run = func(rc RunCtx, w *writer.Writer) (result.Outcome, error) {
		_, err := writer.Send(rc.Ctx, w, writer.Call{WriteCall: journal.WriteCall{Scope: "firmware", Kind: "firmware-window-open", Key: "open"}, Retry: writer.RetryPolicy{MaxAttempts: 1, AttemptTimeout: time.Millisecond}}, func(context.Context) writer.Attempt { select {} })
		return result.OutcomeOf(result.Unresolved), err
	}
	f.cleanup = func(context.Context, *writer.Writer) (json.RawMessage, error) {
		return nil, os.WriteFile(marker, nil, 0600)
	}
	Execute(d, f)
	t.Fatal("hanging attempt escaped the watchdog")
}

func TestFirmwareShutdownBudget(t *testing.T) {
	for _, budget := range []time.Duration{-1 << 63, 0, time.Second, time.Minute, time.Hour, 1<<63 - 1} {
		e := &execution{test: firmwareTest{budget: budget}}
		if got := e.shutdownBudget(); got < Drain+time.Second+CleanupBudget+30*time.Second || got > Drain+time.Hour+CleanupBudget+30*time.Second {
			t.Fatalf("unsafe pre-admission watchdog budget: %v", got)
		}
	}
}
