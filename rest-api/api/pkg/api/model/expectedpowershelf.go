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
)

// APIExpectedPowerShelfCreateRequest is the data structure to capture request to create a new ExpectedPowerShelf
type APIExpectedPowerShelfCreateRequest struct {
	// SiteID is the ID of the Site
	SiteID string `json:"siteId"`
	// BmcMacAddress is the MAC address of the expected power shelf's BMC
	BmcMacAddress string `json:"bmcMacAddress"`
	// DefaultBmcUsername is the username of the expected power shelf's BMC
	DefaultBmcUsername *string `json:"defaultBmcUsername"`
	// DefaultBmcPassword is the password of the expected power shelf's BMC
	DefaultBmcPassword *string `json:"defaultBmcPassword"`
	// ShelfSerialNumber is the serial number of the expected power shelf
	ShelfSerialNumber string `json:"shelfSerialNumber"`
	// BmcIpAddress is the BMC IP address of the expected power shelf
	BmcIpAddress *string `json:"bmcIpAddress"`
	// RackID is the optional rack identifier
	RackID *string `json:"rackId"`
	// Name is the optional name of the expected power shelf
	Name *string `json:"name"`
	// Manufacturer is the optional manufacturer of the expected power shelf
	Manufacturer *string `json:"manufacturer"`
	// Model is the optional model of the expected power shelf
	Model *string `json:"model"`
	// Description is the optional description of the expected power shelf
	Description *string `json:"description"`
	// SlotID is the optional slot identifier
	SlotID *int32 `json:"slotId"`
	// TrayIdx is the optional tray index
	TrayIdx *int32 `json:"trayIdx"`
	// HostID is the optional host identifier
	HostID *int32 `json:"hostId"`
	// Labels is the labels of the expected power shelf
	Labels map[string]string `json:"labels"`
}

// Validate ensure the values passed in request are acceptable
func (epcr *APIExpectedPowerShelfCreateRequest) Validate() error {
	err := validation.ValidateStruct(epcr,
		validation.Field(&epcr.SiteID,
			validation.Required.Error(validationErrorValueRequired),
			validationis.UUID.Error(validationErrorInvalidUUID)),
		validation.Field(&epcr.BmcMacAddress,
			validation.Required.Error(validationErrorValueRequired),
			validationis.MAC),
		validation.Field(&epcr.DefaultBmcUsername,
			validation.Length(0, 16).Error("BMC username must be 16 characters or less")),
		validation.Field(&epcr.DefaultBmcPassword,
			validation.Length(0, 20).Error("BMC password must be 20 characters or less")),
		validation.Field(&epcr.ShelfSerialNumber,
			validation.Required.Error(validationErrorValueRequired),
			validation.Match(util.NotAllWhitespaceRegexp).Error("Shelf serial number consists only of whitespace"),
			validation.Length(1, 32).Error("Shelf serial number must be 32 characters or less")),
		validation.Field(&epcr.RackID,
			validation.NilOrNotEmpty.Error("RackID cannot be empty")),
		validation.Field(&epcr.BmcIpAddress,
			validation.NilOrNotEmpty.Error("BmcIpAddress cannot be empty"),
			validation.When(epcr.BmcIpAddress != nil && *epcr.BmcIpAddress != "",
				validationis.IP.Error("BmcIpAddress must be a valid IPv4 or IPv6 address"))),
		validation.Field(&epcr.Name,
			validation.NilOrNotEmpty.Error("Name cannot be empty")),
		validation.Field(&epcr.Manufacturer,
			validation.NilOrNotEmpty.Error("Manufacturer cannot be empty")),
		validation.Field(&epcr.Model,
			validation.NilOrNotEmpty.Error("Model cannot be empty")),
		validation.Field(&epcr.Description,
			validation.NilOrNotEmpty.Error("Description cannot be empty")),
	)

	if err != nil {
		return err
	}

	if err := util.ValidateLabels(epcr.Labels); err != nil {
		return err
	}

	return nil
}

