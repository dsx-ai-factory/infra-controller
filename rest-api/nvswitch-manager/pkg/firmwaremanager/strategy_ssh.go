// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package firmwaremanager

import (
	"context"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"strings"
	"syscall"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/pkg/firmwaremanager/packages"
	"github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/pkg/objects/nvswitch"
	"github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/pkg/sshclient"

	log "github.com/sirupsen/logrus"
)

// safePathRe matches paths containing only shell-safe characters
// (alphanumerics, slashes, hyphens, underscores, and dots).
var safePathRe = regexp.MustCompile(`^[a-zA-Z0-9/_.\-]+$`)

// Ensure SSHStrategy implements UpdateStrategy.
var _ UpdateStrategy = (*SSHStrategy)(nil)

// SSHStrategy implements firmware updates via SSH.
// Used for CPLD and NVOS firmware updates.
//
// CPLD flow: WAIT_REACHABLE -> COPY -> UPLOAD -> INSTALL -> VERIFY -> CLEANUP
// NVOS flow (bundle): POWER_CYCLE -> WAIT_REACHABLE -> COPY -> UPLOAD -> INSTALL -> VERIFY -> CLEANUP
// NVOS flow (standalone): WAIT_REACHABLE -> COPY -> UPLOAD -> INSTALL -> VERIFY -> CLEANUP
type SSHStrategy struct {
	config       *packages.SSHConfig
	firmwarePath string // Set before each update
}

// NewSSHStrategy creates a new SSH update strategy.
func NewSSHStrategy(config *packages.SSHConfig) *SSHStrategy {
	if config == nil {
		config = &packages.SSHConfig{
			RemoteDir:            "/home/admin",
			RebootTimeoutSeconds: 600, // 10 minutes default
		}
	}
	if config.RemoteDir == "" {
		config.RemoteDir = "/home/admin"
	}
	if config.RebootTimeoutSeconds == 0 {
		config.RebootTimeoutSeconds = 600
	}
	return &SSHStrategy{config: config}
}

// isSshpassAvailable checks if sshpass is installed and available in PATH.
func isSshpassAvailable() bool {
	_, err := exec.LookPath("sshpass")
	return err == nil
}

// SetFirmwarePath sets the path to the firmware file for the current update.
func (s *SSHStrategy) SetFirmwarePath(path string) {
	s.firmwarePath = path
}

// Name returns the strategy type.
func (s *SSHStrategy) Name() Strategy {
	return StrategySSH
}

// Steps returns the ordered sequence of states for SSH updates.
// The steps vary based on component and whether this is a bundle update.
func (s *SSHStrategy) Steps(update *FirmwareUpdate) []UpdateState {
	switch update.Component {
	case nvswitch.NVOS:
		// NVOS updates need power cycle if part of a bundle with other components
		if update.BundleUpdateID != nil && update.SequenceOrder > 1 {
			// Bundle update - power cycle first
			return []UpdateState{
				StatePowerCycle,
				StateWaitReachable,
				StateCopy,
				StateUpload,
				StateInstall,
				StateVerify,
				StateCleanup,
			}
		}
		// Standalone NVOS update - just check reachability first
		return []UpdateState{
			StateWaitReachable,
			StateCopy,
			StateUpload,
			StateInstall,
			StateVerify,
			StateCleanup,
		}

	case nvswitch.CPLD:
		// CPLD updates: check reachability, then standard flow
		return []UpdateState{
			StateWaitReachable,
			StateCopy,
			StateUpload,
			StateInstall,
			StateVerify,
			StateCleanup,
		}

	default:
		// Default SSH flow
		return []UpdateState{StateCopy, StateUpload, StateInstall, StateVerify, StateCleanup}
	}
}

