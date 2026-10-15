// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package cmd

import (
	"context"
	"embed"
	"encoding/json"
	"fmt"
	"html/template"
	"net"
	"net/http"
	"os"
	"sort"
	"strconv"
	"strings"
	"time"

	pb "github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/internal/proto/v1"

	log "github.com/sirupsen/logrus"
	"github.com/spf13/cobra"
	"google.golang.org/grpc"
	"google.golang.org/grpc/credentials/insecure"
	"google.golang.org/protobuf/types/known/emptypb"
)

//go:embed ui_templates/*
var uiTemplates embed.FS

var (
	uiPort       int
	uiGRPCServer string
)

// uiCmd represents the ui command
var uiCmd = &cobra.Command{
	Use:   "ui",
	Short: "Start the dev UI for NSM",
	Long: `Start a web-based dev UI for interacting with NV-Switch Manager.

The UI provides:
  - List of registered NV-switches
  - Firmware update monitoring with filtering
  - Quick actions for power cycle and updates

Example:
  nvswitch-manager ui --port 8080 --grpc-server localhost:50051`,
	Run: func(cmd *cobra.Command, args []string) {
		runUI()
	},
}

func init() {
	rootCmd.AddCommand(uiCmd)
	uiCmd.Flags().IntVar(&uiPort, "port", 8080, "HTTP port for the UI")
	uiCmd.Flags().StringVar(&uiGRPCServer, "grpc-server", "localhost:50051", "NSM gRPC server address")
}

// UIServer handles HTTP requests and proxies to gRPC
type UIServer struct {
	grpcAddr  string
	templates *template.Template
}

// formatEndpoint brackets IPv6 addresses when a port is supplied and preserves
// the table's bare-address display when the server omits the port.
func formatEndpoint(address string, port int32) string {
	if port == 0 {
		return address
	}
	return net.JoinHostPort(address, strconv.Itoa(int(port)))
}

func runUI() {
	// Parse templates
	tmpl, err := template.New("").Funcs(template.FuncMap{
		"formatEndpoint": formatEndpoint,
		"formatTime": func(t time.Time) string {
			return t.Format("2006-01-02 15:04:05")
		},
		"stateClass": func(state string) string {
			switch state {
			case "Completed":
				return "status-completed"
			case "Failed":
				return "status-failed"
			case "Cancelled":
				return "status-cancelled"
			case "Queued":
				return "status-queued"
			default:
				return "status-active"
			}
		},
	}).ParseFS(uiTemplates, "ui_templates/*.html")
	if err != nil {
		log.Fatalf("Failed to parse templates: %v", err)
	}

	server := &UIServer{
		grpcAddr:  uiGRPCServer,
		templates: tmpl,
	}

	// Routes
	http.HandleFunc("/", server.handleIndex)
	http.HandleFunc("/switches", server.handleSwitches)
	http.HandleFunc("/updates", server.handleUpdates)
	http.HandleFunc("/api/switches", server.handleAPISwitches)
	http.HandleFunc("/api/updates", server.handleAPIUpdates)
	http.HandleFunc("/api/bundles", server.handleAPIBundles)
	http.HandleFunc("/api/power-control", server.handleAPIPowerControl)
	http.HandleFunc("/api/queue-update", server.handleAPIQueueUpdate)
	http.HandleFunc("/api/cancel-update", server.handleAPICancelUpdate)
	http.HandleFunc("/api/register-switch", server.handleAPIRegisterSwitch)
	http.HandleFunc("/api/update-log", server.handleAPIUpdateLog)

	addr := fmt.Sprintf(":%d", uiPort)
	log.Infof("Starting dev UI at http://localhost%s (gRPC: %s)", addr, uiGRPCServer)
	if err := http.ListenAndServe(addr, nil); err != nil {
		log.Fatalf("Failed to start UI server: %v", err)
	}
}

func (s *UIServer) getGRPCClient() (pb.NVSwitchManagerClient, *grpc.ClientConn, error) {
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()

	conn, err := grpc.DialContext(ctx, s.grpcAddr,
		grpc.WithTransportCredentials(insecure.NewCredentials()),
		grpc.WithBlock(),
	)
	if err != nil {
		return nil, nil, fmt.Errorf("failed to connect to gRPC server: %w", err)
	}

	return pb.NewNVSwitchManagerClient(conn), conn, nil
}

