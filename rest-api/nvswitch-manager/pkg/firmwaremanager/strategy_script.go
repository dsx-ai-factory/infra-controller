// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package firmwaremanager

import (
	"context"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"syscall"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/pkg/firmwaremanager/packages"
	"github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/pkg/objects/nvswitch"

	log "github.com/sirupsen/logrus"
)

// Ensure ScriptStrategy implements UpdateStrategy.
var _ UpdateStrategy = (*ScriptStrategy)(nil)

// ScriptStrategy implements firmware updates via external shell scripts.
// Scripts are specified per-component in the firmware bundle YAML.
//
// Steps: INSTALL -> VERIFY
type ScriptStrategy struct {
	config       *packages.ScriptConfig
	firmwarePath string   // Set before each update
	scriptName   string   // Script name from component definition
	scriptArgs   []string // Argument tokens from YAML (e.g., ["nvos_ip", "nvos_user", "nvos_password", "fw_file"])
}

// NewScriptStrategy creates a new script update strategy.
func NewScriptStrategy(config *packages.ScriptConfig) *ScriptStrategy {
	if config == nil {
		config = &packages.ScriptConfig{
			ScriptDir:      "scripts",
			TimeoutSeconds: 3600, // 1 hour default
		}
	}
	if config.ScriptDir == "" {
		config.ScriptDir = "scripts"
	}
	if config.TimeoutSeconds == 0 {
		config.TimeoutSeconds = 3600
	}
	return &ScriptStrategy{config: config}
}

// SetFirmwarePath sets the path to the firmware file for the current update.
func (s *ScriptStrategy) SetFirmwarePath(path string) {
	s.firmwarePath = path
}

// SetScriptName sets the script name to execute (from component definition).
func (s *ScriptStrategy) SetScriptName(name string) {
	s.scriptName = name
}

// SetScriptArgs sets the argument tokens for the script.
func (s *ScriptStrategy) SetScriptArgs(tokens []string) {
	s.scriptArgs = tokens
}

// Name returns the strategy type.
func (s *ScriptStrategy) Name() Strategy {
	return StrategyScript
}

// Steps returns the ordered sequence of states for Script updates.
func (s *ScriptStrategy) Steps(update *FirmwareUpdate) []UpdateState {
	return []UpdateState{StateInstall, StateVerify}
}

// ExecuteStep performs the work for a single state.
func (s *ScriptStrategy) ExecuteStep(ctx context.Context, update *FirmwareUpdate, tray *nvswitch.NVSwitchTray) StepOutcome {
	switch update.State {
	case StateInstall:
		return s.executeInstall(ctx, update, tray)
	case StateVerify:
		return s.executeVerify(ctx, update, tray)
	default:
		return Failed(fmt.Errorf("unexpected state for Script strategy: %s", update.State))
	}
}