// ExecuteStep performs the work for a single state.
func (s *SSHStrategy) ExecuteStep(ctx context.Context, update *FirmwareUpdate, tray *nvswitch.NVSwitchTray) StepOutcome {
	switch update.State {
	case StatePowerCycle:
		return s.executePowerCycle(ctx, update, tray)
	case StateWaitReachable:
		return s.executeWaitReachable(ctx, update, tray)
	case StateCopy:
		return s.executeCopy(ctx, update, tray)
	case StateUpload:
		return s.executeUpload(ctx, update, tray)
	case StateInstall:
		return s.executeInstall(ctx, update, tray)
	case StateVerify:
		return s.executeVerify(ctx, update, tray)
	case StateCleanup:
		return s.executeCleanup(ctx, update, tray)
	default:
		return Failed(fmt.Errorf("unexpected state for SSH strategy: %s", update.State))
	}
}

// executePowerCycle performs a power cycle on the tray via BMC Redfish.
// This is only called for NVOS updates that are part of a bundle.
// This is async-aware to handle the shutdown delay without blocking.
func (s *SSHStrategy) executePowerCycle(ctx context.Context, update *FirmwareUpdate, tray *nvswitch.NVSwitchTray) StepOutcome {
	log.Infof("[%s] Power cycling NV-Switch tray before NVOS update", update.ID)

	// Check if we're in the waiting-for-shutdown phase
	execCtx := update.ExecContext
	if execCtx != nil {
		// Already initiated power cycle, check if wait period is done
		if time.Now().After(execCtx.DeadlineAt) {
			log.Debugf("[%s] Shutdown wait period complete, proceeding to reachability check", update.ID)
			return Transition(StateWaitReachable)
		}
		// Still waiting for switch to go down
		return Wait(execCtx)
	}

	// First call - initiate power cycle
	if err := PowerCycleTray(ctx, tray); err != nil {
		return Failed(fmt.Errorf("power cycle failed: %w", err))
	}

	log.Infof("[%s] Power cycle initiated, waiting for switch to go down", update.ID)

	// Set up wait for switch to start shutting down (10 seconds)
	execCtx = &ExecContext{
		StartedAt:  time.Now(),
		DeadlineAt: time.Now().Add(10 * time.Second),
	}
	return Wait(execCtx)
}

// executeWaitReachable waits for the NVOS to become reachable.
// This is async-aware: returns Wait while unreachable, Transition when reachable.
// After becoming reachable, waits for PostReachabilityDelay to allow services to start.
func (s *SSHStrategy) executeWaitReachable(ctx context.Context, update *FirmwareUpdate, tray *nvswitch.NVSwitchTray) StepOutcome {
	// Initialize or retrieve exec context
	execCtx := update.ExecContext
	if execCtx == nil {
		timeout := time.Duration(s.config.RebootTimeoutSeconds) * time.Second
		execCtx = &ExecContext{
			StartedAt:  time.Now(),
			DeadlineAt: time.Now().Add(timeout),
			TargetIP:   tray.NVOS.IP.String(),
		}
		log.Infof("[%s] Waiting for NVOS at %s to become reachable (timeout: %v)", update.ID, tray.NVOS.IP, timeout)
	}

	// Check for timeout
	if time.Now().After(execCtx.DeadlineAt) {
		return Failed(fmt.Errorf("switch not reachable after timeout"))
	}

	// Check if we're in the post-reachability delay phase
	if execCtx.BecameUnreachableAt != nil {
		// We're using BecameUnreachableAt to track when we became reachable (repurposing the field)
		// Actually, let's track it differently - use WaitingForReboot to indicate we're in delay phase
		if execCtx.WaitingForReboot {
			delayDone := execCtx.BecameUnreachableAt.Add(PostReachabilityDelay)
			if time.Now().After(delayDone) {
				log.Infof("[%s] Post-reachability delay complete, proceeding with update", update.ID)
				return Transition(StateCopy)
			}
			remaining := time.Until(delayDone)
			log.Debugf("[%s] Waiting for services to start (%v remaining)", update.ID, remaining.Round(time.Second))
			return Wait(execCtx)
		}
	}

	// Check reachability (non-blocking single check using configured SSH port)
	if IsReachable(tray.NVOS.IP.String(), tray.NVOS.GetPort()) {
		log.Infof("[%s] NVOS is reachable on port %d, waiting %v for services to start", update.ID, tray.NVOS.GetPort(), PostReachabilityDelay)
		// Mark that we've become reachable and start delay timer
		now := time.Now()
		execCtx.BecameUnreachableAt = &now // Repurposing this field to store "became reachable at"
		execCtx.WaitingForReboot = true    // Indicates we're in the delay phase
		return Wait(execCtx)
	}

	// Not reachable yet - wait and poll again
	log.Debugf("[%s] NVOS at %s not yet reachable, will retry", update.ID, tray.NVOS.IP)
	return Wait(execCtx)
}

