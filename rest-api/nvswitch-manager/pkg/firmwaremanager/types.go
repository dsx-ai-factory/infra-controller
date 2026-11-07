// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package firmwaremanager provides firmware update orchestration for NV-Switch trays.
package firmwaremanager

import (
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/pkg/objects/nvswitch"

	"github.com/google/uuid"
)

// Strategy represents the method used to perform a firmware update.
type Strategy string

const (
	// StrategyScript uses external shell scripts for updates (legacy).
	StrategyScript Strategy = "script"
	// StrategySSH uses direct SSH commands for CPLD/NVOS updates.
	StrategySSH Strategy = "ssh"
	// StrategyRedfish uses Redfish API for BMC/BIOS firmware updates.
	StrategyRedfish Strategy = "redfish"
)

// IsValid returns true if the strategy is a known valid strategy.
func (s Strategy) IsValid() bool {
	switch s {
	case StrategyScript, StrategySSH, StrategyRedfish:
		return true
	default:
		return false
	}
}

// UpdateState represents the granular state of a firmware update operation.
type UpdateState string

const (
	// Common states
	StateQueued    UpdateState = "QUEUED"
	StateCompleted UpdateState = "COMPLETED"
	StateFailed    UpdateState = "FAILED"
	StateCancelled UpdateState = "CANCELLED" // Cancelled due to predecessor failure

	// Shared states (used by multiple strategies)
	StateInstall UpdateState = "INSTALL"
	StateVerify  UpdateState = "VERIFY"
	StateCleanup UpdateState = "CLEANUP"

	// NVOS-specific states (pre-update)
	StatePowerCycle    UpdateState = "POWER_CYCLE"    // Power cycle via BMC Redfish (bundle updates only)
	StateWaitReachable UpdateState = "WAIT_REACHABLE" // Wait for NVOS to be pingable

	// SSH-specific states
	StateCopy   UpdateState = "COPY"   // SCP file to switch
	StateUpload UpdateState = "UPLOAD" // nv action fetch

	// Redfish-specific states
	StatePollCompletion UpdateState = "POLL_COMPLETION" // Poll task until done
)

// IsTerminal returns true if the state is a terminal state.
func (s UpdateState) IsTerminal() bool {
	return s == StateCompleted || s == StateFailed || s == StateCancelled
}

// IsActive returns true if the state indicates active processing.
func (s UpdateState) IsActive() bool {
	return !s.IsTerminal() && s != StateQueued
}

// OutcomeType represents the type of outcome from executing a step.
type OutcomeType int

const (
	// OutcomeWait indicates the step is waiting for an async operation to complete.
	// The worker should save exec_context, update last_checked_at, and move on.
	OutcomeWait OutcomeType = iota
	// OutcomeTransition indicates the step completed and should transition to the next state.
	// The worker should clear exec_context and advance the state.
	OutcomeTransition
	// OutcomeFailed indicates the step failed with an error.
	// The worker should mark the update as FAILED and cancel dependent updates.
	OutcomeFailed
)

// String returns a human-readable representation of the outcome type.
func (o OutcomeType) String() string {
	switch o {
	case OutcomeWait:
		return "Wait"
	case OutcomeTransition:
		return "Transition"
	case OutcomeFailed:
		return "Failed"
	default:
		return "Unknown"
	}
}

// ExecContext holds async execution state that persists across poll intervals.
// This allows workers to resume monitoring long-running operations without blocking.
type ExecContext struct {
	// StartedAt is when the async operation began.
	StartedAt time.Time `json:"started_at"`

	// DeadlineAt is when the operation should timeout.
	DeadlineAt time.Time `json:"deadline_at"`

	// TaskURI is the Redfish task URI for polling (Redfish strategy).
	TaskURI string `json:"task_uri,omitempty"`

	// PID is the process ID for script/SCP operations (Script/SSH strategy).
	PID int `json:"pid,omitempty"`

	// TargetIP is the IP address being monitored for reachability checks.
	TargetIP string `json:"target_ip,omitempty"`

	// BecameUnreachableAt tracks when the target became unreachable (for reboot detection).
	BecameUnreachableAt *time.Time `json:"became_unreachable_at,omitempty"`

	// WaitingForReboot indicates we're waiting for the device to come back after reboot.
	WaitingForReboot bool `json:"waiting_for_reboot,omitempty"`
}

// StepOutcome represents the result of executing a single step in the update state machine.
// It uses an explicit outcome model to support async/non-blocking worker pools.
type StepOutcome struct {
	// Type indicates what kind of outcome this is.
	Type OutcomeType

	// NextState is the state to transition to (only valid for OutcomeTransition).
	NextState UpdateState

	// ExecContext holds async state to persist (only valid for OutcomeWait).
	ExecContext *ExecContext

	// Error is the error that caused failure (only valid for OutcomeFailed).
	Error error
}