// APIExpectedPowerShelfUpdateRequest is the data structure to capture user request to update an ExpectedPowerShelf
type APIExpectedPowerShelfUpdateRequest struct {
	// ID is required for batch updates (must be empty or match path value for single update)
	ID *string `json:"id"`
	// BmcMacAddress is the MAC address of the expected power shelf's BMC
	BmcMacAddress *string `json:"bmcMacAddress"`
	// DefaultBmcUsername is the username of the expected power shelf's BMC
	DefaultBmcUsername *string `json:"defaultBmcUsername"`
	// DefaultBmcPassword is the password of the expected power shelf's BMC
	DefaultBmcPassword *string `json:"defaultBmcPassword"`
	// ShelfSerialNumber is the serial number of the expected power shelf
	ShelfSerialNumber *string `json:"shelfSerialNumber"`
	// BmcIpAddress is the BMC IP address of the expected power shelf
	BmcIpAddress *string `json:"bmcIpAddress"`
	// RackID is the optional rack identifier
	RackID *string `json:"rackId"`
	// Name is the optional name of the expected power shelf
	Name *string `json:"name"`
	// Manufacturer is the optional manufacturer of the expected power shelf
	Manufacturer *string `json:"manufacturer"`
	// Model is the optional model of the expected power shelf
	Model *string `json:"model"`
	// Description is the optional description of the expected power shelf
	Description *string `json:"description"`
	// SlotID is the optional slot identifier
	SlotID *int32 `json:"slotId"`
	// TrayIdx is the optional tray index
	TrayIdx *int32 `json:"trayIdx"`
	// HostID is the optional host identifier
	HostID *int32 `json:"hostId"`
	// Labels is the labels of the expected power shelf
	Labels map[string]string `json:"labels"`
}

// Validate ensure the values passed in request are acceptable
func (epur *APIExpectedPowerShelfUpdateRequest) Validate() error {
	if epur.ID != nil {
		if *epur.ID == "" {
			return validation.Errors{
				"id": errors.New("ID cannot be empty"),
			}
		}
		if _, err := uuid.Parse(*epur.ID); err != nil {
			return validation.Errors{
				"id": errors.New("ID must be a valid UUID"),
			}
		}
	}

	err := validation.ValidateStruct(epur,
		validation.Field(&epur.DefaultBmcUsername,
			validation.NilOrNotEmpty.Error("BMC Username cannot be empty"),
			validation.When(epur.DefaultBmcUsername != nil && *epur.DefaultBmcUsername != "",
				validation.Match(util.NotAllWhitespaceRegexp).Error("BMC Username consists only of whitespace")),
			validation.Length(1, 16).Error("BMC Username must be 1-16 characters")),
		validation.Field(&epur.DefaultBmcPassword,
			validation.NilOrNotEmpty.Error("BMC Password cannot be empty"),
			validation.When(epur.DefaultBmcPassword != nil && *epur.DefaultBmcPassword != "",
				validation.Match(util.NotAllWhitespaceRegexp).Error("BMC Password consists only of whitespace")),
			validation.Length(1, 20).Error("BMC Password must be 1-20 characters")),
		validation.Field(&epur.ShelfSerialNumber,
			validation.NilOrNotEmpty.Error("Shelf Serial Number cannot be empty"),
			validation.When(epur.ShelfSerialNumber != nil && *epur.ShelfSerialNumber != "",
				validation.Match(util.NotAllWhitespaceRegexp).Error("Shelf Serial Number consists only of whitespace")),
			validation.Length(1, 32).Error("Shelf Serial Number must be 1-32 characters")),
		validation.Field(&epur.RackID,
			validation.NilOrNotEmpty.Error("RackID cannot be empty")),
		validation.Field(&epur.BmcIpAddress,
			validation.NilOrNotEmpty.Error("BmcIpAddress cannot be empty"),
			validation.When(epur.BmcIpAddress != nil && *epur.BmcIpAddress != "",
				validationis.IP.Error("BmcIpAddress must be a valid IPv4 or IPv6 address"))),
		validation.Field(&epur.Name,
			validation.NilOrNotEmpty.Error("Name cannot be empty")),
		validation.Field(&epur.Manufacturer,
			validation.NilOrNotEmpty.Error("Manufacturer cannot be empty")),
		validation.Field(&epur.Model,
			validation.NilOrNotEmpty.Error("Model cannot be empty")),
		validation.Field(&epur.Description,
			validation.NilOrNotEmpty.Error("Description cannot be empty")),
	)

	if err != nil {
		return err
	}

	if err := util.ValidateLabels(epur.Labels); err != nil {
		return err
	}

	return nil
}

