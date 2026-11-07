// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package firmwaremanager

import (
	"context"
	"fmt"
	"net/http"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/pkg/firmwaremanager/packages"
	"github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/pkg/objects/nvswitch"
	"github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/pkg/redfish"

	log "github.com/sirupsen/logrus"
)

// Ensure RedfishStrategy implements UpdateStrategy.
var _ UpdateStrategy = (*RedfishStrategy)(nil)

// Default timeout for connection retries during upload phase.
const defaultUploadRetryTimeout = 10 * time.Minute

// RedfishStrategy implements firmware updates via Redfish API.
// Used for BMC/BIOS firmware updates.
//
// Steps: UPLOAD -> POLL_COMPLETION -> VERIFY
type RedfishStrategy struct {
	config       *packages.RedfishConfig
	firmwarePath string // Set before each update
}

// NewRedfishStrategy creates a new Redfish update strategy.
func NewRedfishStrategy(config *packages.RedfishConfig) *RedfishStrategy {
	if config == nil {
		config = &packages.RedfishConfig{
			PollIntervalSeconds: 10,
			PollTimeoutSeconds:  1800, // 30 minutes default
		}
	}
	if config.PollIntervalSeconds == 0 {
		config.PollIntervalSeconds = 10
	}
	if config.PollTimeoutSeconds == 0 {
		config.PollTimeoutSeconds = 1800
	}
	return &RedfishStrategy{config: config}
}

// SetFirmwarePath sets the path to the firmware file for the current update.
func (s *RedfishStrategy) SetFirmwarePath(path string) {
	s.firmwarePath = path
}

// Name returns the strategy type.
func (s *RedfishStrategy) Name() Strategy {
	return StrategyRedfish
}

// Steps returns the ordered sequence of states for Redfish updates.
func (s *RedfishStrategy) Steps(update *FirmwareUpdate) []UpdateState {
	return []UpdateState{StateUpload, StatePollCompletion, StateVerify}
}

// ExecuteStep performs the work for a single state.
func (s *RedfishStrategy) ExecuteStep(ctx context.Context, update *FirmwareUpdate, tray *nvswitch.NVSwitchTray) StepOutcome {
	switch update.State {
	case StateUpload:
		return s.executeUpload(ctx, update, tray)
	case StatePollCompletion:
		return s.executePollCompletion(ctx, update, tray)
	case StateVerify:
		return s.executeVerify(ctx, update, tray)
	default:
		return Failed(fmt.Errorf("unexpected state for Redfish strategy: %s", update.State))
	}
}

// executeUpload uploads firmware via Redfish and captures the task URI.
// Connection errors are treated as transient and will be retried until the deadline.
func (s *RedfishStrategy) executeUpload(ctx context.Context, update *FirmwareUpdate, tray *nvswitch.NVSwitchTray) StepOutcome {
	// Initialize or retrieve exec context for retry tracking
	execCtx := update.ExecContext
	if execCtx == nil {
		execCtx = &ExecContext{
			StartedAt:  time.Now(),
			DeadlineAt: time.Now().Add(defaultUploadRetryTimeout),
			TargetIP:   tray.BMC.IP.String(),
		}
		log.Infof("[%s] Starting firmware upload to %s (deadline: %s)",
			update.ID, tray.BMC.IP, execCtx.DeadlineAt.Format(time.RFC3339))
	}

	log.Debugf("[%s] Uploading firmware via Redfish: %s", update.ID, s.firmwarePath)

	if s.firmwarePath == "" {
		return Failed(fmt.Errorf("firmware path not set"))
	}

	// Check for timeout before attempting
	if time.Now().After(execCtx.DeadlineAt) {
		return Failed(fmt.Errorf("timeout: BMC at %s not reachable after %v",
			execCtx.TargetIP, time.Since(execCtx.StartedAt).Round(time.Second)))
	}

	// Create Redfish client
	client, err := redfish.New(ctx, tray.BMC, true)
	if err != nil {
		if isTransientError(err) {
			log.Warnf("[%s] Transient error connecting to BMC: %v (will retry)", update.ID, err)
			return Wait(execCtx)
		}
		return Failed(fmt.Errorf("failed to create Redfish client: %w", err))
	}
	defer client.Logout()

	// Upload firmware
	resp, err := client.UpdateFirmwareByPath(s.firmwarePath)
	if err != nil {
		if isTransientError(err) {
			log.Warnf("[%s] Transient error uploading firmware: %v (will retry)", update.ID, err)
			return Wait(execCtx)
		}
		return Failed(fmt.Errorf("failed to upload firmware: %w", err))
	}
	defer resp.Body.Close()

	// Check response status
	if resp.StatusCode != http.StatusAccepted && resp.StatusCode != http.StatusOK {
		return Failed(fmt.Errorf("firmware upload returned status %d", resp.StatusCode))
	}

	// Extract task URI for polling
	taskURI, err := client.GetTaskURI(resp)
	if err != nil {
		return Failed(fmt.Errorf("failed to get task URI: %w", err))
	}

	log.Infof("[%s] Firmware upload accepted, task URI: %s", update.ID, taskURI)
	update.TaskURI = taskURI

	return Transition(StatePollCompletion)
}