// Page handlers

func (s *UIServer) handleIndex(w http.ResponseWriter, r *http.Request) {
	http.Redirect(w, r, "/switches", http.StatusTemporaryRedirect)
}

// SwitchWithStatus wraps a switch with its update status summary
type SwitchWithStatus struct {
	Switch       *pb.NVSwitchTray
	UpdateStatus *UpdateStatusSummary
}

// UpdateStatusSummary summarizes firmware update status for a switch
type UpdateStatusSummary struct {
	Total           int
	InProgress      int
	Queued          int
	Completed       int
	Failed          int
	Cancelled       int
	InProgressComps []string
	QueuedComps     []string
	CompletedComps  []string
	FailedComps     []string
	StatusText      string
	StatusClass     string
}

func componentName(c pb.NVSwitchComponent) string {
	switch c {
	case pb.NVSwitchComponent_NVSWITCH_COMPONENT_BMC:
		return "BMC"
	case pb.NVSwitchComponent_NVSWITCH_COMPONENT_CPLD:
		return "CPLD"
	case pb.NVSwitchComponent_NVSWITCH_COMPONENT_BIOS:
		return "BIOS"
	case pb.NVSwitchComponent_NVSWITCH_COMPONENT_NVOS:
		return "NVOS"
	default:
		return "?"
	}
}