// executeInstall runs the external update script asynchronously.
// On first call, it starts the script and stores the PID in ExecContext.
// On subsequent calls, it polls the process status without blocking.
func (s *ScriptStrategy) executeInstall(ctx context.Context, update *FirmwareUpdate, tray *nvswitch.NVSwitchTray) StepOutcome {
	// Check if we're already tracking a running process
	if update.ExecContext != nil && update.ExecContext.PID != 0 {
		return s.pollScriptProcess(ctx, update)
	}

	// First call - start the script
	log.Infof("[%s] Starting update script for component %s", update.ID, update.Component)

	if s.firmwarePath == "" {
		return Failed(fmt.Errorf("firmware path not set"))
	}

	if s.scriptName == "" {
		return Failed(fmt.Errorf("script name not set (must be specified in bundle YAML)"))
	}

	// Resolve script path
	scriptPath := s.resolveScriptPath(s.scriptName)

	// Check script exists
	if _, err := os.Stat(scriptPath); os.IsNotExist(err) {
		return Failed(fmt.Errorf("script not found: %s", scriptPath))
	}

	// Build command arguments based on script type
	args := s.buildScriptArgs(update, tray)

	log.Infof("[%s] Running script: %s with %d args", update.ID, scriptPath, len(args))

	// Create command (without context timeout - we'll manage timeout via ExecContext)
	cmd := exec.Command(scriptPath, args...)
	cmd.Env = os.Environ()

	// Capture stdout/stderr to files for debugging
	logFile := fmt.Sprintf("/tmp/nsm-script-%s.log", update.ID)
	f, err := os.Create(logFile)
	if err != nil {
		log.Warnf("[%s] Failed to create log file %s: %v", update.ID, logFile, err)
		cmd.Stdout = nil
		cmd.Stderr = nil
	} else {
		cmd.Stdout = f
		cmd.Stderr = f
		log.Infof("[%s] Script output will be logged to %s", update.ID, logFile)
	}

	// Start the process asynchronously
	if err := cmd.Start(); err != nil {
		if f != nil {
			f.Close()
		}
		return Failed(fmt.Errorf("failed to start script: %w", err))
	}

	// Close our handle to the log file - the child process has its own fd
	// and will continue writing to it. This prevents file descriptor leaks.
	if f != nil {
		f.Close()
	}

	pid := cmd.Process.Pid
	timeout := time.Duration(s.config.TimeoutSeconds) * time.Second

	log.Infof("[%s] Script started with PID %d, timeout: %v", update.ID, pid, timeout)

	// Create exec context for async tracking
	execCtx := &ExecContext{
		StartedAt:  time.Now(),
		DeadlineAt: time.Now().Add(timeout),
		PID:        pid,
	}

	return Wait(execCtx)
}

// resolveScriptPath returns the full path to a script.
// If the script name is absolute, returns it as-is.
// Otherwise, joins it with the script directory.
func (s *ScriptStrategy) resolveScriptPath(scriptName string) string {
	if filepath.IsAbs(scriptName) {
		return scriptName
	}
	return filepath.Join(s.config.ScriptDir, scriptName)
}

// buildScriptArgs builds the command-line arguments for the script.
// Each token in scriptArgs is resolved to its runtime value.
//
// Valid tokens:
//   - bmc_ip, bmc_user, bmc_password
//   - nvos_ip, nvos_user, nvos_password
//   - fw_file
//
// If scriptArgs is empty, defaults based on component type.
func (s *ScriptStrategy) buildScriptArgs(update *FirmwareUpdate, tray *nvswitch.NVSwitchTray) []string {
	tokens := s.scriptArgs

	// Default tokens if not specified
	if len(tokens) == 0 {
		switch update.Component {
		case nvswitch.BMC, nvswitch.BIOS:
			tokens = []string{"bmc_ip", "bmc_user", "bmc_password", "fw_file"}
		case nvswitch.CPLD, nvswitch.NVOS:
			tokens = []string{"nvos_ip", "nvos_user", "nvos_password", "fw_file"}
		default:
			tokens = []string{"bmc_ip", "bmc_user", "bmc_password", "nvos_ip", "nvos_user", "nvos_password", "fw_file"}
		}
	}

	// Resolve each token to its value
	args := make([]string, 0, len(tokens))
	for _, token := range tokens {
		args = append(args, s.resolveToken(token, tray))
	}
	return args
}

// resolveToken converts a token name to its runtime value.
func (s *ScriptStrategy) resolveToken(token string, tray *nvswitch.NVSwitchTray) string {
	switch token {
	case "bmc_ip":
		return tray.BMC.IP.String()
	case "bmc_user":
		return tray.BMC.Credential.User
	case "bmc_password":
		return tray.BMC.Credential.Password.Value
	case "nvos_ip":
		return tray.NVOS.IP.String()
	case "nvos_user":
		return tray.NVOS.Credential.User
	case "nvos_password":
		return tray.NVOS.Credential.Password.Value
	case "fw_file":
		return s.firmwarePath
	default:
		// Shouldn't happen if validation passed
		return ""
	}
}