// APIExpectedPowerShelf is the data structure to capture API representation of an ExpectedPowerShelf
type APIExpectedPowerShelf struct {
	// ID is the ID of this Expected Power Shelf
	ID uuid.UUID `json:"id"`
	// BmcMacAddress is the MAC address of the expected power shelf's BMC
	BmcMacAddress string `json:"bmcMacAddress"`
	// SiteID is the ID of the site this power shelf belongs to
	SiteID uuid.UUID `json:"siteId"`
	// Site is the site information
	Site *APISite `json:"site,omitempty"`
	// ShelfSerialNumber is the serial number of the expected power shelf
	ShelfSerialNumber string `json:"shelfSerialNumber"`
	// BmcIpAddress is the BMC IP address of the expected power shelf
	BmcIpAddress *string `json:"bmcIpAddress"`
	// RackID is the optional rack identifier
	RackID *string `json:"rackId"`
	// Name is the optional name of the expected power shelf
	Name *string `json:"name"`
	// Manufacturer is the optional manufacturer of the expected power shelf
	Manufacturer *string `json:"manufacturer"`
	// Model is the optional model of the expected power shelf
	Model *string `json:"model"`
	// Description is the optional description of the expected power shelf
	Description *string `json:"description"`
	// SlotID is the optional slot identifier
	SlotID *int32 `json:"slotId"`
	// TrayIdx is the optional tray index
	TrayIdx *int32 `json:"trayIdx"`
	// HostID is the optional host identifier
	HostID *int32 `json:"hostId"`
	// Labels is the labels of the expected power shelf
	Labels APILabels `json:"labels"`
	// Created indicates the ISO datetime string for when the ExpectedPowerShelf was created
	Created time.Time `json:"created"`
	// Updated indicates the ISO datetime string for when the ExpectedPowerShelf was last updated
	Updated time.Time `json:"updated"`
}

// NewAPIExpectedPowerShelf accepts a DB layer ExpectedPowerShelf object and returns an API object
func NewAPIExpectedPowerShelf(dbModel *cdbm.ExpectedPowerShelf) *APIExpectedPowerShelf {
	apieps := &APIExpectedPowerShelf{
		ID:                dbModel.ID,
		BmcMacAddress:     dbModel.BmcMacAddress,
		SiteID:            dbModel.SiteID,
		ShelfSerialNumber: dbModel.ShelfSerialNumber,
		BmcIpAddress:      dbModel.BmcIpAddress,
		RackID:            dbModel.RackID,
		Name:              dbModel.Name,
		Manufacturer:      dbModel.Manufacturer,
		Model:             dbModel.Model,
		Description:       dbModel.Description,
		SlotID:            dbModel.SlotID,
		TrayIdx:           dbModel.TrayIdx,
		HostID:            dbModel.HostID,
		Labels:            APILabels(dbModel.Labels),
		Created:           dbModel.Created,
		Updated:           dbModel.Updated,
	}

	// Expand Site details if available
	if dbModel.Site != nil {
		site := NewAPISite(*dbModel.Site, []cdbm.StatusDetail{}, nil)
		apieps.Site = &site
	}

	return apieps
}
