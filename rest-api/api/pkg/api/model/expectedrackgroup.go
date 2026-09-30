// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"bytes"
	"encoding/json"
	"errors"
	"fmt"
	"time"

	"github.com/google/uuid"

	validation "github.com/go-ozzo/ozzo-validation/v4"
	validationis "github.com/go-ozzo/ozzo-validation/v4/is"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model/util"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
)

type APIExpectedRackGroupMember struct {
	Type         string `json:"type"`
	Manufacturer string `json:"manufacturer"`
	ID           string `json:"id"`
}

func (member APIExpectedRackGroupMember) ToDBModel() cdbm.ExpectedRackGroupMember {
	return cdbm.ExpectedRackGroupMember{Type: cdbm.ExpectedRackGroupMemberType(member.Type), Manufacturer: member.Manufacturer, ID: member.ID}
}

func (member *APIExpectedRackGroupMember) FromDBModel(value cdbm.ExpectedRackGroupMember) {
	*member = APIExpectedRackGroupMember{Type: string(value.Type), Manufacturer: value.Manufacturer, ID: value.ID}
}

type APIExpectedRackGroupRack struct {
	RackID  string                       `json:"rackId"`
	Members []APIExpectedRackGroupMember `json:"members"`
}

func (rack APIExpectedRackGroupRack) ToDBModel() cdbm.ExpectedRackGroupRack {
	result := cdbm.ExpectedRackGroupRack{RackID: rack.RackID, Members: make([]cdbm.ExpectedRackGroupMember, 0, len(rack.Members))}
	for _, member := range rack.Members {
		result.Members = append(result.Members, member.ToDBModel())
	}
	return result
}

func (rack *APIExpectedRackGroupRack) FromDBModel(value cdbm.ExpectedRackGroupRack) {
	rack.RackID = value.RackID
	rack.Members = make([]APIExpectedRackGroupMember, 0, len(value.Members))
	for _, value := range value.Members {
		var member APIExpectedRackGroupMember
		member.FromDBModel(value)
		rack.Members = append(rack.Members, member)
	}
}

func (group APIExpectedRackGroup) validateRacks(_ interface{}) error {
	dbModel := cdbm.ExpectedRackGroup{Racks: make([]cdbm.ExpectedRackGroupRack, 0, len(group.Racks))}
	for _, rack := range group.Racks {
		dbModel.Racks = append(dbModel.Racks, rack.ToDBModel())
	}
	return dbModel.Validate()
}

// APIExpectedRackGroupCreateRequest is the data structure to capture request to create a new ExpectedRackGroup
type APIExpectedRackGroupCreateRequest struct {
	// SiteID is the ID of the Site the rack group belongs to.
	SiteID string `json:"siteId"`
	// RackGroupID is the operator-supplied identifier for the rack group (string, not UUID).
	// Unique per Site.
	RackGroupID string `json:"rackGroupId"`
	// Topology is the externally declared group-level topology identifier.
	Topology string `json:"topology"`
	// Racks contains rack identities and their device membership.
	Racks []APIExpectedRackGroupRack `json:"racks"`
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
		validation.Field(&ercr.Racks, validation.By(APIExpectedRackGroup{Racks: ercr.Racks}.validateRacks)),
		validation.Field(&ercr.Name,
			validation.Length(0, 256), validationis.ASCII),
		validation.Field(&ercr.Description, validation.Length(0, 1024)),
	)

	if err != nil {
		return err
	}
	return validateExpectedRackGroupLabels(ercr.Labels)
}

// APIExpectedRackGroupUpdateRequest is the data structure to capture user request to update an ExpectedRackGroup
type APIExpectedRackGroupUpdateRequest struct {
	// ID can be omitted or null for PATCH. A supplied string must match the
	// path UUID in lowercase hyphenated form; an empty string is invalid.
	ID *string `json:"id"`
	// RackGroupID is the operator-supplied rack group identifier. It is immutable on
	// update: it may be omitted or set to the existing value, but a changed
	// value is rejected by the handler before any database mutation because
	// Core uses rackGroupId as the identity key.
	RackGroupID *string `json:"rackGroupId"`
	// Topology optionally replaces the group-level topology identifier.
	Topology *string `json:"topology"`
	// Racks contains rack identities and their device membership.
	Racks []APIExpectedRackGroupRack `json:"racks"`
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
		_, err := uuid.Parse(*erur.ID)
		if err != nil {
			return validation.Errors{
				"id": errors.New("ID must be a valid UUID"),
			}
		}
	}

	// Reject empty updates: require at least one mutable field. An update with
	// no fields would still bump the timestamp and trigger a workflow round-trip.
	if erur.RackGroupID == nil && erur.Topology == nil && erur.Racks == nil && erur.Name == nil && erur.Description == nil && erur.Labels == nil {
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
		validation.Field(&erur.Racks, validation.By(APIExpectedRackGroup{Racks: erur.Racks}.validateRacks)),
		validation.Field(&erur.Name,
			validation.Length(0, 256), validationis.ASCII),
		validation.Field(&erur.Description, validation.Length(0, 1024)),
	)

	if err != nil {
		return err
	}
	return validateExpectedRackGroupLabels(erur.Labels)
}

// Core metadata permits Unicode label values, but requires ASCII label keys.
func validateExpectedRackGroupLabels(labels map[string]string) error {
	err := util.ValidateLabels(labels)
	if err != nil {
		return err
	}
	for key := range labels {
		err = validationis.ASCII.Validate(key)
		if err != nil {
			return validation.Errors{"labels": err}
		}
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
	// Racks contains rack identities and their device membership.
	Racks []APIExpectedRackGroupRack `json:"racks"`
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
		Racks:       make([]APIExpectedRackGroupRack, 0, len(dbModel.Racks)),
		Name:        dbModel.Name,
		Description: dbModel.Description,
		Labels:      APILabels(dbModel.Labels),
		Created:     dbModel.Created,
		Updated:     dbModel.Updated,
	}

	for _, value := range dbModel.Racks {
		var rack APIExpectedRackGroupRack
		rack.FromDBModel(value)
		apier.Racks = append(apier.Racks, rack)
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
	// Only an explicitly supplied [] may clear the site's inventory.
	if rar.ExpectedRackGroups == nil {
		return validation.Errors{"expectedRackGroups": errors.New("must be provided and must not be null; use [] to clear all groups")}
	}
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
		err := er.Validate()
		if err != nil {
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

// UnmarshalJSON rejects obsolete flat membership fields instead of silently dropping devices.
func (ercr *APIExpectedRackGroupCreateRequest) UnmarshalJSON(data []byte) error {
	type request APIExpectedRackGroupCreateRequest
	decoder := json.NewDecoder(bytes.NewReader(data))
	decoder.DisallowUnknownFields()
	return decoder.Decode((*request)(ercr))
}

// UnmarshalJSON applies the same strict request shape to partial updates.
func (erur *APIExpectedRackGroupUpdateRequest) UnmarshalJSON(data []byte) error {
	type request APIExpectedRackGroupUpdateRequest
	decoder := json.NewDecoder(bytes.NewReader(data))
	decoder.DisallowUnknownFields()
	return decoder.Decode((*request)(erur))
}