// executeCopy copies the firmware file to the switch via SCP.
// This is async-aware: starts SCP in background on first call, polls for completion on subsequent calls.
func (s *SSHStrategy) executeCopy(ctx context.Context, update *FirmwareUpdate, tray *nvswitch.NVSwitchTray) StepOutcome {
	// Check if we're in the polling phase (SCP already started)
	execCtx := update.ExecContext
	if execCtx != nil && execCtx.PID > 0 {
		return s.handleSCPWait(ctx, update, tray, execCtx)
	}

	// First call - start SCP process asynchronously
	log.Infof("[%s] Copying firmware to switch via SCP: %s", update.ID, s.firmwarePath)

	if s.firmwarePath == "" {
		return Failed(fmt.Errorf("firmware path not set"))
	}

	// Verify firmware file exists
	if _, err := os.Stat(s.firmwarePath); err != nil {
		return Failed(fmt.Errorf("firmware file not found: %w", err))
	}

	// Check sshpass is available for async SCP
	if !isSshpassAvailable() {
		return Failed(fmt.Errorf("sshpass not found in PATH: required for async SCP operations"))
	}

	// Build SCP command
	fileName := filepath.Base(s.firmwarePath)
	remotePath := filepath.Join(s.config.RemoteDir, fileName)
	targetAddr := sshclient.SCPDestination(
		tray.NVOS.Credential.User,
		tray.NVOS.IP.String(),
		remotePath,
	)

	// Get the port from NVOS config (uses custom port if set, otherwise default 22)
	port := fmt.Sprintf("%d", tray.NVOS.GetPort())

	cmd := exec.Command("sshpass", "-e",
		"scp",
		"-P", port,
		"-o", "StrictHostKeyChecking=no",
		"-o", "UserKnownHostsFile=/dev/null",
		"-o", "ConnectTimeout=30",
		s.firmwarePath,
		targetAddr,
	)
	cmd.Env = append(os.Environ(), "SSHPASS="+tray.NVOS.Credential.Password.Value)

	if err := cmd.Start(); err != nil {
		return Failed(fmt.Errorf("failed to start SCP: %w", err))
	}

	pid := cmd.Process.Pid
	log.Infof("[%s] SCP started with PID %d, copying to %s", update.ID, pid, targetAddr)

	// Set up exec context for async polling
	// SCP timeout: allow up to 30 minutes for large firmware files
	scpTimeout := 30 * time.Minute
	execCtx = &ExecContext{
		StartedAt:  time.Now(),
		DeadlineAt: time.Now().Add(scpTimeout),
		PID:        pid,
		TargetIP:   tray.NVOS.IP.String(),
	}

	return Wait(execCtx)
}

