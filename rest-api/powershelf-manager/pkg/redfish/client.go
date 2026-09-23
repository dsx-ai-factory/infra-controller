// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package redfish wraps gofish to provide service-focused Redfish operations (inventory, power control, and firmware upload)
// with minimal coupling to underlying transport details.
package redfish

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"io/ioutil"
	"net"
	"net/http"

	"github.com/NVIDIA/infra-controller/rest-api/powershelf-manager/pkg/objects/pmc"
	"github.com/NVIDIA/infra-controller/rest-api/powershelf-manager/pkg/objects/powershelf"
	"github.com/NVIDIA/infra-controller/rest-api/powershelf-manager/pkg/objects/powersupply"

	log "github.com/sirupsen/logrus"

	"os"

	"github.com/stmcginnis/gofish"
	"github.com/stmcginnis/gofish/redfish"
)

// checkResponse returns an error if the HTTP status code indicates failure (>= 300),
// including the response body in the error message for diagnostics.
func checkResponse(resp *http.Response) error {
	defer resp.Body.Close()

	if resp.StatusCode < http.StatusMultipleChoices {
		return nil
	}

	body, readErr := io.ReadAll(resp.Body)
	if readErr != nil {
		return fmt.Errorf("HTTP %d %s (could not read response body: %v)", resp.StatusCode, resp.Status, readErr)
	}
	return fmt.Errorf("HTTP %d %s: %s", resp.StatusCode, resp.Status, string(body))
}

// RedfishClient manages a gofish API client and PMC context to perform typed Redfish operations against a device.
type RedfishClient struct {
	pmc *pmc.PMC
	*gofish.APIClient
	gofish.ClientConfig
}

func buildEndpoint(ip net.IP) string {
	port := "443"
	// TODO: remove this--hack for running the service from my macbook
	if ip.String() == "127.0.0.1" {
		port = "8443"
	}
	return "https://" + net.JoinHostPort(ip.String(), port)
}

// New creates a RedfishClient for the given PMC and context.
func New(ctx context.Context, pmc *pmc.PMC, reuse_connections bool) (*RedfishClient, error) {
	client_config := gofish.ClientConfig{
		Endpoint:         buildEndpoint(pmc.IP),
		Username:         pmc.Credential.User,
		Password:         pmc.Credential.Password.Value,
		Insecure:         true,
		ReuseConnections: reuse_connections,
	}

	client, err := gofish.ConnectContext(ctx, client_config)
	if err != nil {
		return nil, err
	}

	return &RedfishClient{pmc: pmc, APIClient: client, ClientConfig: client_config}, nil
}

// QueryPowerState returns the power state of the shelf.
func (c *RedfishClient) QueryPowerState() (redfish.PowerState, error) {
	chassis, err := c.QueryChassis()
	if err != nil {
		return redfish.OffPowerState, err
	}

	return chassis.PowerState, nil
}

// QueryChassis fetches the powershelf chassis or returns an error if not found.
func (c *RedfishClient) QueryChassis() (*redfish.Chassis, error) {
	chassis, err := c.Service.Chassis()
	if err != nil {
		return nil, err
	}

	for _, ch := range chassis {
		if ch.ID == "powershelf" {
			return ch, nil
		}
	}

	return nil, errors.New("could not find a powershelf chassis subsystem")
}

// QueryManager fetches the PMC manager or returns an error if not found.
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

	return nil, errors.New("could not find the pmc manager")
}

// Power OFF the shelf
func (c *RedfishClient) PowerOff() (*http.Response, error) {
	uri := "/redfish/v1/Chassis/powershelf/Actions/Chassis.ForceOff"
	body := map[string]interface{}{
		"ForceOffType": "ForceOff",
	}

	log.Printf("powering off... uri %v body: %v \n", uri, body)
	return c.Post(uri, body)
}

// Power ON the shelf
func (c *RedfishClient) PowerOn() (*http.Response, error) {
	uri := "/redfish/v1/Chassis/powershelf/Actions/Chassis.On"
	body := map[string]interface{}{
		"OnType": "On",
	}
	return c.Post(uri, body)
}

// ResetChassis resets the chassis
func (c *RedfishClient) ResetChassis() (*http.Response, error) {
	uri := "/redfish/v1/Chassis/powershelf/Actions/Chassis.Reset"
	body := map[string]interface{}{
		"ResetType": "Reset",
	}
	return c.Post(uri, body)
}

type ResetPmcType string

const (
	GracefulRestart ResetPmcType = "GracefulRestart"
	ForceRestart    ResetPmcType = "ForceRestart"
)

// ResetPmc resets the manager (PMC).
func (c *RedfishClient) ResetPmc(resetType ResetPmcType) (*http.Response, error) {
	uri := "/redfish/v1/Managers/bmc/Actions/Manager.Reset"
	body := map[string]interface{}{
		"ResetType": resetType,
	}
	return c.Post(uri, body)
}

