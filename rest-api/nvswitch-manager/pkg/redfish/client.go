// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package redfish wraps gofish to provide service-focused Redfish operations (inventory, power control, and firmware upload)
// for NV-Switch trays with minimal coupling to underlying transport details.
package redfish

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"io/ioutil"
	"net"
	"net/http"
	"net/url"
	"os"
	"strconv"

	"github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/pkg/objects/bmc"

	log "github.com/sirupsen/logrus"
	"github.com/stmcginnis/gofish"
	"github.com/stmcginnis/gofish/redfish"
)

// RedfishClient manages a gofish API client and BMC context to perform typed Redfish operations against a device.
type RedfishClient struct {
	bmc *bmc.BMC
	*gofish.APIClient
	gofish.ClientConfig
}

// buildEndpoint constructs the Redfish HTTPS endpoint URL for a BMC at the given
// IP and port. net.JoinHostPort brackets IPv6 literals so the resulting URL has
// a valid authority component (e.g. https://[2001:db8::1]:8443).
func buildEndpoint(ip net.IP, port int) string {
	return (&url.URL{
		Scheme: "https",
		Host:   net.JoinHostPort(ip.String(), strconv.Itoa(port)),
	}).String()
}

// New creates a RedfishClient for the given BMC and context.
func New(ctx context.Context, b *bmc.BMC, reuse_connections bool) (*RedfishClient, error) {
	if b == nil {
		return nil, fmt.Errorf("BMC is nil")
	}
	if b.Credential == nil {
		return nil, fmt.Errorf("BMC credentials not set")
	}

	// Build the Redfish endpoint, bracketing IPv6 literals for a valid URL authority.
	endpoint := buildEndpoint(b.IP, b.GetPort())

	client_config := gofish.ClientConfig{
		Endpoint:         endpoint,
		Username:         b.Credential.User,
		Password:         b.Credential.Password.Value,
		Insecure:         true,
		ReuseConnections: reuse_connections,
	}

	client, err := gofish.ConnectContext(ctx, client_config)
	if err != nil {
		return nil, err
	}

	return &RedfishClient{bmc: b, APIClient: client, ClientConfig: client_config}, nil
}

// QueryChassis fetches the NV-Switch chassis or returns an error if not found.
func (c *RedfishClient) QueryChassis() (*redfish.Chassis, error) {
	chassis, err := c.Service.Chassis()
	if err != nil {
		return nil, err
	}

	// NV-Switch trays may have different chassis IDs; return the first one found
	if len(chassis) > 0 {
		return chassis[0], nil
	}

	return nil, errors.New("could not find a chassis subsystem")
}

// QueryManager fetches the BMC manager or returns an error if not found.
func (c *RedfishClient) QueryManager() (*redfish.Manager, error) {
	managers, err := c.Service.Managers()
	if err != nil {
		return nil, err
	}

	for _, m := range managers {
		if m.ID == "bmc" {
			return m, nil
		}
	}

	// Return the first manager if "bmc" not found
	if len(managers) > 0 {
		return managers[0], nil
	}

	return nil, errors.New("could not find the BMC manager")
}

// ResetType represents a Redfish ComputerSystem.Reset action.
type ResetType string

const (
	ResetForceOff         ResetType = "ForceOff"
	ResetPowerCycle       ResetType = "PowerCycle"
	ResetGracefulShutdown ResetType = "GracefulShutdown"
	ResetOn               ResetType = "On"
	ResetForceOn          ResetType = "ForceOn"
	ResetGracefulRestart  ResetType = "GracefulRestart"
	ResetForceRestart     ResetType = "ForceRestart"
)

// ResetSystem performs a ComputerSystem.Reset action on the NV-Switch tray via Redfish.
func (c *RedfishClient) ResetSystem(resetType ResetType) (*http.Response, error) {
	uri := "/redfish/v1/Systems/System_0/Actions/ComputerSystem.Reset"
	body := map[string]interface{}{
		"ResetType": string(resetType),
	}

	log.Printf("Resetting NV-Switch (action=%s)... uri %v", resetType, uri)
	return c.Post(uri, body)
}

