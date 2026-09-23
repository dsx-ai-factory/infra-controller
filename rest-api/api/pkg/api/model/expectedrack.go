// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"errors"
	"fmt"
	"time"

	"github.com/google/uuid"

	validation "github.com/go-ozzo/ozzo-validation/v4"
	validationis "github.com/go-ozzo/ozzo-validation/v4/is"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model/util"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
)

// APIExpectedRackCreateRequest is the data structure to capture request to create a new ExpectedRack
type APIExpectedRackCreateRequest struct {
	// SiteID is the ID of the Site the rack belongs to
	SiteID string `json:"siteId"`
	// RackID is the operator-supplied identifier for the rack (string, not UUID).
	// Unique per Site.
	RackID string `json:"rackId"`
	// RackProfileID identifies the rack profile this rack conforms to
	RackProfileID string `json:"rackProfileId"`
	// Name is the optional human-readable name of the expected rack
	Name *string `json:"name"`
	// Description is the optional human-readable description of the expected rack
	Description *string `json:"description"`
	// Labels carries arbitrary key/value pairs. Well-known keys (chassis.*,
	// location.*) are used to convey chassis identity and physical location.
	Labels map[string]string `json:"labels"`
}

// Validate ensure the values passed in request are acceptable
func (ercr *APIExpectedRackCreateRequest) Validate() error {
	err := validation.ValidateStruct(ercr,
		validation.Field(&ercr.SiteID,
			validation.Required.Error(validationErrorValueRequired),
			validationis.UUID.Error(validationErrorInvalidUUID)),
		validation.Field(&ercr.RackID,
			validation.Required.Error(validationErrorValueRequired),
			validation.Match(util.NotAllWhitespaceRegexp).Error("RackID consists only of whitespace")),
		validation.Field(&ercr.RackProfileID,
			validation.Required.Error(validationErrorValueRequired),
			validation.Match(util.NotAllWhitespaceRegexp).Error("RackProfileID consists only of whitespace")),
		validation.Field(&ercr.Name,
			validation.NilOrNotEmpty.Error("Name cannot be empty")),
		validation.Field(&ercr.Description,
			validation.NilOrNotEmpty.Error("Description cannot be empty")),
	)

	if err != nil {
		return err
	}

	if err := util.ValidateLabels(ercr.Labels); err != nil {
		return err
	}

	return nil
}

// APIExpectedRackUpdateRequest is the data structure to capture user request to update an ExpectedRack
type APIExpectedRackUpdateRequest struct {
	// ID can be omitted or null for PATCH. A supplied string must match the
	// path UUID in lowercase hyphenated form; an empty string is invalid.
	ID *string `json:"id"`
	// RackID is the operator-supplied rack identifier. It is immutable on
	// update: it may be omitted or set to the existing value, but a changed
	// value is rejected by the handler before any database mutation because
	// Core and Flow use rackId as the identity key.
	RackID *string `json:"rackId"`
	// RackProfileID is the optional new rack profile ID
	RackProfileID *string `json:"rackProfileId"`
	// Name is the optional new human-readable name of the expected rack
	Name *string `json:"name"`
	// Description is the optional new human-readable description of the expected rack
	Description *string `json:"description"`
	// Labels carries arbitrary key/value pairs. Well-known keys (chassis.*,
	// location.*) are used to convey chassis identity and physical location.
	Labels map[string]string `json:"labels"`
}

// Validate ensure the values passed in request are acceptable
func (erur *APIExpectedRackUpdateRequest) Validate() error {
	if erur.ID != nil {
		if *erur.ID == "" {
			return validation.Errors{
				"id": errors.New("ID cannot be empty"),
			}
		}
		if _, err := uuid.Parse(*erur.ID); err != nil {
			return validation.Errors{
				"id": errors.New("ID must be a valid UUID"),
			}
		}
	}

	// Reject empty updates: require at least one mutable field. An update with
	// no fields would still bump the timestamp and trigger a workflow round-trip.
	if erur.RackID == nil && erur.RackProfileID == nil && erur.Name == nil && erur.Description == nil && erur.Labels == nil {
		return validation.Errors{
			"body": errors.New("at least one mutable field must be provided"),
		}
	}

	err := validation.ValidateStruct(erur,
		validation.Field(&erur.RackID,
			validation.NilOrNotEmpty.Error("RackID cannot be empty"),
			validation.When(erur.RackID != nil && *erur.RackID != "",
				validation.Match(util.NotAllWhitespaceRegexp).Error("RackID consists only of whitespace"))),
		validation.Field(&erur.RackProfileID,
			validation.NilOrNotEmpty.Error("RackProfileID cannot be empty"),
			validation.When(erur.RackProfileID != nil && *erur.RackProfileID != "",
				validation.Match(util.NotAllWhitespaceRegexp).Error("RackProfileID consists only of whitespace"))),
		validation.Field(&erur.Name,
			validation.NilOrNotEmpty.Error("Name cannot be empty")),
		validation.Field(&erur.Description,
			validation.NilOrNotEmpty.Error("Description cannot be empty")),
	)

	if err != nil {
		return err
	}

	if err := util.ValidateLabels(erur.Labels); err != nil {
		return err
	}

	return nil
}

