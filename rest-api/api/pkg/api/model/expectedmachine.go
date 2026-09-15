// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"errors"
	"time"

	"github.com/google/uuid"

	validation "github.com/go-ozzo/ozzo-validation/v4"
	validationis "github.com/go-ozzo/ozzo-validation/v4/is"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model/util"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

const (
	// ExpectedMachineMaxBatchItems is the maximum number of ExpectedMachines allowed in a single batch operation
	ExpectedMachineMaxBatchItems = 100
)

// APIHostLifecycleProfile captures per-host lifecycle settings that affect how
// NICo progresses a host through its state machine.
type APIHostLifecycleProfile struct {
	// DisableLockdown, when true, skips locking down the server during host
	// lifecycle management. When omitted, the existing value is preserved.
	DisableLockdown *bool `json:"disableLockdown"`
}

// ToDBModel converts the API profile into its DB model form. A nil receiver
// maps to the zero-value profile (no setting present).
func (p *APIHostLifecycleProfile) ToDBModel() cdbm.HostLifecycleProfile {
	if p == nil {
		return cdbm.HostLifecycleProfile{}
	}
	return cdbm.HostLifecycleProfile{DisableLockdown: p.DisableLockdown}
}

// ToDBModelPtr converts the API profile into a DB model pointer, returning nil
// when the update request did not set a meaningful field.
func (p *APIHostLifecycleProfile) ToDBModelPtr() *cdbm.HostLifecycleProfile {
	if p == nil {
		return nil
	}
	v := p.ToDBModel()
	if !v.HasSetFields() {
		return nil
	}
	return &v
}

// NewAPIHostLifecycleProfile builds the API profile from its DB model form,
// returning nil when no setting is present so the field is omitted in responses.
func NewAPIHostLifecycleProfile(p cdbm.HostLifecycleProfile) *APIHostLifecycleProfile {
	if p.DisableLockdown == nil {
		return nil
	}
	return &APIHostLifecycleProfile{DisableLockdown: p.DisableLockdown}
}

// APIExpectedMachineCreateRequest is the data structure to capture instance request to create a new ExpectedMachine
type APIExpectedMachineCreateRequest struct {
	// SiteID is the ID of the Site
	SiteID string `json:"siteId"`
	// BmcMacAddress is the MAC address of the expected machine's BMC
	BmcMacAddress string `json:"bmcMacAddress"`
	// BmcUsername is the username of the expected machine's BMC
	DefaultBmcUsername *string `json:"defaultBmcUsername"`
	// DefaultBmcPassword is the password of the expected machine's BMC
	DefaultBmcPassword *string `json:"defaultBmcPassword"`
	// ChassisSerialNumber is the serial number of the expected machine's chassis
	ChassisSerialNumber string `json:"chassisSerialNumber"`
	// FallbackDPUSerialNumbers is the serial numbers of the expected machine's fallback DPUs
	FallbackDPUSerialNumbers []string `json:"fallbackDPUSerialNumbers"`
	// SkuId is the optional UUID for an SKU
	SkuID *string `json:"skuId"`
	// RackID is the optional rack identifier
	RackID *string `json:"rackId"`
	// BmcIpAddress is the optional BMC IP address of the expected machine
	BmcIpAddress *string `json:"bmcIpAddress"`
	// Name is the optional name of the expected machine
	Name *string `json:"name"`
	// Manufacturer is the optional manufacturer of the expected machine
	Manufacturer *string `json:"manufacturer"`
	// Model is the optional model of the expected machine
	Model *string `json:"model"`
	// Description is the optional description of the expected machine
	Description *string `json:"description"`
	// SlotID is the optional slot identifier
	SlotID *int32 `json:"slotId"`
	// TrayIdx is the optional tray index
	TrayIdx *int32 `json:"trayIdx"`
	// HostID is the optional host identifier
	HostID *int32 `json:"hostId"`
	// IsDpfEnabled marks whether this host is eligible for DPF-based provisioning
	IsDpfEnabled *bool `json:"isDpfEnabled"`
	// Labels is the labels of the expected machine
	Labels map[string]string `json:"labels"`
	// HostLifecycleProfile is the optional per-host lifecycle profile
	HostLifecycleProfile *APIHostLifecycleProfile `json:"hostLifecycleProfile"`
}