// PowerCycle performs a power cycle on the NV-Switch tray via Redfish.
func (c *RedfishClient) PowerCycle() (*http.Response, error) {
	return c.ResetSystem(ResetPowerCycle)
}

type ResetBMCType string

const (
	GracefulBMCRestart ResetBMCType = "GracefulRestart"
	ForceBMCRestart    ResetBMCType = "ForceRestart"
)

// ResetBMC resets the BMC manager.
func (c *RedfishClient) ResetBMC(resetType ResetBMCType) (*http.Response, error) {
	manager, err := c.QueryManager()
	if err != nil {
		return nil, err
	}

	uri := fmt.Sprintf("%s/Actions/Manager.Reset", manager.ODataID)
	body := map[string]interface{}{
		"ResetType": resetType,
	}
	return c.Post(uri, body)
}

// UpdateService returns the Redfish UpdateService resource.
func (c *RedfishClient) UpdateService() (*redfish.UpdateService, error) {
	return c.Service.UpdateService()
}

// FirmwareInventories lists software/firmware inventories.
func (c *RedfishClient) FirmwareInventories() ([]*redfish.SoftwareInventory, error) {
	updateService, err := c.UpdateService()
	if err != nil {
		return nil, err
	}

	return updateService.FirmwareInventories()
}

// GetHttpPushUriApplyTime returns the current HttpPushUriApplyTime setting from UpdateService.
// Returns the ApplyTime string (e.g., "Immediate", "OnReset") or empty string if not found.
func (c *RedfishClient) GetHttpPushUriApplyTime() (string, error) {
	updateService, err := c.UpdateService()
	if err != nil {
		return "", fmt.Errorf("failed to get UpdateService: %w", err)
	}

	// Access HTTPPushURIOptions.HTTPPushURIApplyTime.ApplyTime
	if updateService.HTTPPushURIOptions.HTTPPushURIApplyTime.ApplyTime != "" {
		return string(updateService.HTTPPushURIOptions.HTTPPushURIApplyTime.ApplyTime), nil
	}

	return "", nil
}

// SetHttpPushUriApplyTimeImmediate configures firmware apply time to Immediate on UpdateService.
func (c *RedfishClient) SetHttpPushUriApplyTimeImmediate() (*http.Response, error) {
	body := map[string]interface{}{
		"HttpPushUriOptions": map[string]interface{}{
			"HttpPushUriApplyTime": map[string]interface{}{
				"ApplyTime": "Immediate",
			},
		},
	}

	return c.Patch("/redfish/v1/UpdateService", body)
}

// EnsureHttpPushUriApplyTimeImmediate checks the current apply time and sets it to Immediate if needed.
// Returns nil if already Immediate or successfully set. Returns error only on real failures.
func (c *RedfishClient) EnsureHttpPushUriApplyTimeImmediate() error {
	// First, check the current setting
	currentApplyTime, err := c.GetHttpPushUriApplyTime()
	if err != nil {
		return fmt.Errorf("failed to get current apply time: %w", err)
	}

	// If already Immediate, nothing to do
	if currentApplyTime == "Immediate" {
		log.Debugf("HttpPushUriApplyTime is already set to Immediate")
		return nil
	}

	// Need to set it to Immediate
	log.Infof("HttpPushUriApplyTime is '%s', setting to Immediate", currentApplyTime)
	resp, err := c.SetHttpPushUriApplyTimeImmediate()
	if resp != nil && resp.Body != nil {
		resp.Body.Close()
	}
	if err != nil {
		return fmt.Errorf("failed to set apply time to Immediate: %w", err)
	}

	return nil
}