func (s *UIServer) handleSwitches(w http.ResponseWriter, r *http.Request) {
	client, conn, err := s.getGRPCClient()
	if err != nil {
		s.renderError(w, "Connection Error", err.Error())
		return
	}
	defer conn.Close()

	ctx := context.Background()
	resp, err := client.GetNVSwitches(ctx, &pb.NVSwitchRequest{})
	if err != nil {
		s.renderError(w, "API Error", err.Error())
		return
	}

	// Get filter and sort parameters
	statusFilter := r.URL.Query().Get("status")
	rackFilter := r.URL.Query().Get("rack")
	sortBy := r.URL.Query().Get("sort")

	// Get bundles for the dropdown
	bundlesResp, _ := client.ListBundles(ctx, &emptypb.Empty{})
	var bundles []string
	if bundlesResp != nil {
		for _, b := range bundlesResp.Bundles {
			bundles = append(bundles, b.Version)
		}
	}

	// Get all updates in a single API call
	allUpdatesResp, _ := client.GetAllUpdates(ctx, &emptypb.Empty{})

	// Group updates by switch UUID
	updatesBySwitch := make(map[string][]*pb.FirmwareUpdateInfo)
	if allUpdatesResp != nil {
		for _, u := range allUpdatesResp.Updates {
			updatesBySwitch[u.SwitchUuid] = append(updatesBySwitch[u.SwitchUuid], u)
		}
	}

	// Collect unique rack IDs and get update status for each switch
	rackIDSet := make(map[string]bool)
	var allSwitchesWithStatus []SwitchWithStatus
	for _, sw := range resp.Nvswitches {
		// Track rack IDs for the filter dropdown
		if sw.RackId != "" {
			rackIDSet[sw.RackId] = true
		}
		status := &UpdateStatusSummary{}

		switchUpdates := updatesBySwitch[sw.Uuid]
		if len(switchUpdates) > 0 {
			// Find the most recent bundle (by looking at the newest update's BundleUpdateId or CreatedAt)
			// Updates are returned sorted by CreatedAt descending (newest first)
			var latestBundleID string
			var latestCreatedAt time.Time

			// First pass: find the most recent bundle
			for _, u := range switchUpdates {
				if u.CreatedAt != nil {
					created := u.CreatedAt.AsTime()
					if created.After(latestCreatedAt) {
						latestCreatedAt = created
						latestBundleID = u.BundleUpdateId
					}
				}
			}

			// Second pass: only count updates from the most recent bundle
			for _, u := range switchUpdates {
				// Skip if not from the most recent bundle
				if latestBundleID != "" && u.BundleUpdateId != latestBundleID {
					continue
				}
				// For single-component updates (no bundle ID), only include if it's the newest
				if latestBundleID == "" && u.BundleUpdateId == "" {
					if u.CreatedAt == nil || !u.CreatedAt.AsTime().Equal(latestCreatedAt) {
						continue
					}
				}

				status.Total++
				compName := componentName(u.Component)
				switch u.State {
				case pb.UpdateState_UPDATE_STATE_QUEUED:
					status.Queued++
					status.QueuedComps = append(status.QueuedComps, compName)
				case pb.UpdateState_UPDATE_STATE_COMPLETED:
					status.Completed++
					status.CompletedComps = append(status.CompletedComps, compName)
				case pb.UpdateState_UPDATE_STATE_FAILED:
					status.Failed++
					status.FailedComps = append(status.FailedComps, compName)
				case pb.UpdateState_UPDATE_STATE_CANCELLED:
					status.Cancelled++
				default:
					// Any other state is "in progress"
					if !isTerminalState(u.State) && u.State != pb.UpdateState_UPDATE_STATE_QUEUED {
						status.InProgress++
						status.InProgressComps = append(status.InProgressComps, compName)
					}
				}
			}
		}

		// Determine status text and class
		// Build status text showing all states
		var parts []string
		if status.Completed > 0 {
			parts = append(parts, strings.Join(status.CompletedComps, "/")+" ✓")
		}
		if status.InProgress > 0 {
			parts = append(parts, strings.Join(status.InProgressComps, "/")+" ▶")
		}
		if status.Queued > 0 {
			parts = append(parts, strings.Join(status.QueuedComps, "/")+" ⏳")
		}
		if status.Failed > 0 {
			parts = append(parts, strings.Join(status.FailedComps, "/")+" ✗")
		}

		if len(parts) > 0 {
			status.StatusText = strings.Join(parts, " ")
		} else {
			status.StatusText = "No updates"
		}

		// Determine status class based on priority
		if status.Failed > 0 {
			status.StatusClass = "status-failed"
		} else if status.InProgress > 0 {
			status.StatusClass = "status-active"
		} else if status.Queued > 0 {
			status.StatusClass = "status-queued"
		} else if status.Completed > 0 {
			status.StatusClass = "status-completed"
		} else {
			status.StatusClass = ""
		}

		allSwitchesWithStatus = append(allSwitchesWithStatus, SwitchWithStatus{
			Switch:       sw,
			UpdateStatus: status,
		})
	}

	// Apply filtering
	var switchesWithStatus []SwitchWithStatus
	for _, sws := range allSwitchesWithStatus {
		include := true

		// Rack filter
		if rackFilter != "" {
			if rackFilter == "_none_" {
				include = sws.Switch.RackId == ""
			} else {
				include = sws.Switch.RackId == rackFilter
			}
		}

		// Status filter (only if still included)
		if include {
			switch statusFilter {
			case "in_progress":
				include = sws.UpdateStatus.InProgress > 0
			case "queued":
				include = sws.UpdateStatus.Queued > 0 && sws.UpdateStatus.InProgress == 0
			case "completed":
				// All updates completed, none failed/in progress/queued
				include = sws.UpdateStatus.Completed > 0 &&
					sws.UpdateStatus.InProgress == 0 &&
					sws.UpdateStatus.Queued == 0 &&
					sws.UpdateStatus.Failed == 0
			case "failed":
				include = sws.UpdateStatus.Failed > 0
			case "no_updates":
				include = sws.UpdateStatus.Total == 0
			}
		}

		if include {
			switchesWithStatus = append(switchesWithStatus, sws)
		}
	}

	// Apply sorting
	switch sortBy {
	case "uuid":
		sort.Slice(switchesWithStatus, func(i, j int) bool {
			return switchesWithStatus[i].Switch.Uuid < switchesWithStatus[j].Switch.Uuid
		})
	case "rack_id":
		sort.Slice(switchesWithStatus, func(i, j int) bool {
			return switchesWithStatus[i].Switch.RackId < switchesWithStatus[j].Switch.RackId
		})
	case "bmc_ip":
		sort.Slice(switchesWithStatus, func(i, j int) bool {
			ipI, ipJ := "", ""
			if switchesWithStatus[i].Switch.Bmc != nil {
				ipI = switchesWithStatus[i].Switch.Bmc.IpAddress
			}
			if switchesWithStatus[j].Switch.Bmc != nil {
				ipJ = switchesWithStatus[j].Switch.Bmc.IpAddress
			}
			return ipI < ipJ
		})
	case "nvos_ip":
		sort.Slice(switchesWithStatus, func(i, j int) bool {
			ipI, ipJ := "", ""
			if switchesWithStatus[i].Switch.Nvos != nil {
				ipI = switchesWithStatus[i].Switch.Nvos.IpAddress
			}
			if switchesWithStatus[j].Switch.Nvos != nil {
				ipJ = switchesWithStatus[j].Switch.Nvos.IpAddress
			}
			return ipI < ipJ
		})
	case "status":
		// Sort by status priority: failed, in_progress, queued, completed, no_updates
		sort.Slice(switchesWithStatus, func(i, j int) bool {
			return statusPriority(switchesWithStatus[i].UpdateStatus) <
				statusPriority(switchesWithStatus[j].UpdateStatus)
		})
	}

	// Collect rack IDs into sorted slice for dropdown
	var rackIDs []string
	for rackID := range rackIDSet {
		rackIDs = append(rackIDs, rackID)
	}
	sort.Strings(rackIDs)

	data := map[string]interface{}{
		"Switches":     switchesWithStatus,
		"Bundles":      bundles,
		"RackIDs":      rackIDs,
		"Page":         "switches",
		"StatusFilter": statusFilter,
		"RackFilter":   rackFilter,
		"SortBy":       sortBy,
	}

	s.templates.ExecuteTemplate(w, "layout.html", data)
}