// handleSCPWait checks if the async SCP process has completed.
func (s *SSHStrategy) handleSCPWait(ctx context.Context, update *FirmwareUpdate, tray *nvswitch.NVSwitchTray, execCtx *ExecContext) StepOutcome {
	// Check for timeout
	if time.Now().After(execCtx.DeadlineAt) {
		// Try to kill the SCP process
		if proc, err := os.FindProcess(execCtx.PID); err == nil {
			_ = proc.Kill()
		}
		return Failed(fmt.Errorf("SCP timeout after %v", time.Since(execCtx.StartedAt)))
	}

	// Check if process has completed using Wait4 with WNOHANG (non-blocking)
	var ws syscall.WaitStatus
	pid, err := syscall.Wait4(execCtx.PID, &ws, syscall.WNOHANG, nil)
	if err != nil {
		// ECHILD means the process has already been reaped or doesn't exist
		if err == syscall.ECHILD {
			// Process already completed and was reaped, assume success
			// Verify file was actually copied
			return s.verifySCPComplete(ctx, update, tray)
		}
		return Failed(fmt.Errorf("failed to check SCP status: %w", err))
	}

	if pid == 0 {
		// Process is still running
		log.Debugf("[%s] SCP still in progress (PID %d)", update.ID, execCtx.PID)
		return Wait(execCtx)
	}

	// Process has completed
	if ws.Exited() {
		exitCode := ws.ExitStatus()
		if exitCode == 0 {
			log.Infof("[%s] SCP completed successfully", update.ID)
			return s.verifySCPComplete(ctx, update, tray)
		}
		return Failed(fmt.Errorf("SCP failed with exit code %d", exitCode))
	}

	if ws.Signaled() {
		return Failed(fmt.Errorf("SCP terminated by signal %d", ws.Signal()))
	}

	// Unexpected status
	return Failed(fmt.Errorf("SCP ended with unexpected status: %v", ws))
}

// verifySCPComplete verifies the file was copied successfully.
func (s *SSHStrategy) verifySCPComplete(ctx context.Context, update *FirmwareUpdate, tray *nvswitch.NVSwitchTray) StepOutcome {
	client, err := sshclient.New(ctx, tray.NVOS)
	if err != nil {
		return Failed(fmt.Errorf("failed to create SSH client for verification: %w", err))
	}
	defer client.Close()

	fileName := filepath.Base(s.firmwarePath)
	remotePath := filepath.Join(s.config.RemoteDir, fileName)

	if !safePathRe.MatchString(remotePath) {
		return Failed(fmt.Errorf("remote path contains invalid characters: %s", remotePath))
	}

	output, err := client.RunCommand(fmt.Sprintf("ls -l %s", remotePath))
	if err != nil {
		return Failed(fmt.Errorf("failed to verify file copy: %w", err))
	}
	log.Infof("[%s] File copied: %s", update.ID, strings.TrimSpace(output))

	return Transition(StateUpload)
}

// executeUpload uses "nv action fetch" to import the firmware into NVOS.
func (s *SSHStrategy) executeUpload(ctx context.Context, update *FirmwareUpdate, tray *nvswitch.NVSwitchTray) StepOutcome {
	log.Infof("[%s] Fetching firmware into NVOS", update.ID)

	client, err := sshclient.New(ctx, tray.NVOS)
	if err != nil {
		return Failed(fmt.Errorf("failed to create SSH client: %w", err))
	}
	defer client.Close()

	fileName := filepath.Base(s.firmwarePath)
	remotePath := filepath.Join(s.config.RemoteDir, fileName)

	if !safePathRe.MatchString(remotePath) {
		return Failed(fmt.Errorf("remote path contains invalid characters: %s", remotePath))
	}

	var fetchCmd string
	switch update.Component {
	case nvswitch.CPLD:
		fetchCmd = fmt.Sprintf("nv action fetch platform firmware CPLD1 file://%s", remotePath)
	case nvswitch.NVOS:
		fetchCmd = fmt.Sprintf("nv action fetch system image file://%s", remotePath)
	default:
		return Failed(fmt.Errorf("unsupported component for SSH upload: %s", update.Component))
	}

	output, err := client.RunCommand(fetchCmd)
	if err != nil {
		return Failed(fmt.Errorf("failed to fetch firmware: %w (output: %s)", err, output))
	}

	log.Debugf("[%s] Fetch output: %s", update.ID, strings.TrimSpace(output))
	return Transition(StateInstall)
}