// APIExpectedRack is the data structure to capture API representation of an ExpectedRack
type APIExpectedRack struct {
	// ID is the unique identifier (UUID) of the expected rack
	ID uuid.UUID `json:"id"`
	// SiteID is the ID of the Site this rack belongs to
	SiteID uuid.UUID `json:"siteId"`
	// Site is the site information
	Site *APISite `json:"site,omitempty"`
	// RackID is the operator-supplied identifier for the rack
	RackID string `json:"rackId"`
	// RackProfileID identifies the rack profile this rack conforms to
	RackProfileID string `json:"rackProfileId"`
	// Name is the optional human-readable name of the expected rack
	Name string `json:"name"`
	// Description is the optional human-readable description of the expected rack
	Description string `json:"description"`
	// Labels carries arbitrary key/value pairs. Well-known keys (chassis.*,
	// location.*) are used to convey chassis identity and physical location.
	Labels APILabels `json:"labels"`
	// Created indicates the ISO datetime string for when the ExpectedRack was created
	Created time.Time `json:"created"`
	// Updated indicates the ISO datetime string for when the ExpectedRack was last updated
	Updated time.Time `json:"updated"`
}

// NewAPIExpectedRack accepts a DB layer ExpectedRack object and returns an API object
func NewAPIExpectedRack(dbModel *cdbm.ExpectedRack) *APIExpectedRack {
	if dbModel == nil {
		return nil
	}

	apier := &APIExpectedRack{
		ID:            dbModel.ID,
		SiteID:        dbModel.SiteID,
		RackID:        dbModel.RackID,
		RackProfileID: dbModel.RackProfileID,
		Name:          dbModel.Name,
		Description:   dbModel.Description,
		Labels:        APILabels(dbModel.Labels),
		Created:       dbModel.Created,
		Updated:       dbModel.Updated,
	}

	if dbModel.Site != nil {
		site := NewAPISite(*dbModel.Site, []cdbm.StatusDetail{}, nil)
		apier.Site = &site
	}

	return apier
}

// APIReplaceAllExpectedRacksRequest is the data structure to capture user request
// to replace the full set of ExpectedRacks for a Site with the provided list.
type APIReplaceAllExpectedRacksRequest struct {
	// SiteID is the ID of the Site whose ExpectedRacks should be replaced
	SiteID string `json:"siteId"`
	// ExpectedRacks is the list of ExpectedRack create requests to use as the
	// replacement set for the Site. May be empty to clear all ExpectedRacks
	// for the Site.
	ExpectedRacks []*APIExpectedRackCreateRequest `json:"expectedRacks"`
}

// Validate ensure the values passed in request are acceptable
func (rar *APIReplaceAllExpectedRacksRequest) Validate() error {
	err := validation.ValidateStruct(rar,
		validation.Field(&rar.SiteID,
			validation.Required.Error(validationErrorValueRequired),
			validationis.UUID.Error(validationErrorInvalidUUID)),
	)
	if err != nil {
		return err
	}

	// Validate every entry and ensure they all reference the same Site as the top-level SiteID
	for i, er := range rar.ExpectedRacks {
		if er == nil {
			return validation.Errors{
				"expectedRacks": errors.New("ExpectedRack entry cannot be null"),
			}
		}
		if err := er.Validate(); err != nil {
			return validation.Errors{
				"expectedRacks": fmt.Errorf("entry %d: %w", i, err),
			}
		}
		if er.SiteID != rar.SiteID {
			return validation.Errors{
				"expectedRacks": fmt.Errorf("entry %d: siteId does not match top-level siteId", i),
			}
		}
	}

	// Ensure rack IDs are unique within the request
	seen := make(map[string]bool, len(rar.ExpectedRacks))
	for i, er := range rar.ExpectedRacks {
		if seen[er.RackID] {
			return validation.Errors{
				"expectedRacks": fmt.Errorf("entry %d: duplicate rackId %q", i, er.RackID),
			}
		}
		seen[er.RackID] = true
	}

	return nil
}