// statusPriority returns a sort priority for update status (lower = higher priority)
func statusPriority(status *UpdateStatusSummary) int {
	if status.Failed > 0 {
		return 1
	}
	if status.InProgress > 0 {
		return 2
	}
	if status.Queued > 0 {
		return 3
	}
	if status.Completed > 0 {
		return 4
	}
	return 5 // no updates
}

func isTerminalState(state pb.UpdateState) bool {
	return state == pb.UpdateState_UPDATE_STATE_COMPLETED ||
		state == pb.UpdateState_UPDATE_STATE_FAILED ||
		state == pb.UpdateState_UPDATE_STATE_CANCELLED
}

func (s *UIServer) handleUpdates(w http.ResponseWriter, r *http.Request) {
	client, conn, err := s.getGRPCClient()
	if err != nil {
		s.renderError(w, "Connection Error", err.Error())
		return
	}
	defer conn.Close()

	ctx := context.Background()

	// Get filter parameters
	stateFilter := r.URL.Query().Get("state")
	rackFilter := r.URL.Query().Get("rack")
	switchFilter := r.URL.Query().Get("switch")
	sortBy := r.URL.Query().Get("sort")
	showAll := r.URL.Query().Get("all") == "true"

	// Get all switches first
	switchesResp, err := client.GetNVSwitches(ctx, &pb.NVSwitchRequest{})
	if err != nil {
		s.renderError(w, "API Error", err.Error())
		return
	}

	// Build switch UUID to rack ID mapping and collect unique rack IDs
	switchToRack := make(map[string]string)
	rackIDSet := make(map[string]bool)
	for _, sw := range switchesResp.Nvswitches {
		switchToRack[sw.Uuid] = sw.RackId
		if sw.RackId != "" {
			rackIDSet[sw.RackId] = true
		}
	}

	// Build sorted rack IDs list
	var rackIDs []string
	for rackID := range rackIDSet {
		rackIDs = append(rackIDs, rackID)
	}
	sort.Strings(rackIDs)

	// Get all updates in a single API call
	updatesResp, err := client.GetAllUpdates(ctx, &emptypb.Empty{})
	if err != nil {
		s.renderError(w, "API Error", err.Error())
		return
	}
	allUpdates := updatesResp.Updates

	// Filter updates
	var filteredUpdates []*pb.FirmwareUpdateInfo
	for _, u := range allUpdates {
		state := stateToString(u.State)

		// Filter by state
		if stateFilter != "" && state != stateFilter {
			continue
		}

		// Filter by specific switch UUID
		if switchFilter != "" && u.SwitchUuid != switchFilter {
			continue
		}

		// Filter by rack ID
		if rackFilter != "" {
			switchRackID := switchToRack[u.SwitchUuid]
			if rackFilter == "_none_" {
				if switchRackID != "" {
					continue
				}
			} else if switchRackID != rackFilter {
				continue
			}
		}

		// By default, hide terminal states unless "all" is requested
		if !showAll && stateFilter == "" {
			if u.State == pb.UpdateState_UPDATE_STATE_COMPLETED ||
				u.State == pb.UpdateState_UPDATE_STATE_FAILED ||
				u.State == pb.UpdateState_UPDATE_STATE_CANCELLED {
				continue
			}
		}

		filteredUpdates = append(filteredUpdates, u)
	}

	// Apply sorting
	switch sortBy {
	case "rack_id":
		sort.Slice(filteredUpdates, func(i, j int) bool {
			return switchToRack[filteredUpdates[i].SwitchUuid] < switchToRack[filteredUpdates[j].SwitchUuid]
		})
	case "update_id":
		sort.Slice(filteredUpdates, func(i, j int) bool {
			return filteredUpdates[i].Id < filteredUpdates[j].Id
		})
	case "switch":
		sort.Slice(filteredUpdates, func(i, j int) bool {
			return filteredUpdates[i].SwitchUuid < filteredUpdates[j].SwitchUuid
		})
	case "component":
		sort.Slice(filteredUpdates, func(i, j int) bool {
			return filteredUpdates[i].Component < filteredUpdates[j].Component
		})
	case "bundle":
		sort.Slice(filteredUpdates, func(i, j int) bool {
			return filteredUpdates[i].BundleVersion < filteredUpdates[j].BundleVersion
		})
	case "state":
		sort.Slice(filteredUpdates, func(i, j int) bool {
			return filteredUpdates[i].State < filteredUpdates[j].State
		})
	case "started":
		sort.Slice(filteredUpdates, func(i, j int) bool {
			if filteredUpdates[i].CreatedAt == nil {
				return true
			}
			if filteredUpdates[j].CreatedAt == nil {
				return false
			}
			return filteredUpdates[i].CreatedAt.AsTime().Before(filteredUpdates[j].CreatedAt.AsTime())
		})
	case "updated":
		sort.Slice(filteredUpdates, func(i, j int) bool {
			if filteredUpdates[i].UpdatedAt == nil {
				return true
			}
			if filteredUpdates[j].UpdatedAt == nil {
				return false
			}
			return filteredUpdates[i].UpdatedAt.AsTime().Before(filteredUpdates[j].UpdatedAt.AsTime())
		})
	}

	data := map[string]interface{}{
		"Updates":      filteredUpdates,
		"SwitchToRack": switchToRack,
		"RackIDs":      rackIDs,
		"Page":         "updates",
		"StateFilter":  stateFilter,
		"RackFilter":   rackFilter,
		"SortBy":       sortBy,
		"ShowAll":      showAll,
		"States": []string{
			"Queued", "Power Cycling", "Waiting for Reachability", "Copying",
			"Uploading", "Installing", "Polling Completion", "Verifying",
			"Cleaning Up", "Completed", "Failed", "Cancelled",
		},
	}

	s.templates.ExecuteTemplate(w, "layout.html", data)
}