// Validate ensure the values passed in request are acceptable
func (emcr *APIExpectedMachineCreateRequest) Validate() error {
	err := validation.ValidateStruct(emcr,
		validation.Field(&emcr.SiteID,
			validation.When(emcr.SiteID != "", validationis.UUID.Error(validationErrorInvalidUUID))),
		validation.Field(&emcr.BmcMacAddress,
			validation.Required.Error(validationErrorValueRequired),
			validationis.MAC),
		validation.Field(&emcr.DefaultBmcUsername,
			validation.Length(0, 16).Error("BMC username must be 16 characters or less")),
		validation.Field(&emcr.DefaultBmcPassword,
			validation.Length(0, 20).Error("BMC password must be 20 characters or less")),
		validation.Field(&emcr.ChassisSerialNumber,
			validation.Required.Error(validationErrorValueRequired),
			validation.Match(util.NotAllWhitespaceRegexp).Error("Chassis serial number consists only of whitespace"),
			validation.Length(1, 32).Error("Chassis serial number must be 32 characters or less")),
		validation.Field(&emcr.SkuID,
			validation.NilOrNotEmpty.Error("SkuID cannot be empty")),
		validation.Field(&emcr.RackID,
			validation.NilOrNotEmpty.Error("RackID cannot be empty")),
		validation.Field(&emcr.BmcIpAddress,
			validation.NilOrNotEmpty.Error("BmcIpAddress cannot be empty"),
			validation.When(emcr.BmcIpAddress != nil && *emcr.BmcIpAddress != "",
				validationis.IP.Error("BmcIpAddress must be a valid IPv4 or IPv6 address"))),
		validation.Field(&emcr.Name,
			validation.NilOrNotEmpty.Error("Name cannot be empty")),
		validation.Field(&emcr.Manufacturer,
			validation.NilOrNotEmpty.Error("Manufacturer cannot be empty")),
		validation.Field(&emcr.Model,
			validation.NilOrNotEmpty.Error("Model cannot be empty")),
		validation.Field(&emcr.Description,
			validation.NilOrNotEmpty.Error("Description cannot be empty")),
	)

	if err != nil {
		return err
	}

	if err := util.ValidateLabels(emcr.Labels); err != nil {
		return err
	}

	return nil
}

// APIExpectedMachineUpdateRequest is the data structure to capture user request to update an ExpectedMachine
type APIExpectedMachineUpdateRequest struct {
	// ID is required for batch updates (must be empty or match path value for single update)
	ID *string `json:"id"`
	// BmcMacAddress may reassert the ExpectedMachine's current BMC MAC, but
	// cannot change it after creation.
	BmcMacAddress *string `json:"bmcMacAddress"`
	// BmcUsername is the username of the expected machine's BMC
	DefaultBmcUsername *string `json:"defaultBmcUsername"`
	// DefaultBmcPassword is the password of the expected machine's BMC
	DefaultBmcPassword *string `json:"defaultBmcPassword"`
	// ChassisSerialNumber is the serial number of the expected machine's chassis
	ChassisSerialNumber *string `json:"chassisSerialNumber"`
	// FallbackDPUSerialNumbers is the serial numbers of the expected machine's fallback DPUs
	FallbackDPUSerialNumbers []string `json:"fallbackDPUSerialNumbers"`
	// SkuId is the optional UUID for an SKU
	SkuID *string `json:"skuId"`
	// RackID is the optional rack identifier
	RackID *string `json:"rackId"`
	// BmcIpAddress is the optional BMC IP address of the expected machine.
	// An empty string is the PATCH clear sentinel; nil preserves the stored value.
	BmcIpAddress *string `json:"bmcIpAddress"`
	// Name is the optional name of the expected machine
	Name *string `json:"name"`
	// Manufacturer is the optional manufacturer of the expected machine
	Manufacturer *string `json:"manufacturer"`
	// Model is the optional model of the expected machine
	Model *string `json:"model"`
	// Description is the optional description of the expected machine
	Description *string `json:"description"`
	// SlotID is the optional slot identifier
	SlotID *int32 `json:"slotId"`
	// TrayIdx is the optional tray index
	TrayIdx *int32 `json:"trayIdx"`
	// HostID is the optional host identifier
	HostID *int32 `json:"hostId"`
	// IsDpfEnabled marks whether this host is eligible for DPF-based provisioning
	IsDpfEnabled *bool `json:"isDpfEnabled"`
	// Labels is the labels of the expected machine
	Labels map[string]string `json:"labels"`
	// HostLifecycleProfile is the optional per-host lifecycle profile
	HostLifecycleProfile *APIHostLifecycleProfile `json:"hostLifecycleProfile"`
}

