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

// APIExpectedRackGroupCreateRequest is the data structure to capture request to create a new ExpectedRackGroup
type APIExpectedRackGroupCreateRequest struct {
	// SiteID is the ID of the Site the rack group belongs to.
	SiteID string `json:"siteId"`
	// RackGroupID is the operator-supplied identifier for the rack group (string, not UUID).
	// Unique per Site.
	RackGroupID string `json:"rackGroupId"`
	// Topology is the externally declared group-level topology identifier.
	Topology string `json:"topology"`
	// RackIDs are the external identifiers of expected racks in this group.
	RackIDs []string                      `json:"rackIds"`
	Members cdbm.ExpectedRackGroupMembers `json:"members"`
	// Name is the optional human-readable name of the expected rack group.
	Name *string `json:"name"`
	// Description is the optional human-readable description of the expected rack group.
	Description *string `json:"description"`
	// Labels carries arbitrary key/value pairs. Well-known keys (chassis.*,
	// location.*) are used to convey chassis identity and physical location.
	Labels map[string]string `json:"labels"`
}

// Validate ensure the values passed in request are acceptable
func (ercr *APIExpectedRackGroupCreateRequest) Validate() error {
	err := validation.ValidateStruct(ercr,
		validation.Field(&ercr.SiteID,
			validation.Required.Error(validationErrorValueRequired),
			validationis.UUID.Error(validationErrorInvalidUUID)),
		validation.Field(&ercr.RackGroupID,
			validation.Required.Error(validationErrorValueRequired),
			validation.RuneLength(1, 128),
			validation.Match(util.NotAllWhitespaceRegexp).Error("RackGroupID consists only of whitespace")),
		validation.Field(&ercr.Topology,
			validation.Required.Error(validationErrorValueRequired),
			validation.RuneLength(1, 128),
			validation.Match(util.NotAllWhitespaceRegexp).Error("Topology consists only of whitespace")),
		validation.Field(&ercr.RackIDs,
			validation.Each(validation.Required, validation.Match(util.NotAllWhitespaceRegexp).Error("RackID consists only of whitespace"))),
		validation.Field(&ercr.Name,
			validation.RuneLength(0, 255)),
	)

	if err != nil {
		return err
	}
	seenRackIDs := make(map[string]struct{}, len(ercr.RackIDs))
	for _, rackID := range ercr.RackIDs {
		if _, exists := seenRackIDs[rackID]; exists {
			return validation.Errors{"rackIds": fmt.Errorf("duplicate rackId %q", rackID)}
		}
		seenRackIDs[rackID] = struct{}{}
	}

	if err := ercr.Members.Validate(); err != nil {
		return err
	}

	if err := util.ValidateLabels(ercr.Labels); err != nil {
		return err
	}

	return nil
}

// APIExpectedRackGroupUpdateRequest is the data structure to capture user request to update an ExpectedRackGroup
type APIExpectedRackGroupUpdateRequest struct {
	// ID is required for batch updates (must be empty or match path value for single update).
	ID *string `json:"id"`
	// RackGroupID is the operator-supplied rack group identifier. It is immutable on
	// update: it may be omitted or set to the existing value, but a changed
	// value is rejected by the handler before any database mutation because
	// Core uses rackGroupId as the identity key.
	RackGroupID *string `json:"rackGroupId"`
	// Topology optionally replaces the group-level topology identifier.
	Topology *string `json:"topology"`
	// RackIDs optionally replaces the complete membership of the group.
	RackIDs []string                      `json:"rackIds"`
	Members cdbm.ExpectedRackGroupMembers `json:"members"`
	// Name is the optional new human-readable name of the expected rack group.
	Name *string `json:"name"`
	// Description is the optional new human-readable description of the expected rack group.
	Description *string `json:"description"`
	// Labels carries arbitrary key/value pairs. Well-known keys (chassis.*,
	// location.*) are used to convey chassis identity and physical location.
	Labels map[string]string `json:"labels"`
}

// Validate ensure the values passed in request are acceptable
func (erur *APIExpectedRackGroupUpdateRequest) Validate() error {
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
	if erur.RackGroupID == nil && erur.Topology == nil && erur.RackIDs == nil && erur.Members == nil && erur.Name == nil && erur.Description == nil && erur.Labels == nil {
		return validation.Errors{
			"body": errors.New("at least one mutable field must be provided"),
		}
	}

	err := validation.ValidateStruct(erur,
		validation.Field(&erur.RackGroupID,
			validation.NilOrNotEmpty.Error("RackGroupID cannot be empty"),
			validation.RuneLength(1, 128),
			validation.When(erur.RackGroupID != nil && *erur.RackGroupID != "",
				validation.Match(util.NotAllWhitespaceRegexp).Error("RackGroupID consists only of whitespace"))),
		validation.Field(&erur.Topology,
			validation.NilOrNotEmpty.Error("Topology cannot be empty"),
			validation.RuneLength(1, 128),
			validation.When(erur.Topology != nil && *erur.Topology != "",
				validation.Match(util.NotAllWhitespaceRegexp).Error("Topology consists only of whitespace"))),
		validation.Field(&erur.RackIDs,
			validation.When(erur.RackIDs != nil,
				validation.Each(validation.Required, validation.Match(util.NotAllWhitespaceRegexp).Error("RackID consists only of whitespace")))),
		validation.Field(&erur.Name,
			validation.RuneLength(0, 255)),
	)

	if err != nil {
		return err
	}
	seenRackIDs := make(map[string]struct{}, len(erur.RackIDs))
	for _, rackID := range erur.RackIDs {
		if _, exists := seenRackIDs[rackID]; exists {
			return validation.Errors{"rackIds": fmt.Errorf("duplicate rackId %q", rackID)}
		}
		seenRackIDs[rackID] = struct{}{}
	}

	if err := erur.Members.Validate(); err != nil {
		return err
	}

	if err := util.ValidateLabels(erur.Labels); err != nil {
		return err
	}

	return nil
}