// API handlers for AJAX calls

func (s *UIServer) handleAPISwitches(w http.ResponseWriter, r *http.Request) {
	client, conn, err := s.getGRPCClient()
	if err != nil {
		http.Error(w, err.Error(), http.StatusServiceUnavailable)
		return
	}
	defer conn.Close()

	resp, err := client.GetNVSwitches(context.Background(), &pb.NVSwitchRequest{})
	if err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}

	w.Header().Set("Content-Type", "application/json")
	json.NewEncoder(w).Encode(resp.Nvswitches)
}

func (s *UIServer) handleAPIUpdates(w http.ResponseWriter, r *http.Request) {
	client, conn, err := s.getGRPCClient()
	if err != nil {
		http.Error(w, err.Error(), http.StatusServiceUnavailable)
		return
	}
	defer conn.Close()

	switchUUID := r.URL.Query().Get("switch")
	if switchUUID == "" {
		http.Error(w, "switch parameter required", http.StatusBadRequest)
		return
	}

	resp, err := client.GetUpdatesForSwitch(context.Background(), &pb.GetUpdatesForSwitchRequest{SwitchUuid: switchUUID})
	if err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}

	w.Header().Set("Content-Type", "application/json")
	json.NewEncoder(w).Encode(resp.Updates)
}