// UploadFirmwareByPath opens a local file and uploads it via UpdateService.
func (c *RedfishClient) UploadFirmwareByPath(firmwarePath string) (*http.Response, error) {
	// Open the firmware file
	firmwareFile, err := os.Open(firmwarePath)
	if err != nil {
		return nil, err
	}
	defer firmwareFile.Close()

	return c.UploadFirmware(firmwareFile)

}

// UploadFirmware uploads firmware from an io.Reader using basic auth.
func (c *RedfishClient) UploadFirmware(fw io.Reader) (*http.Response, error) {
	// Create a new POST request with the file reader as the body
	req, err := http.NewRequest("POST", c.ClientConfig.Endpoint+"/redfish/v1/UpdateService", fw)
	if err != nil {
		return nil, err
	}

	// Set headers
	req.Header.Set("Content-Type", "application/octet-stream")

	// Set basic authentication
	req.SetBasicAuth(c.ClientConfig.Username, c.ClientConfig.Password)

	return c.APIClient.HTTPClient.Do(req)

}

// UpdateFirmware ensures apply time is Immediate then uploads firmware from the reader.
// It first checks the current apply time setting and only attempts to change it if needed.
func (c *RedfishClient) UpdateFirmware(fw io.Reader) (*http.Response, error) {
	// Ensure apply time is set to Immediate (only sets if not already Immediate)
	if err := c.EnsureHttpPushUriApplyTimeImmediate(); err != nil {
		return nil, err
	}

	return c.UploadFirmware(fw)
}

// UpdateFirmwareByPath opens a local file and performs UpdateFirmware.
func (c *RedfishClient) UpdateFirmwareByPath(firmwarePath string) (*http.Response, error) {
	// Open the firmware file
	firmwareFile, err := os.Open(firmwarePath)
	if err != nil {
		return nil, err
	}
	defer firmwareFile.Close()

	return c.UpdateFirmware(firmwareFile)
}

// TaskResponse represents a Redfish task response
type TaskResponse struct {
	TaskURI string `json:"@odata.id"`
}

// GetTaskURI extracts task URI from firmware update response
func (c *RedfishClient) GetTaskURI(resp *http.Response) (string, error) {
	if resp == nil {
		return "", errors.New("nil response")
	}
	defer resp.Body.Close()

	body, err := ioutil.ReadAll(resp.Body)
	if err != nil {
		return "", err
	}

	// Parse the response to find the task URI
	var taskData map[string]interface{}
	if err := json.Unmarshal(body, &taskData); err != nil {
		return "", err
	}

	// Look for task URI in various possible locations
	if id, ok := taskData["@odata.id"].(string); ok {
		return id, nil
	}

	// Check for task monitor header
	if taskMonitor := resp.Header.Get("Location"); taskMonitor != "" {
		return taskMonitor, nil
	}

	return "", errors.New("could not find task URI in response")
}

// GetTaskStatus queries the status of a task
func (c *RedfishClient) GetTaskStatus(taskURI string) (string, int, error) {
	fullURI := c.ClientConfig.Endpoint + taskURI

	req, err := http.NewRequest("GET", fullURI, nil)
	if err != nil {
		return "", 0, err
	}

	req.SetBasicAuth(c.ClientConfig.Username, c.ClientConfig.Password)

	resp, err := c.APIClient.HTTPClient.Do(req)
	if err != nil {
		return "", 0, err
	}
	defer resp.Body.Close()

	body, err := ioutil.ReadAll(resp.Body)
	if err != nil {
		return "", 0, err
	}

	var taskData map[string]interface{}
	if err := json.Unmarshal(body, &taskData); err != nil {
		return "", 0, err
	}

	state := ""
	percentComplete := 0

	if s, ok := taskData["TaskState"].(string); ok {
		state = s
	}

	if p, ok := taskData["PercentComplete"].(float64); ok {
		percentComplete = int(p)
	}

	return state, percentComplete, nil
}