// APIExpectedRackGroup is the data structure to capture API representation of an ExpectedRackGroup
type APIExpectedRackGroup struct {
	// ID is the unique identifier (UUID) of the expected rack group.
	ID uuid.UUID `json:"id"`
	// SiteID is the ID of the Site this rack group belongs to.
	SiteID uuid.UUID `json:"siteId"`
	// Site is the site information
	Site *APISite `json:"site"`
	// RackGroupID is the operator-supplied identifier for the rack group.
	RackGroupID string `json:"rackGroupId"`
	// Topology is the externally declared group-level topology identifier.
	Topology string `json:"topology"`
	// RackIDs are the external identifiers of expected racks in this group.
	RackIDs []string                      `json:"rackIds"`
	Members cdbm.ExpectedRackGroupMembers `json:"members"`
	// Name is the optional human-readable name of the expected rack group.
	Name string `json:"name"`
	// Description is the optional human-readable description of the expected rack group.
	Description string `json:"description"`
	// Labels carries arbitrary key/value pairs. Well-known keys (chassis.*,
	// location.*) are used to convey chassis identity and physical location.
	Labels APILabels `json:"labels"`
	// Created indicates the ISO datetime string for when the ExpectedRackGroup was created
	Created time.Time `json:"created"`
	// Updated indicates the ISO datetime string for when the ExpectedRackGroup was last updated
	Updated time.Time `json:"updated"`
}

// NewAPIExpectedRackGroup accepts a DB layer ExpectedRackGroup object and returns an API object
func NewAPIExpectedRackGroup(dbModel *cdbm.ExpectedRackGroup) *APIExpectedRackGroup {
	if dbModel == nil {
		return nil
	}

	apier := &APIExpectedRackGroup{
		ID:          dbModel.ID,
		SiteID:      dbModel.SiteID,
		RackGroupID: dbModel.RackGroupID,
		Topology:    dbModel.Topology,
		RackIDs:     dbModel.RackIDs,
		Members:     dbModel.Members,
		Name:        dbModel.Name,
		Description: dbModel.Description,
		Labels:      APILabels(dbModel.Labels),
		Created:     dbModel.Created,
		Updated:     dbModel.Updated,
	}

	if dbModel.Site != nil {
		site := NewAPISite(*dbModel.Site, []cdbm.StatusDetail{}, nil)
		apier.Site = &site
	}

	return apier
}

// APIReplaceAllExpectedRackGroupsRequest is the data structure to capture user request
// to replace the full set of ExpectedRackGroups for a Site with the provided list.
type APIReplaceAllExpectedRackGroupsRequest struct {
	// SiteID is the ID of the Site whose ExpectedRackGroups should be replaced
	SiteID string `json:"siteId"`
	// ExpectedRackGroups is the list of ExpectedRackGroup create requests to use as the
	// replacement set for the Site. May be empty to clear all ExpectedRackGroups
	// for the Site.
	ExpectedRackGroups []*APIExpectedRackGroupCreateRequest `json:"expectedRackGroups"`
}

// Validate ensure the values passed in request are acceptable
func (rar *APIReplaceAllExpectedRackGroupsRequest) Validate() error {
	err := validation.ValidateStruct(rar,
		validation.Field(&rar.SiteID,
			validation.Required.Error(validationErrorValueRequired),
			validationis.UUID.Error(validationErrorInvalidUUID)),
	)
	if err != nil {
		return err
	}

	// Validate every entry and ensure they all reference the same Site as the top-level SiteID
	for i, er := range rar.ExpectedRackGroups {
		if er == nil {
			return validation.Errors{
				"expectedRackGroups": errors.New("ExpectedRackGroup entry cannot be null"),
			}
		}
		if err := er.Validate(); err != nil {
			return validation.Errors{
				"expectedRackGroups": fmt.Errorf("entry %d: %w", i, err),
			}
		}
		if er.SiteID != rar.SiteID {
			return validation.Errors{
				"expectedRackGroups": fmt.Errorf("entry %d: siteId does not match top-level siteId", i),
			}
		}
	}

	// Ensure group IDs are unique within the replacement set.
	seen := make(map[string]bool, len(rar.ExpectedRackGroups))
	for i, er := range rar.ExpectedRackGroups {
		if seen[er.RackGroupID] {
			return validation.Errors{
				"expectedRackGroups": fmt.Errorf("entry %d: duplicate rackGroupId %q", i, er.RackGroupID),
			}
		}
		seen[er.RackGroupID] = true
	}

	return nil
}