func (s *UIServer) handleAPIBundles(w http.ResponseWriter, r *http.Request) {
	client, conn, err := s.getGRPCClient()
	if err != nil {
		http.Error(w, err.Error(), http.StatusServiceUnavailable)
		return
	}
	defer conn.Close()

	resp, err := client.ListBundles(context.Background(), &emptypb.Empty{})
	if err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}

	w.Header().Set("Content-Type", "application/json")
	json.NewEncoder(w).Encode(resp.Bundles)
}

func (s *UIServer) handleAPIPowerControl(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodPost {
		http.Error(w, "Method not allowed", http.StatusMethodNotAllowed)
		return
	}

	switchUUID := r.FormValue("switch_uuid")
	if switchUUID == "" {
		http.Error(w, "switch_uuid required", http.StatusBadRequest)
		return
	}

	actionStr := r.FormValue("action")
	if actionStr == "" {
		actionStr = "PowerCycle"
	}

	actionMap := map[string]pb.PowerAction{
		"ForceOff":         pb.PowerAction_POWER_ACTION_FORCE_OFF,
		"PowerCycle":       pb.PowerAction_POWER_ACTION_POWER_CYCLE,
		"GracefulShutdown": pb.PowerAction_POWER_ACTION_GRACEFUL_SHUTDOWN,
		"On":               pb.PowerAction_POWER_ACTION_ON,
		"ForceOn":          pb.PowerAction_POWER_ACTION_FORCE_ON,
		"GracefulRestart":  pb.PowerAction_POWER_ACTION_GRACEFUL_RESTART,
		"ForceRestart":     pb.PowerAction_POWER_ACTION_FORCE_RESTART,
	}

	action, ok := actionMap[actionStr]
	if !ok {
		http.Error(w, "invalid action: "+actionStr, http.StatusBadRequest)
		return
	}

	client, conn, err := s.getGRPCClient()
	if err != nil {
		http.Error(w, err.Error(), http.StatusServiceUnavailable)
		return
	}
	defer conn.Close()

	resp, err := client.PowerControl(context.Background(), &pb.PowerControlRequest{
		Uuids:  []string{switchUUID},
		Action: action,
	})
	if err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}

	w.Header().Set("Content-Type", "application/json")
	json.NewEncoder(w).Encode(resp)
}

// BulkQueueUpdateRequest wraps a bulk firmware update request
type BulkQueueUpdateRequest struct {
	SwitchUUIDs []string `json:"switch_uuids"`
	Bundle      string   `json:"bundle"`
	Components  string   `json:"components"`
}