// pollScriptProcess checks the status of a running script process without blocking.
// Uses syscall.Wait4 with WNOHANG to poll process status.
func (s *ScriptStrategy) pollScriptProcess(ctx context.Context, update *FirmwareUpdate) StepOutcome {
	execCtx := update.ExecContext
	pid := execCtx.PID

	log.Debugf("[%s] Polling script process PID %d", update.ID, pid)

	// Check for timeout
	if time.Now().After(execCtx.DeadlineAt) {
		// Try to kill the process
		if proc, err := os.FindProcess(pid); err == nil {
			_ = proc.Kill()
		}
		return Failed(fmt.Errorf("script timed out after %v", time.Since(execCtx.StartedAt)))
	}

	// Use syscall.Wait4 with WNOHANG to check process status without blocking
	var wstatus syscall.WaitStatus
	var rusage syscall.Rusage

	wpid, err := syscall.Wait4(pid, &wstatus, syscall.WNOHANG, &rusage)
	if err != nil {
		// ECHILD means process doesn't exist or isn't our child
		if err == syscall.ECHILD {
			// Process may have been reaped by another goroutine or doesn't exist
			// Assume success if we can't find it (optimistic)
			log.Warnf("[%s] Process %d not found (ECHILD), assuming completion", update.ID, pid)
			return Transition(StateVerify)
		}
		return Failed(fmt.Errorf("failed to check process status: %w", err))
	}

	// wpid == 0 means process is still running (WNOHANG returned without waiting)
	if wpid == 0 {
		log.Debugf("[%s] Script process %d still running", update.ID, pid)
		return Wait(execCtx)
	}

	// Process has exited - check exit status
	if wstatus.Exited() {
		exitCode := wstatus.ExitStatus()
		if exitCode == 0 {
			log.Infof("[%s] Script process %d completed successfully", update.ID, pid)
			return Transition(StateVerify)
		}
		return Failed(fmt.Errorf("script exited with code %d", exitCode))
	}

	// Process was signaled
	if wstatus.Signaled() {
		sig := wstatus.Signal()
		return Failed(fmt.Errorf("script terminated by signal: %s", sig))
	}

	// Unknown status - shouldn't happen
	log.Warnf("[%s] Unknown process status for PID %d: %v", update.ID, pid, wstatus)
	return Wait(execCtx)
}

// executeVerify verifies the firmware version after script execution.
func (s *ScriptStrategy) executeVerify(ctx context.Context, update *FirmwareUpdate, tray *nvswitch.NVSwitchTray) StepOutcome {
	log.Infof("[%s] Verifying firmware version", update.ID)

	actualVersion, err := s.GetCurrentVersion(ctx, tray, update.Component)
	if err != nil {
		// Non-fatal for script strategy - scripts may have their own verification
		log.Warnf("[%s] Failed to verify version: %v", update.ID, err)
		return Transition(StateCompleted)
	}

	update.VersionActual = actualVersion

	if actualVersion != update.VersionTo {
		log.Warnf("[%s] Version mismatch: expected %s, got %s", update.ID, update.VersionTo, actualVersion)
	} else {
		log.Infof("[%s] Firmware version verified: %s", update.ID, actualVersion)
	}

	return Transition(StateCompleted)
}

// GetCurrentVersion queries the current firmware version.
// For script strategy, we delegate to the appropriate method based on component.
func (s *ScriptStrategy) GetCurrentVersion(ctx context.Context, tray *nvswitch.NVSwitchTray, component nvswitch.Component) (string, error) {
	// Script strategy can use either Redfish or SSH depending on component
	switch component {
	case nvswitch.BMC, nvswitch.BIOS:
		// Use Redfish for BMC and BIOS firmware
		redfishStrategy := NewRedfishStrategy(nil)
		return redfishStrategy.GetCurrentVersion(ctx, tray, component)
	case nvswitch.CPLD, nvswitch.NVOS:
		// Use SSH for CPLD and NVOS
		sshStrategy := NewSSHStrategy(nil)
		return sshStrategy.GetCurrentVersion(ctx, tray, component)
	default:
		return "", fmt.Errorf("unsupported component: %s", component)
	}
}