// Wait creates a StepOutcome indicating the step is waiting for an async operation.
// The exec context will be persisted and the worker will poll again after the interval.
func Wait(ctx *ExecContext) StepOutcome {
	return StepOutcome{
		Type:        OutcomeWait,
		ExecContext: ctx,
	}
}

// Transition creates a StepOutcome indicating successful transition to the next state.
// The exec context will be cleared and the state machine will advance.
func Transition(nextState UpdateState) StepOutcome {
	return StepOutcome{
		Type:      OutcomeTransition,
		NextState: nextState,
	}
}

// Failed creates a StepOutcome indicating the step failed with an error.
// The update will be marked as FAILED and dependent updates will be cancelled.
func Failed(err error) StepOutcome {
	return StepOutcome{
		Type:  OutcomeFailed,
		Error: err,
	}
}

// FirmwareUpdate represents a firmware update operation tracked in the database.
type FirmwareUpdate struct {
	// Primary key - unique identifier for this update operation
	ID uuid.UUID `json:"id"`

	// Foreign key to nvswitch table
	SwitchUUID uuid.UUID `json:"switch_uuid"`

	// What is being updated
	Component     nvswitch.Component `json:"component"`      // FIRMWARE, CPLD, NVOS
	BundleVersion string             `json:"bundle_version"` // Version of the firmware bundle

	// How it's being updated
	Strategy Strategy `json:"strategy"` // script, ssh, redfish

	// Current state in the state machine
	State UpdateState `json:"state"`

	// Version tracking
	VersionFrom   string `json:"version_from"`   // Version before update
	VersionTo     string `json:"version_to"`     // Target version
	VersionActual string `json:"version_actual"` // Actual version after update (set during verify)

	// Redfish-specific: task URI for resume capability
	TaskURI string `json:"task_uri,omitempty"`

	// Error information
	ErrorMessage string `json:"error_message,omitempty"`

	// Sequencing fields for multi-component updates
	BundleUpdateID *uuid.UUID `json:"bundle_update_id,omitempty"` // Groups related updates
	SequenceOrder  int        `json:"sequence_order"`             // Order within bundle (1, 2, 3...)
	PredecessorID  *uuid.UUID `json:"predecessor_id,omitempty"`   // Must complete before this starts

	// Async worker pool fields
	ExecContext   *ExecContext `json:"exec_context,omitempty"`    // Persisted async execution state
	LastCheckedAt *time.Time   `json:"last_checked_at,omitempty"` // Last time worker polled this update

	// Timestamps
	CreatedAt time.Time `json:"created_at"`
	UpdatedAt time.Time `json:"updated_at"`
}

// NewFirmwareUpdate creates a new FirmwareUpdate in QUEUED state.
func NewFirmwareUpdate(
	switchUUID uuid.UUID,
	component nvswitch.Component,
	bundleVersion string,
	strategy Strategy,
	versionTo string,
) *FirmwareUpdate {
	now := time.Now()
	return &FirmwareUpdate{
		ID:            uuid.New(),
		SwitchUUID:    switchUUID,
		Component:     component,
		BundleVersion: bundleVersion,
		Strategy:      strategy,
		State:         StateQueued,
		VersionTo:     versionTo,
		CreatedAt:     now,
		UpdatedAt:     now,
	}
}

// WithSequencing sets the sequencing fields for multi-component updates.
func (fu *FirmwareUpdate) WithSequencing(bundleUpdateID *uuid.UUID, order int, predecessorID *uuid.UUID) *FirmwareUpdate {
	fu.BundleUpdateID = bundleUpdateID
	fu.SequenceOrder = order
	fu.PredecessorID = predecessorID
	return fu
}

// SetState updates the state and timestamp.
func (fu *FirmwareUpdate) SetState(state UpdateState) {
	fu.State = state
	fu.UpdatedAt = time.Now()
}

// SetError sets the error message and transitions to FAILED state.
func (fu *FirmwareUpdate) SetError(err error) {
	fu.ErrorMessage = err.Error()
	fu.SetState(StateFailed)
}

// GetFirstState returns the initial execution state for a given firmware update.
// For SSH strategy, the first state depends on the component and whether it's a bundle update.
func GetFirstState(update *FirmwareUpdate) UpdateState {
	switch update.Strategy {
	case StrategyRedfish:
		return StateUpload

	case StrategySSH:
		// For NVOS, the first state depends on bundle context
		if update.Component == nvswitch.NVOS {
			if update.BundleUpdateID != nil && update.SequenceOrder > 1 {
				// Bundle update with predecessors - power cycle first
				return StatePowerCycle
			}
			// Standalone NVOS update - check reachability first
			return StateWaitReachable
		}
		// For CPLD, check reachability first
		if update.Component == nvswitch.CPLD {
			return StateWaitReachable
		}
		// Default SSH flow
		return StateCopy

	case StrategyScript:
		return StateInstall

	default:
		return StateInstall
	}
}