func (s *UIServer) handleAPIQueueUpdate(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodPost {
		http.Error(w, "Method not allowed", http.StatusMethodNotAllowed)
		return
	}

	var switchUUIDs []string
	var bundleVersion string
	var componentsStr string

	// Check content type - support both JSON and form data
	contentType := r.Header.Get("Content-Type")
	if strings.HasPrefix(contentType, "application/json") {
		// Parse JSON body for bulk updates
		var bulkReq BulkQueueUpdateRequest
		if err := json.NewDecoder(r.Body).Decode(&bulkReq); err != nil {
			http.Error(w, "Invalid JSON: "+err.Error(), http.StatusBadRequest)
			return
		}
		switchUUIDs = bulkReq.SwitchUUIDs
		bundleVersion = bulkReq.Bundle
		componentsStr = bulkReq.Components
	} else {
		// Parse form values (single switch, backward compatible)
		switchUUID := r.FormValue("switch_uuid")
		if switchUUID != "" {
			switchUUIDs = []string{switchUUID}
		}
		bundleVersion = r.FormValue("bundle")
		componentsStr = r.FormValue("components")
	}

	if len(switchUUIDs) == 0 || bundleVersion == "" {
		http.Error(w, "switch_uuid(s) and bundle required", http.StatusBadRequest)
		return
	}

	client, conn, err := s.getGRPCClient()
	if err != nil {
		http.Error(w, err.Error(), http.StatusServiceUnavailable)
		return
	}
	defer conn.Close()

	// Parse components
	var components []pb.NVSwitchComponent
	if componentsStr != "" {
		for _, c := range strings.Split(componentsStr, ",") {
			switch strings.ToLower(strings.TrimSpace(c)) {
			case "bmc":
				components = append(components, pb.NVSwitchComponent_NVSWITCH_COMPONENT_BMC)
			case "cpld":
				components = append(components, pb.NVSwitchComponent_NVSWITCH_COMPONENT_CPLD)
			case "bios":
				components = append(components, pb.NVSwitchComponent_NVSWITCH_COMPONENT_BIOS)
			case "nvos":
				components = append(components, pb.NVSwitchComponent_NVSWITCH_COMPONENT_NVOS)
			}
		}
	}

	// Use bulk API for all requests
	resp, err := client.QueueUpdates(context.Background(), &pb.QueueUpdatesRequest{
		SwitchUuids:   switchUUIDs,
		BundleVersion: bundleVersion,
		Components:    components,
	})
	if err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}

	w.Header().Set("Content-Type", "application/json")
	json.NewEncoder(w).Encode(resp)
}

func (s *UIServer) handleAPICancelUpdate(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodPost {
		http.Error(w, "Method not allowed", http.StatusMethodNotAllowed)
		return
	}

	updateID := r.FormValue("update_id")
	if updateID == "" {
		http.Error(w, "update_id required", http.StatusBadRequest)
		return
	}

	client, conn, err := s.getGRPCClient()
	if err != nil {
		http.Error(w, err.Error(), http.StatusServiceUnavailable)
		return
	}
	defer conn.Close()

	resp, err := client.CancelUpdate(context.Background(), &pb.CancelUpdateRequest{UpdateId: updateID})
	if err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}

	w.Header().Set("Content-Type", "application/json")
	json.NewEncoder(w).Encode(resp)
}

// SwitchRegistrationRequest represents a single switch registration in JSON format
type SwitchRegistrationRequest struct {
	RackID string `json:"rack_id"`
	BMC    struct {
		MAC      string `json:"mac"`
		IP       string `json:"ip"`
		Port     int32  `json:"port"`
		User     string `json:"user"`
		Password string `json:"password"`
	} `json:"bmc"`
	NVOS struct {
		MAC      string `json:"mac"`
		IP       string `json:"ip"`
		Port     int32  `json:"port"`
		User     string `json:"user"`
		Password string `json:"password"`
	} `json:"nvos"`
}

// BulkRegistrationRequest wraps multiple switch registrations
type BulkRegistrationRequest struct {
	Switches []SwitchRegistrationRequest `json:"switches"`
}