// executeInstall uses "nv action install" to install the firmware.
// This is async-aware to handle NVOS reboot without blocking.
func (s *SSHStrategy) executeInstall(ctx context.Context, update *FirmwareUpdate, tray *nvswitch.NVSwitchTray) StepOutcome {
	// Check if we're in the reboot-waiting phase
	execCtx := update.ExecContext
	if execCtx != nil && execCtx.WaitingForReboot {
		return s.handleRebootWait(ctx, update, tray, execCtx)
	}

	log.Infof("[%s] Installing firmware", update.ID)

	client, err := sshclient.New(ctx, tray.NVOS)
	if err != nil {
		return Failed(fmt.Errorf("failed to create SSH client: %w", err))
	}
	defer client.Close()

	fileName := filepath.Base(s.firmwarePath)

	if !safePathRe.MatchString(fileName) {
		return Failed(fmt.Errorf("firmware filename contains invalid characters: %s", fileName))
	}

	var installCmd string
	switch update.Component {
	case nvswitch.CPLD:
		installCmd = fmt.Sprintf("nv action install platform firmware CPLD1 files \"%s\"", fileName)
	case nvswitch.NVOS:
		installCmd = fmt.Sprintf("nv action install system image files \"%s\" force", fileName)
	default:
		return Failed(fmt.Errorf("unsupported component for SSH install: %s", update.Component))
	}

	output, err := client.RunCommand(installCmd)
	if err != nil {
		// For NVOS, the install triggers a reboot which closes the SSH connection
		if update.Component == nvswitch.NVOS && (strings.Contains(err.Error(), "EOF") || strings.Contains(err.Error(), "connection reset")) {
			log.Infof("[%s] NVOS install initiated, waiting for reboot", update.ID)

			// Set up async reboot wait
			downTimeout := 2 * time.Minute
			upTimeout := time.Duration(s.config.RebootTimeoutSeconds) * time.Second
			execCtx = &ExecContext{
				StartedAt:        time.Now(),
				DeadlineAt:       time.Now().Add(downTimeout + upTimeout),
				TargetIP:         tray.NVOS.IP.String(),
				WaitingForReboot: true,
			}
			return Wait(execCtx)
		}
		return Failed(fmt.Errorf("failed to install firmware: %w (output: %s)", err, output))
	}

	log.Infof("[%s] Install output: %s", update.ID, strings.TrimSpace(output))
	return Transition(StateVerify)
}

// handleRebootWait handles the async wait for NVOS reboot completion.
func (s *SSHStrategy) handleRebootWait(ctx context.Context, update *FirmwareUpdate, tray *nvswitch.NVSwitchTray, execCtx *ExecContext) StepOutcome {
	// Check for timeout
	if time.Now().After(execCtx.DeadlineAt) {
		return Failed(fmt.Errorf("timeout waiting for NVOS reboot"))
	}

	// Check if switch went down (became unreachable)
	nvosPort := tray.NVOS.GetPort()
	if execCtx.BecameUnreachableAt == nil {
		if !IsReachable(execCtx.TargetIP, nvosPort) {
			now := time.Now()
			execCtx.BecameUnreachableAt = &now
			log.Debugf("[%s] Switch became unreachable, waiting for it to come back", update.ID)
		}
		return Wait(execCtx)
	}

	// Switch was unreachable, check if it's back
	if IsReachable(execCtx.TargetIP, nvosPort) {
		log.Infof("[%s] Switch is back after reboot", update.ID)
		return Transition(StateVerify)
	}

	// Still waiting for switch to come back
	log.Debugf("[%s] Still waiting for switch to come back after reboot", update.ID)
	return Wait(execCtx)
}