// FactoryResetPmc resets manager settings to defaults.
func (c *RedfishClient) FactoryResetPmc() (*http.Response, error) {
	uri := "/redfish/v1/Managers/bmc/Actions/Manager.ResetToDefaults"
	body := map[string]interface{}{
		"ResetType": "ResetAll",
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

// UploadFirmware uploads firmware from an io.Reader via the Redfish UpdateService.
func (c *RedfishClient) UploadFirmware(fw io.Reader) (*http.Response, error) {
	req, err := http.NewRequest("POST", c.ClientConfig.Endpoint+"/redfish/v1/UpdateService", fw)
	if err != nil {
		return nil, err
	}

	req.Header.Set("Content-Type", "application/octet-stream")
	req.SetBasicAuth(c.ClientConfig.Username, c.ClientConfig.Password)

	return c.APIClient.HTTPClient.Do(req)
}

// UpdateFirmware sets apply time to Immediate then uploads firmware from the reader.
func (c *RedfishClient) UpdateFirmware(fw io.Reader) error {
	resp, err := c.SetHttpPushUriApplyTimeImmediate()
	if err != nil {
		return fmt.Errorf("failed to set apply time to Immediate: %w", err)
	}

	err = checkResponse(resp)
	if err != nil {
		return fmt.Errorf("failed to set apply time to Immediate: %w", err)
	}

	resp, err = c.UploadFirmware(fw)
	if err != nil {
		return fmt.Errorf("failed to upload firmware: %w", err)
	}

	return checkResponse(resp)
}

// QueryPowerSubsystem returns the chassis PowerSubsystem resource.
func (c *RedfishClient) QueryPowerSubsystem() (*redfish.PowerSubsystem, error) {
	chassis, err := c.QueryChassis()
	if err != nil {
		return nil, err
	}

	return chassis.PowerSubsystem()
}

// QueryPowerSupply fetches and hydrates a PowerSupply by URI, including sensors.
func (c *RedfishClient) QueryPowerSupply(uri string) (*powersupply.PowerSupply, error) {
	// Ensure the full URL is used
	fullURI := fmt.Sprintf("%s%s", c.ClientConfig.Endpoint, uri)

	powerSupply := new(powersupply.PowerSupply)

	// Create a new POST request with the file reader as the body
	req, err := http.NewRequest("GET", fullURI, nil)
	if err != nil {
		return nil, err
	}

	// Set basic authentication
	req.SetBasicAuth(c.ClientConfig.Username, c.ClientConfig.Password)

	resp, err := c.APIClient.HTTPClient.Do(req)
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()

	// Read the response body
	body, err := ioutil.ReadAll(resp.Body)
	if err != nil {
		return nil, err
	}

	// Decode the JSON response into the PowerSupply struct
	err = json.NewDecoder(bytes.NewReader(body)).Decode(&powerSupply)
	if err != nil {
		return nil, err
	}

	var sensors []*redfish.Sensor
	for _, sensorRef := range powerSupply.Sensors {
		var sensor redfish.Sensor
		err := c.Service.Get(c.Service.GetClient(), sensorRef.ODataID, &sensor)
		if err != nil {
			return nil, err
		}
		sensors = append(sensors, &sensor)
	}
	powerSupply.Sensors = sensors

	return powerSupply, nil
}

// QueryPowerSupplies enumerates and hydrates all power supplies.
func (c *RedfishClient) QueryPowerSupplies() ([]*powersupply.PowerSupply, error) {
	power_subsystem, err := c.QueryPowerSubsystem()
	if err != nil {
		return nil, err
	}

	psus, err := power_subsystem.PowerSupplies()
	if err != nil {
		return nil, err
	}

	ret_psus := make([]*powersupply.PowerSupply, 0, len(psus))

	for _, psu := range psus {
		uri := psu.ODataID
		powerSupply, err := c.QueryPowerSupply(uri)
		if err != nil {
			return nil, err
		}
		ret_psus = append(ret_psus, powerSupply)
	}

	return ret_psus, nil
}

// QueryPowerShelf aggregates the PMC, chassis, manager, and power supplies into a single view.
func (c *RedfishClient) QueryPowerShelf() (*powershelf.PowerShelf, error) {
	powershelf := &powershelf.PowerShelf{
		PMC: c.pmc,
	}

	chassis, err := c.QueryChassis()
	if err != nil {
		return nil, err
	}

	powershelf.Chassis = chassis

	manager, err := c.QueryManager()
	if err != nil {
		return nil, err
	}
	powershelf.Manager = manager

	psus, err := c.QueryPowerSupplies()
	if err != nil {
		return nil, err
	}
	powershelf.PowerSupplies = psus

	return powershelf, nil
}