func (s *UIServer) handleAPIRegisterSwitch(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodPost {
		http.Error(w, "Method not allowed", http.StatusMethodNotAllowed)
		return
	}

	var registrations []SwitchRegistrationRequest

	// Check content type - support both JSON and form data
	contentType := r.Header.Get("Content-Type")
	if strings.HasPrefix(contentType, "application/json") {
		// Parse JSON body
		var bulkReq BulkRegistrationRequest
		if err := json.NewDecoder(r.Body).Decode(&bulkReq); err != nil {
			http.Error(w, "Invalid JSON: "+err.Error(), http.StatusBadRequest)
			return
		}
		registrations = bulkReq.Switches
	} else {
		// Parse form values (single switch, backward compatible)
		reg := SwitchRegistrationRequest{
			RackID: r.FormValue("rack_id"),
		}
		reg.BMC.MAC = r.FormValue("bmc_mac")
		reg.BMC.IP = r.FormValue("bmc_ip")
		if p, err := parseInt32(r.FormValue("bmc_port")); err == nil {
			reg.BMC.Port = p
		}
		reg.BMC.User = r.FormValue("bmc_user")
		reg.BMC.Password = r.FormValue("bmc_pass")
		reg.NVOS.MAC = r.FormValue("nvos_mac")
		reg.NVOS.IP = r.FormValue("nvos_ip")
		if p, err := parseInt32(r.FormValue("nvos_port")); err == nil {
			reg.NVOS.Port = p
		}
		reg.NVOS.User = r.FormValue("nvos_user")
		reg.NVOS.Password = r.FormValue("nvos_pass")
		registrations = []SwitchRegistrationRequest{reg}
	}

	if len(registrations) == 0 {
		http.Error(w, "No switches to register", http.StatusBadRequest)
		return
	}

	// Validate and build gRPC request
	var protoRequests []*pb.RegisterNVSwitchRequest
	for i, reg := range registrations {
		if reg.BMC.MAC == "" || reg.BMC.IP == "" || reg.NVOS.MAC == "" || reg.NVOS.IP == "" {
			http.Error(w, fmt.Sprintf("Switch %d: BMC and NVOS MAC/IP are required", i+1), http.StatusBadRequest)
			return
		}

		// Apply defaults
		bmcUser := reg.BMC.User
		if bmcUser == "" {
			bmcUser = "root"
		}
		bmcPass := reg.BMC.Password
		nvosUser := reg.NVOS.User
		if nvosUser == "" {
			nvosUser = "admin"
		}
		nvosPass := reg.NVOS.Password

		protoRequests = append(protoRequests, &pb.RegisterNVSwitchRequest{
			Vendor: pb.Vendor_VENDOR_NVIDIA,
			RackId: reg.RackID,
			Bmc: &pb.Subsystem{
				MacAddress: reg.BMC.MAC,
				IpAddress:  reg.BMC.IP,
				Port:       reg.BMC.Port,
				Credentials: &pb.Credentials{
					Username: bmcUser,
					Password: bmcPass,
				},
			},
			Nvos: &pb.Subsystem{
				MacAddress: reg.NVOS.MAC,
				IpAddress:  reg.NVOS.IP,
				Port:       reg.NVOS.Port,
				Credentials: &pb.Credentials{
					Username: nvosUser,
					Password: nvosPass,
				},
			},
		})
	}

	client, conn, err := s.getGRPCClient()
	if err != nil {
		http.Error(w, err.Error(), http.StatusServiceUnavailable)
		return
	}
	defer conn.Close()

	resp, err := client.RegisterNVSwitches(context.Background(), &pb.RegisterNVSwitchesRequest{
		RegistrationRequests: protoRequests,
	})
	if err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}

	w.Header().Set("Content-Type", "application/json")
	json.NewEncoder(w).Encode(resp)
}

func parseInt32(s string) (int32, error) {
	var i int
	_, err := fmt.Sscanf(s, "%d", &i)
	return int32(i), err
}

func (s *UIServer) handleAPIUpdateLog(w http.ResponseWriter, r *http.Request) {
	updateID := r.URL.Query().Get("id")
	if updateID == "" {
		http.Error(w, "id parameter required", http.StatusBadRequest)
		return
	}

	// Sanitize the update ID to prevent path traversal
	// UUID format: xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx
	for _, c := range updateID {
		if !((c >= 'a' && c <= 'f') || (c >= '0' && c <= '9') || c == '-') {
			http.Error(w, "invalid update ID format", http.StatusBadRequest)
			return
		}
	}

	logPath := fmt.Sprintf("/tmp/nsm-script-%s.log", updateID)

	content, err := os.ReadFile(logPath)
	if err != nil {
		if os.IsNotExist(err) {
			w.Header().Set("Content-Type", "application/json")
			json.NewEncoder(w).Encode(map[string]interface{}{
				"found": false,
				"error": "Log file not found. This update may not use the script strategy or hasn't started yet.",
			})
			return
		}
		http.Error(w, "failed to read log: "+err.Error(), http.StatusInternalServerError)
		return
	}

	w.Header().Set("Content-Type", "application/json")
	json.NewEncoder(w).Encode(map[string]interface{}{
		"found":   true,
		"content": string(content),
		"path":    logPath,
	})
}

func (s *UIServer) renderError(w http.ResponseWriter, title, message string) {
	data := map[string]interface{}{
		"ErrorTitle":   title,
		"ErrorMessage": message,
		"Page":         "error",
	}
	s.templates.ExecuteTemplate(w, "layout.html", data)
}