// Validate ensure the values passed in request are acceptable
func (emur *APIExpectedMachineUpdateRequest) Validate() error {
	credentialErr := validateExpectedComponentCredentialPair(emur.DefaultBmcUsername, emur.DefaultBmcPassword, "defaultBmcUsername", "defaultBmcPassword")
	if credentialErr != nil {
		return credentialErr
	}

	if emur.ID != nil {
		if *emur.ID == "" {
			return validation.Errors{
				"id": errors.New("ID cannot be empty"),
			}
		}
		if _, err := uuid.Parse(*emur.ID); err != nil {
			return validation.Errors{
				"id": errors.New("ID must be a valid UUID"),
			}
		}
	}

	err := validation.ValidateStruct(emur,
		validation.Field(&emur.BmcMacAddress,
			validation.NilOrNotEmpty.Error("BmcMacAddress cannot be empty"),
			validation.When(emur.BmcMacAddress != nil && *emur.BmcMacAddress != "",
				validationis.MAC)),
		validation.Field(&emur.DefaultBmcUsername,
			validation.NilOrNotEmpty.Error("BMC Username cannot be empty"),
			validation.When(emur.DefaultBmcUsername != nil && *emur.DefaultBmcUsername != "",
				validation.Match(util.NotAllWhitespaceRegexp).Error("BMC Username consists only of whitespace")),
			validation.Length(1, 16).Error("BMC Username must be 1-16 characters")),
		validation.Field(&emur.DefaultBmcPassword,
			validation.NilOrNotEmpty.Error("BMC Password cannot be empty"),
			validation.When(emur.DefaultBmcPassword != nil && *emur.DefaultBmcPassword != "",
				validation.Match(util.NotAllWhitespaceRegexp).Error("BMC Password consists only of whitespace")),
			validation.Length(1, 20).Error("BMC Password must be 1-20 characters")),
		validation.Field(&emur.ChassisSerialNumber,
			validation.NilOrNotEmpty.Error("Chassis Serial Number cannot be empty"),
			validation.When(emur.ChassisSerialNumber != nil && *emur.ChassisSerialNumber != "",
				validation.Match(util.NotAllWhitespaceRegexp).Error("Chassis Serial Number consists only of whitespace")),
			validation.Length(1, 32).Error("Chassis Serial Number must be 1-32 characters")),
		validation.Field(&emur.SkuID,
			validation.NilOrNotEmpty.Error("SkuID cannot be empty")),
		validation.Field(&emur.RackID,
			validation.NilOrNotEmpty.Error("RackID cannot be empty")),
		validation.Field(&emur.BmcIpAddress,
			validation.When(emur.BmcIpAddress != nil && *emur.BmcIpAddress != "",
				validationis.IP.Error("BmcIpAddress must be a valid IPv4 or IPv6 address"))),
		validation.Field(&emur.Name,
			validation.NilOrNotEmpty.Error("Name cannot be empty")),
		validation.Field(&emur.Manufacturer,
			validation.NilOrNotEmpty.Error("Manufacturer cannot be empty")),
		validation.Field(&emur.Model,
			validation.NilOrNotEmpty.Error("Model cannot be empty")),
		validation.Field(&emur.Description,
			validation.NilOrNotEmpty.Error("Description cannot be empty")),
	)

	if err != nil {
		return err
	}

	if err := util.ValidateLabels(emur.Labels); err != nil {
		return err
	}

	return nil
}

// ToProto builds the Core patch from the updated cloud row and the fields
// selected by this request. Explicit zero and empty values remain updates.
func (emur *APIExpectedMachineUpdateRequest) ToProto(entity *cdbm.ExpectedMachine) *corev1.PatchExpectedMachineRequest {
	resource := entity.ToProto(cdbm.ExpectedMachineCredentials{
		Username: emur.DefaultBmcUsername,
		Password: emur.DefaultBmcPassword,
	})

	// The DB row may still hold an address this PATCH omitted. Core needs
	// the request value so nil preserves its reservation and "" clears it.
	resource.BmcIpAddress = emur.BmcIpAddress
	return &corev1.PatchExpectedMachineRequest{
		ExpectedMachine: resource,
		UpdateMask: expectedComponentUpdateMask(
			expectedComponentUpdateField{"bmc_username", emur.DefaultBmcUsername != nil},
			expectedComponentUpdateField{"bmc_password", emur.DefaultBmcPassword != nil},
			expectedComponentUpdateField{"bmc_ip_address", emur.BmcIpAddress != nil},
			expectedComponentUpdateField{"rack_id", emur.RackID != nil},
			expectedComponentUpdateField{"metadata.name", emur.Name != nil},
			expectedComponentUpdateField{"metadata.description", emur.Description != nil},
			expectedComponentUpdateField{"metadata.labels", emur.Labels != nil || emur.Manufacturer != nil || emur.Model != nil || emur.SlotID != nil || emur.TrayIdx != nil || emur.HostID != nil},
			expectedComponentUpdateField{"chassis_serial_number", emur.ChassisSerialNumber != nil},
			expectedComponentUpdateField{"fallback_dpu_serial_numbers", emur.FallbackDPUSerialNumbers != nil},
			expectedComponentUpdateField{"sku_id", emur.SkuID != nil},
			expectedComponentUpdateField{"is_dpf_enabled", emur.IsDpfEnabled != nil},
			expectedComponentUpdateField{"host_lifecycle_profile.disable_lockdown", emur.HostLifecycleProfile != nil && emur.HostLifecycleProfile.DisableLockdown != nil},
		),
	}
}