// executeVerify verifies the firmware version after installation.
func (s *SSHStrategy) executeVerify(ctx context.Context, update *FirmwareUpdate, tray *nvswitch.NVSwitchTray) StepOutcome {
	log.Infof("[%s] Verifying firmware version", update.ID)

	actualVersion, err := s.GetCurrentVersion(ctx, tray, update.Component)
	if err != nil {
		return Failed(fmt.Errorf("failed to get current version: %w", err))
	}

	update.VersionActual = actualVersion

	if actualVersion != update.VersionTo {
		log.Warnf("[%s] Version mismatch: expected %s, got %s", update.ID, update.VersionTo, actualVersion)
	} else {
		log.Infof("[%s] Firmware version verified: %s", update.ID, actualVersion)
	}

	return Transition(StateCleanup)
}

// executeCleanup removes fetched firmware files from the switch.
func (s *SSHStrategy) executeCleanup(ctx context.Context, update *FirmwareUpdate, tray *nvswitch.NVSwitchTray) StepOutcome {
	log.Infof("[%s] Cleaning up temporary files", update.ID)

	client, err := sshclient.New(ctx, tray.NVOS)
	if err != nil {
		// Non-fatal: cleanup is best-effort
		log.Warnf("[%s] Failed to create SSH client for cleanup: %v", update.ID, err)
		return Transition(StateCompleted)
	}
	defer client.Close()

	fileName := filepath.Base(s.firmwarePath)
	remotePath := filepath.Join(s.config.RemoteDir, fileName)

	if !safePathRe.MatchString(remotePath) {
		// Non-fatal: cleanup is best-effort
		log.Warnf("[%s] Remote path contains invalid characters, skipping cleanup: %s", update.ID, remotePath)
		return Transition(StateCompleted)
	}

	// Delete the copied file
	if _, err := client.RunCommand(fmt.Sprintf("rm -f %s", remotePath)); err != nil {
		log.Warnf("[%s] Failed to delete copied file: %v", update.ID, err)
	}

	// Component-specific cleanup
	switch update.Component {
	case nvswitch.CPLD:
		// Delete the fetched firmware from NVOS store
		deleteCmd := fmt.Sprintf("nv action delete platform firmware CPLD1 files \"%s\"", fileName)
		if _, err := client.RunCommand(deleteCmd); err != nil {
			log.Warnf("[%s] Failed to delete fetched CPLD firmware: %v", update.ID, err)
		}

	case nvswitch.NVOS:
		// Uninstall old NVOS image
		if _, err := client.RunCommand("nv action uninstall system image force"); err != nil {
			log.Warnf("[%s] Failed to uninstall old NVOS image: %v", update.ID, err)
		}
	}

	return Transition(StateCompleted)
}

// GetCurrentVersion queries the current firmware version via SSH.
func (s *SSHStrategy) GetCurrentVersion(ctx context.Context, tray *nvswitch.NVSwitchTray, component nvswitch.Component) (string, error) {
	client, err := sshclient.New(ctx, tray.NVOS)
	if err != nil {
		return "", fmt.Errorf("failed to create SSH client: %w", err)
	}
	defer client.Close()

	switch component {
	case nvswitch.CPLD:
		output, err := client.RunCommand("nv show platform firmware")
		if err != nil {
			return "", fmt.Errorf("failed to get CPLD version: %w", err)
		}
		// Parse CPLD version from output
		// Format varies, look for "CPLD1" line
		for _, line := range strings.Split(output, "\n") {
			if strings.Contains(line, "CPLD1") {
				fields := strings.Fields(line)
				if len(fields) >= 2 {
					return fields[len(fields)-1], nil
				}
			}
		}
		return strings.TrimSpace(output), nil

	case nvswitch.NVOS:
		output, err := client.RunCommand("nv show system version")
		if err != nil {
			return "", fmt.Errorf("failed to get NVOS version: %w", err)
		}
		// Parse version from output
		for _, line := range strings.Split(output, "\n") {
			if strings.Contains(line, "version") {
				fields := strings.Fields(line)
				if len(fields) >= 2 {
					return fields[1], nil
				}
			}
		}
		return strings.TrimSpace(output), nil

	default:
		return "", fmt.Errorf("component %s not supported via SSH", component)
	}
}