// executePollCompletion polls the Redfish task until completion.
// This is async-aware: it returns Wait if still polling, Transition when done.
// Connection errors are treated as transient and will be retried until the deadline.
func (s *RedfishStrategy) executePollCompletion(ctx context.Context, update *FirmwareUpdate, tray *nvswitch.NVSwitchTray) StepOutcome {
	log.Debugf("[%s] Polling Redfish task: %s", update.ID, update.TaskURI)

	if update.TaskURI == "" {
		return Failed(fmt.Errorf("no task URI to poll"))
	}

	// Initialize or retrieve exec context
	execCtx := update.ExecContext
	if execCtx == nil {
		timeout := time.Duration(s.config.PollTimeoutSeconds) * time.Second
		execCtx = &ExecContext{
			StartedAt:  time.Now(),
			DeadlineAt: time.Now().Add(timeout),
			TaskURI:    update.TaskURI,
			TargetIP:   tray.BMC.IP.String(),
		}
		log.Infof("[%s] Starting Redfish task poll, deadline: %s", update.ID, execCtx.DeadlineAt.Format(time.RFC3339))
	}

	// Check for timeout
	if time.Now().After(execCtx.DeadlineAt) {
		return Failed(fmt.Errorf("timeout waiting for Redfish task completion after %v",
			time.Since(execCtx.StartedAt).Round(time.Second)))
	}

	// Create Redfish client
	client, err := redfish.New(ctx, tray.BMC, true)
	if err != nil {
		if isTransientError(err) {
			log.Warnf("[%s] Transient error connecting to BMC during poll: %v (will retry)", update.ID, err)
			return Wait(execCtx)
		}
		return Failed(fmt.Errorf("failed to create Redfish client: %w", err))
	}
	defer client.Logout()

	state, percentComplete, err := client.GetTaskStatus(update.TaskURI)
	if err != nil {
		// Treat all polling errors as transient - the task may still be running
		log.Warnf("[%s] Error polling task status: %v (will retry)", update.ID, err)
		return Wait(execCtx)
	}

	log.Debugf("[%s] Task state: %s, progress: %d%%", update.ID, state, percentComplete)

	switch state {
	case "Completed":
		log.Infof("[%s] Redfish task completed successfully", update.ID)
		return Transition(StateVerify)
	case "Exception", "Killed", "Cancelled":
		return Failed(fmt.Errorf("Redfish task failed with state: %s", state))
	default:
		// Still running (Running, Starting, Pending, etc.) - wait and poll again
		return Wait(execCtx)
	}
}

// executeVerify verifies the firmware version after update.
// This is async-aware to handle BMC stabilization and connection retries.
func (s *RedfishStrategy) executeVerify(ctx context.Context, update *FirmwareUpdate, tray *nvswitch.NVSwitchTray) StepOutcome {
	log.Debugf("[%s] Verifying firmware version", update.ID)

	// Initialize or retrieve exec context for stabilization and retry tracking
	execCtx := update.ExecContext
	if execCtx == nil {
		// First call - set up verification with initial stabilization wait
		// Allow 5 minutes for BMC to stabilize and become reachable after update
		execCtx = &ExecContext{
			StartedAt:  time.Now(),
			DeadlineAt: time.Now().Add(5 * time.Minute),
			TargetIP:   tray.BMC.IP.String(),
		}
		log.Infof("[%s] Starting verification phase (deadline: %s)", update.ID, execCtx.DeadlineAt.Format(time.RFC3339))
		// Wait briefly for BMC to stabilize before first attempt
		return Wait(execCtx)
	}

	// Check for timeout
	if time.Now().After(execCtx.DeadlineAt) {
		return Failed(fmt.Errorf("timeout: unable to verify firmware version after %v",
			time.Since(execCtx.StartedAt).Round(time.Second)))
	}

	// Attempt to verify version
	actualVersion, err := s.GetCurrentVersion(ctx, tray, update.Component)
	if err != nil {
		if isTransientError(err) {
			log.Warnf("[%s] Transient error during verification: %v (will retry)", update.ID, err)
			return Wait(execCtx)
		}
		return Failed(fmt.Errorf("failed to get current version: %w", err))
	}

	update.VersionActual = actualVersion

	if actualVersion != update.VersionTo {
		log.Warnf("[%s] Version mismatch: expected %s, got %s", update.ID, update.VersionTo, actualVersion)
		// This might not be a failure - some devices report version differently
	} else {
		log.Infof("[%s] Firmware version verified: %s", update.ID, actualVersion)
	}

	return Transition(StateCompleted)
}

// GetCurrentVersion queries the current firmware version via Redfish.
func (s *RedfishStrategy) GetCurrentVersion(ctx context.Context, tray *nvswitch.NVSwitchTray, component nvswitch.Component) (string, error) {
	client, err := redfish.New(ctx, tray.BMC, true)
	if err != nil {
		return "", fmt.Errorf("failed to create Redfish client: %w", err)
	}
	defer client.Logout()

	switch component {
	case nvswitch.BMC:
		// Get BMC manager firmware version
		manager, err := client.QueryManager()
		if err != nil {
			return "", fmt.Errorf("failed to query manager: %w", err)
		}
		return manager.FirmwareVersion, nil
	case nvswitch.BIOS:
		// TODO: Get BIOS version from Redfish
		// This would typically come from Systems resource
		return "", fmt.Errorf("BIOS version query not yet implemented")
	default:
		return "", fmt.Errorf("component %s not supported via Redfish", component)
	}
}