// APIExpectedMachine is the data structure to capture API representation of an ExpectedMachine
type APIExpectedMachine struct {
	// ID is the ID of this Expected Machine
	ID uuid.UUID `json:"id"`
	// BmcMacAddress is the MAC address of the expected machine's BMC
	BmcMacAddress string `json:"bmcMacAddress"`
	// SiteID is the ID of the site this machine belongs to
	SiteID uuid.UUID `json:"siteId"`
	// Site is the site information
	Site *APISite `json:"site,omitempty"`
	// ChassisSerialNumber is the serial number of the expected machine's chassis
	ChassisSerialNumber string `json:"chassisSerialNumber"`
	// FallbackDPUSerialNumbers is the serial numbers of the expected machine's fallback DPUs
	FallbackDPUSerialNumbers APIList[string] `json:"fallbackDPUSerialNumbers"`
	// SkuID is the ID of the SKU
	SkuID *string `json:"skuId"`
	// Sku is the SKU information
	Sku *APISku `json:"sku,omitempty"`
	// MachineID is the ID of the Machine associated with this Expected Machine
	MachineID *string `json:"machineId"`
	// Machine is the optional Machine information associated with this Expected Machine
	Machine *APIMachineSummary `json:"machine,omitempty"`
	// RackID is the optional rack identifier
	RackID *string `json:"rackId"`
	// BmcIpAddress is the optional BMC IP address of the expected machine
	BmcIpAddress *string `json:"bmcIpAddress"`
	// Name is the optional name of the expected machine
	Name *string `json:"name"`
	// Manufacturer is the optional manufacturer of the expected machine
	Manufacturer *string `json:"manufacturer"`
	// Model is the optional model of the expected machine
	Model *string `json:"model"`
	// Description is the optional description of the expected machine
	Description *string `json:"description"`
	// SlotID is the optional slot identifier
	SlotID *int32 `json:"slotId"`
	// TrayIdx is the optional tray index
	TrayIdx *int32 `json:"trayIdx"`
	// HostID is the optional host identifier
	HostID *int32 `json:"hostId"`
	// IsDpfEnabled indicates whether this host is eligible for DPF-based provisioning
	IsDpfEnabled *bool `json:"isDpfEnabled"`
	// Labels is the labels of the expected machine
	Labels APILabels `json:"labels"`
	// HostLifecycleProfile is the optional per-host lifecycle profile
	HostLifecycleProfile *APIHostLifecycleProfile `json:"hostLifecycleProfile,omitempty"`
	// Created indicates the ISO datetime string for when the ExpectedMachine was created
	Created time.Time `json:"created"`
	// Updated indicates the ISO datetime string for when the ExpectedMachine was last updated
	Updated time.Time `json:"updated"`
}

// NewAPIExpectedMachine accepts a DB layer ExpectedMachine object and returns an API object
func NewAPIExpectedMachine(dibp *cdbm.ExpectedMachine) *APIExpectedMachine {
	apiem := &APIExpectedMachine{
		ID:                       dibp.ID,
		BmcMacAddress:            dibp.BmcMacAddress,
		SiteID:                   dibp.SiteID,
		ChassisSerialNumber:      dibp.ChassisSerialNumber,
		FallbackDPUSerialNumbers: dibp.FallbackDpuSerialNumbers,
		SkuID:                    dibp.SkuID,
		MachineID:                dibp.MachineID,
		RackID:                   dibp.RackID,
		BmcIpAddress:             dibp.BmcIpAddress,
		Name:                     dibp.Name,
		Manufacturer:             dibp.Manufacturer,
		Model:                    dibp.Model,
		Description:              dibp.Description,
		SlotID:                   dibp.SlotID,
		TrayIdx:                  dibp.TrayIdx,
		HostID:                   dibp.HostID,
		IsDpfEnabled:             dibp.IsDpfEnabled,
		Labels:                   APILabels(dibp.Labels),
		HostLifecycleProfile:     NewAPIHostLifecycleProfile(dibp.HostLifecycleProfile),
		Created:                  dibp.Created,
		Updated:                  dibp.Updated,
	}

	// Expand Site details if available
	if dibp.Site != nil {
		site := NewAPISite(*dibp.Site, []cdbm.StatusDetail{}, nil)
		apiem.Site = &site
	}

	// Expand SKU details if available
	if dibp.Sku != nil {
		sku := NewAPISku(dibp.Sku)
		apiem.Sku = sku
	}

	// Expand Machine details if available
	if dibp.Machine != nil {
		machine := NewAPIMachineSummary(dibp.Machine)
		apiem.Machine = machine
	}

	return apiem
}
