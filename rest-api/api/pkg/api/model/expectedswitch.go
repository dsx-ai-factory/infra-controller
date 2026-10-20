// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"encoding/json"
	"errors"
	"fmt"
	"regexp"
	"time"

	"github.com/google/uuid"

	validation "github.com/go-ozzo/ozzo-validation/v4"
	validationis "github.com/go-ozzo/ozzo-validation/v4/is"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model/util"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// nvosMacAddressRegexp matches the 6-octet, colon- or hyphen-separated MAC
// address format the OpenAPI contract publishes for NVOS interface MACs.
var nvosMacAddressRegexp = regexp.MustCompile(`^([0-9A-Fa-f]{2}[:-]){5}([0-9A-Fa-f]{2})$`)

// APINvosMacAddresses is the deprecated flat NVOS MAC list returned in responses.
type APINvosMacAddresses []string

// APINvosInterface is one NVOS ethernet port of an expected switch: its MAC
// address and whether it is the primary (eth0) port NICo manages via RMS.
type APINvosInterface struct {
	// MacAddress is the MAC of this NVOS ethernet port.
	MacAddress string `json:"macAddress"`
	// IsPrimary marks this port as the primary (eth0) port. At most one entry
	// per switch may be primary; when no entry is marked, the REST layer defaults
	// the first port on a multi-port switch (a single-port switch is always
	// primary) when it builds the Core request, so Core does not apply this
	// default on the REST path.
	IsPrimary bool `json:"isPrimary"`
}

// APINvosInterfaces is the list of NVOS ethernet ports on an expected switch.
type APINvosInterfaces []APINvosInterface

// Validate checks that every entry carries a well-formed MAC, that no MAC
// repeats (ignoring case and separator style since those spellings refer to the
// same interface), and that at most one entry is marked primary.
func (interfaces APINvosInterfaces) Validate() error {
	seen := make(map[string]bool, len(interfaces))
	primaryCount := 0
	for i := range interfaces {
		iface := interfaces[i]
		if err := validation.Validate(iface.MacAddress,
			validation.Required.Error(validationErrorValueRequired),
			validation.Match(nvosMacAddressRegexp).Error("must be a MAC address of six colon- or hyphen-separated octets")); err != nil {
			return err
		}
		key := cdbm.NormalizeMacAddress(iface.MacAddress)
		if seen[key] {
			return fmt.Errorf("duplicate MAC address: %s", iface.MacAddress)
		}
		seen[key] = true
		if iface.IsPrimary {
			primaryCount++
		}
	}
	if primaryCount > 1 {
		return errors.New("at most one nvosInterfaces entry may be marked primary")
	}
	return nil
}

// MacAddresses returns the ordered NVOS MAC list for the flat DB column. A nil
// receiver yields nil so an absent field stays distinct from an explicitly
// empty set.
func (interfaces APINvosInterfaces) MacAddresses() []string {
	if interfaces == nil {
		return nil
	}
	macs := make([]string, 0, len(interfaces))
	for i := range interfaces {
		macs = append(macs, interfaces[i].MacAddress)
	}
	return macs
}

// PrimaryMacAddress returns the explicitly flagged primary MAC, or nil when no
// entry is flagged. The REST persistence layer applies its first-port wire
// default after storing an unflagged list.
func (interfaces APINvosInterfaces) PrimaryMacAddress() *string {
	for i := range interfaces {
		if interfaces[i].IsPrimary {
			mac := interfaces[i].MacAddress
			return &mac
		}
	}
	return nil
}

// APIExpectedSwitchCreateRequest is the data structure to capture request to create a new ExpectedSwitch
type APIExpectedSwitchCreateRequest struct {
	// SiteID is the ID of the Site
	SiteID string `json:"siteId"`
	// BmcMacAddress is the MAC address of the expected switch's BMC
	BmcMacAddress string `json:"bmcMacAddress"`
	// DefaultBmcUsername is the username of the expected switch's BMC
	DefaultBmcUsername *string `json:"defaultBmcUsername"`
	// DefaultBmcPassword is the password of the expected switch's BMC
	DefaultBmcPassword *string `json:"defaultBmcPassword"`
	// SwitchSerialNumber is the serial number of the expected switch
	SwitchSerialNumber string `json:"switchSerialNumber"`
	// NvOsUsername is the NVOS username of the expected switch
	NvOsUsername *string `json:"nvOsUsername"`
	// NvOsPassword is the NVOS password of the expected switch
	NvOsPassword *string `json:"nvOsPassword"`
	// NvosInterfaces lists the switch's NVOS ethernet ports. Mark one entry
	// primary to choose eth0; a single-port switch is primary implicitly and a
	// multi-port switch with no primary marked defaults to the first entry.
	NvosInterfaces APINvosInterfaces `json:"nvosInterfaces"`
	// NvosMacAddresses is deprecated and ignored, regardless of its JSON value.
	// Use NvosInterfaces to configure ports and primary selection.
	NvosMacAddresses json.RawMessage `json:"nvosMacAddresses"`
	// RackID is the optional rack identifier
	RackID *string `json:"rackId"`
	// BmcIpAddress is the optional BMC IP address of the expected switch
	BmcIpAddress *string `json:"bmcIpAddress"`
	// Name is the optional name of the expected switch
	Name *string `json:"name"`
	// Manufacturer is the optional manufacturer of the expected switch
	Manufacturer *string `json:"manufacturer"`
	// Model is the optional model of the expected switch
	Model *string `json:"model"`
	// Description is the optional description of the expected switch
	Description *string `json:"description"`
	// SlotID is the optional slot identifier
	SlotID *int32 `json:"slotId"`
	// TrayIdx is the optional tray index
	TrayIdx *int32 `json:"trayIdx"`
	// HostID is the optional host identifier
	HostID *int32 `json:"hostId"`
	// Labels is the labels of the expected switch
	Labels map[string]string `json:"labels"`
}

// Validate ensure the values passed in request are acceptable
func (escr *APIExpectedSwitchCreateRequest) Validate() error {
	err := validation.ValidateStruct(escr,
		validation.Field(&escr.SiteID,
			validation.Required.Error(validationErrorValueRequired),
			validationis.UUID.Error(validationErrorInvalidUUID)),
		validation.Field(&escr.BmcMacAddress,
			validation.Required.Error(validationErrorValueRequired),
			validationis.MAC),
		validation.Field(&escr.DefaultBmcPassword,
			validation.Length(0, 255).Error("BMC password must be 255 characters or less")),
		validation.Field(&escr.SwitchSerialNumber,
			validation.Required.Error(validationErrorValueRequired),
			validation.Match(util.NotAllWhitespaceRegexp).Error("Switch serial number consists only of whitespace"),
			validation.Length(1, 32).Error("Switch serial number must be 32 characters or less")),
		validation.Field(&escr.NvosInterfaces, validation.Required.Error("at least one nvosInterfaces entry is required")),
		validation.Field(&escr.RackID,
			validation.NilOrNotEmpty.Error("RackID cannot be empty")),
		validation.Field(&escr.BmcIpAddress,
			validation.NilOrNotEmpty.Error("BmcIpAddress cannot be empty"),
			validation.When(escr.BmcIpAddress != nil && *escr.BmcIpAddress != "",
				validation.By(util.ValidateExpectedBmcIPAddress))),
		validation.Field(&escr.Name,
			validation.NilOrNotEmpty.Error("Name cannot be empty")),
		validation.Field(&escr.Manufacturer,
			validation.NilOrNotEmpty.Error("Manufacturer cannot be empty")),
		validation.Field(&escr.Model,
			validation.NilOrNotEmpty.Error("Model cannot be empty")),
		validation.Field(&escr.Description,
			validation.NilOrNotEmpty.Error("Description cannot be empty")),
	)

	if err != nil {
		return err
	}

	if err := util.ValidateLabels(escr.Labels); err != nil {
		return err
	}

	return nil
}

// APIExpectedSwitchUpdateRequest is the data structure to capture user request to update an ExpectedSwitch
type APIExpectedSwitchUpdateRequest struct {
	// ID can be omitted or null for PATCH. A supplied string must match the
	// path UUID in lowercase hyphenated form; an empty string is invalid.
	ID *string `json:"id"`
	// BmcMacAddress is the MAC address of the expected switch's BMC
	BmcMacAddress *string `json:"bmcMacAddress"`
	// DefaultBmcUsername is the username of the expected switch's BMC
	DefaultBmcUsername *string `json:"defaultBmcUsername"`
	// DefaultBmcPassword is the password of the expected switch's BMC
	DefaultBmcPassword *string `json:"defaultBmcPassword"`
	// SwitchSerialNumber is the serial number of the expected switch
	SwitchSerialNumber *string `json:"switchSerialNumber"`
	// NvOsUsername is the NVOS username of the expected switch
	NvOsUsername *string `json:"nvOsUsername"`
	// NvOsPassword is the NVOS password of the expected switch
	NvOsPassword *string `json:"nvOsPassword"`
	// NvosInterfaces replaces the switch's NVOS ethernet port set. Omission
	// leaves the stored set unchanged; an empty array is rejected because a
	// switch must keep at least one NVOS port. Mark one entry primary to choose
	// eth0.
	NvosInterfaces APINvosInterfaces `json:"nvosInterfaces"`
	// NvosMacAddresses is deprecated and ignored, regardless of its JSON value.
	// Use NvosInterfaces to configure ports and primary selection.
	NvosMacAddresses json.RawMessage `json:"nvosMacAddresses"`
	// RackID is the optional rack identifier
	RackID *string `json:"rackId"`
	// BmcIpAddress is the optional BMC IP address of the expected switch
	BmcIpAddress *string `json:"bmcIpAddress"`
	// Name is the optional name of the expected switch
	Name *string `json:"name"`
	// Manufacturer is the optional manufacturer of the expected switch
	Manufacturer *string `json:"manufacturer"`
	// Model is the optional model of the expected switch
	Model *string `json:"model"`
	// Description is the optional description of the expected switch
	Description *string `json:"description"`
	// SlotID is the optional slot identifier
	SlotID *int32 `json:"slotId"`
	// TrayIdx is the optional tray index
	TrayIdx *int32 `json:"trayIdx"`
	// HostID is the optional host identifier
	HostID *int32 `json:"hostId"`
	// Labels is the labels of the expected switch
	Labels map[string]string `json:"labels"`
}

// Validate ensure the values passed in request are acceptable
func (esur *APIExpectedSwitchUpdateRequest) Validate() error {
	if esur.ID != nil {
		if *esur.ID == "" {
			return validation.Errors{
				"id": errors.New("ID cannot be empty"),
			}
		}
		if _, err := uuid.Parse(*esur.ID); err != nil {
			return validation.Errors{
				"id": errors.New("ID must be a valid UUID"),
			}
		}
	}

	// The structured field requires a nonempty replacement.
	if esur.NvosInterfaces != nil && len(esur.NvosInterfaces) == 0 {
		return validation.Errors{
			"nvosInterfaces": errors.New("nvosInterfaces must not be an empty list; omit the field to leave the NVOS ports unchanged"),
		}
	}

	err := validation.ValidateStruct(esur,
		validation.Field(&esur.DefaultBmcUsername,
			validation.NilOrNotEmpty.Error("BMC Username cannot be empty"),
			validation.When(esur.DefaultBmcUsername != nil && *esur.DefaultBmcUsername != "",
				validation.Match(util.NotAllWhitespaceRegexp).Error("BMC Username consists only of whitespace"))),
		validation.Field(&esur.DefaultBmcPassword,
			validation.NilOrNotEmpty.Error("BMC Password cannot be empty"),
			validation.When(esur.DefaultBmcPassword != nil && *esur.DefaultBmcPassword != "",
				validation.Match(util.NotAllWhitespaceRegexp).Error("BMC Password consists only of whitespace")),
			validation.Length(1, 255).Error("BMC Password must be 1-255 characters")),
		validation.Field(&esur.NvOsUsername,
			validation.NilOrNotEmpty.Error("NVOS Username cannot be empty")),
		validation.Field(&esur.NvOsPassword,
			validation.NilOrNotEmpty.Error("NVOS Password cannot be empty")),
		validation.Field(&esur.SwitchSerialNumber,
			validation.NilOrNotEmpty.Error("Switch Serial Number cannot be empty"),
			validation.When(esur.SwitchSerialNumber != nil && *esur.SwitchSerialNumber != "",
				validation.Match(util.NotAllWhitespaceRegexp).Error("Switch Serial Number consists only of whitespace")),
			validation.Length(1, 32).Error("Switch Serial Number must be 1-32 characters")),
		validation.Field(&esur.NvosInterfaces),
		validation.Field(&esur.RackID,
			validation.NilOrNotEmpty.Error("RackID cannot be empty")),
		validation.Field(&esur.BmcIpAddress,
			validation.When(esur.BmcIpAddress != nil && *esur.BmcIpAddress != "",
				validation.By(util.ValidateExpectedBmcIPAddress))),
		validation.Field(&esur.Name,
			validation.NilOrNotEmpty.Error("Name cannot be empty")),
		validation.Field(&esur.Manufacturer,
			validation.NilOrNotEmpty.Error("Manufacturer cannot be empty")),
		validation.Field(&esur.Model,
			validation.NilOrNotEmpty.Error("Model cannot be empty")),
		validation.Field(&esur.Description,
			validation.NilOrNotEmpty.Error("Description cannot be empty")),
	)

	if err != nil {
		return err
	}

	if err := util.ValidateLabels(esur.Labels); err != nil {
		return err
	}

	return nil
}

// ToProto builds the Core patch from the updated cloud row and the fields
// selected by this request. Call Validate before conversion and pass the
// updated cloud row so derived metadata labels include its retained values.
// Explicit zero and empty values remain updates.
func (esur *APIExpectedSwitchUpdateRequest) ToProto(entity *cdbm.ExpectedSwitch) *corev1.PatchExpectedSwitchRequest {
	resource := entity.ToProto(cdbm.ExpectedSwitchCredentials{
		BmcUsername:  esur.DefaultBmcUsername,
		BmcPassword:  esur.DefaultBmcPassword,
		NvosUsername: esur.NvOsUsername,
		NvosPassword: esur.NvOsPassword,
	})
	return &corev1.PatchExpectedSwitchRequest{
		ExpectedSwitch: resource,
		UpdateMask: util.ExpectedComponentUpdateMask(
			util.ExpectedComponentUpdateField{Path: "bmc_username", Present: esur.DefaultBmcUsername != nil},
			util.ExpectedComponentUpdateField{Path: "bmc_password", Present: esur.DefaultBmcPassword != nil},
			util.ExpectedComponentUpdateField{Path: "bmc_ip_address", Present: esur.BmcIpAddress != nil},
			util.ExpectedComponentUpdateField{Path: "rack_id", Present: esur.RackID != nil},
			util.ExpectedComponentUpdateField{Path: "metadata.name", Present: esur.Name != nil},
			util.ExpectedComponentUpdateField{Path: "metadata.description", Present: esur.Description != nil},
			util.ExpectedComponentUpdateField{Path: "metadata.labels", Present: esur.Labels != nil || esur.Manufacturer != nil || esur.Model != nil || esur.SlotID != nil || esur.TrayIdx != nil || esur.HostID != nil},
			util.ExpectedComponentUpdateField{Path: "switch_serial_number", Present: esur.SwitchSerialNumber != nil},
			util.ExpectedComponentUpdateField{Path: "nvos_interfaces", Present: esur.NvosInterfaces != nil},
			util.ExpectedComponentUpdateField{Path: "nvos_username", Present: esur.NvOsUsername != nil},
			util.ExpectedComponentUpdateField{Path: "nvos_password", Present: esur.NvOsPassword != nil},
		),
	}
}

// APIExpectedSwitch is the data structure to capture API representation of an ExpectedSwitch
type APIExpectedSwitch struct {
	// ID is the ID of this Expected Switch
	ID uuid.UUID `json:"id"`
	// BmcMacAddress is the MAC address of the expected switch's BMC
	BmcMacAddress string `json:"bmcMacAddress"`
	// SiteID is the ID of the site this switch belongs to
	SiteID uuid.UUID `json:"siteId"`
	// Site is the site information
	Site *APISite `json:"site,omitempty"`
	// SwitchSerialNumber is the serial number of the expected switch
	SwitchSerialNumber string `json:"switchSerialNumber"`
	// NvosInterfaces lists the switch's NVOS ethernet ports with the primary
	// (eth0) port flagged.
	NvosInterfaces APINvosInterfaces `json:"nvosInterfaces"`
	// NvosMacAddresses is the deprecated flat form retained for v2 clients.
	// New clients should use NvosInterfaces.
	NvosMacAddresses APINvosMacAddresses `json:"nvosMacAddresses"`
	// RackID is the optional rack identifier
	RackID *string `json:"rackId"`
	// BmcIpAddress is the optional BMC IP address of the expected switch
	BmcIpAddress *string `json:"bmcIpAddress"`
	// Name is the optional name of the expected switch
	Name *string `json:"name"`
	// Manufacturer is the optional manufacturer of the expected switch
	Manufacturer *string `json:"manufacturer"`
	// Model is the optional model of the expected switch
	Model *string `json:"model"`
	// Description is the optional description of the expected switch
	Description *string `json:"description"`
	// SlotID is the optional slot identifier
	SlotID *int32 `json:"slotId"`
	// TrayIdx is the optional tray index
	TrayIdx *int32 `json:"trayIdx"`
	// HostID is the optional host identifier
	HostID *int32 `json:"hostId"`
	// Labels is the labels of the expected switch
	Labels APILabels `json:"labels"`
	// Created indicates the ISO datetime string for when the ExpectedSwitch was created
	Created time.Time `json:"created"`
	// Updated indicates the ISO datetime string for when the ExpectedSwitch was last updated
	Updated time.Time `json:"updated"`
}

// newAPINvosInterfaces converts the DB model's canonical NVOS port view into
// the API representation so the response flags the same primary as the workflow
// proto. A nil stored MAC list yields an empty array on the REST surface.
func newAPINvosInterfaces(dbModel *cdbm.ExpectedSwitch) APINvosInterfaces {
	views := dbModel.NvosInterfaces()
	if views == nil {
		return make(APINvosInterfaces, 0)
	}
	interfaces := make(APINvosInterfaces, 0, len(views))
	for _, view := range views {
		interfaces = append(interfaces, APINvosInterface{
			MacAddress: view.MacAddress,
			IsPrimary:  view.IsPrimary,
		})
	}
	return interfaces
}

// NewAPIExpectedSwitch accepts a DB layer ExpectedSwitch object and returns an API object
func NewAPIExpectedSwitch(dbModel *cdbm.ExpectedSwitch) *APIExpectedSwitch {
	legacyNvosMacAddresses := make(APINvosMacAddresses, len(dbModel.NvosMacAddresses))
	copy(legacyNvosMacAddresses, dbModel.NvosMacAddresses)
	apies := &APIExpectedSwitch{
		ID:                 dbModel.ID,
		BmcMacAddress:      dbModel.BmcMacAddress,
		SiteID:             dbModel.SiteID,
		SwitchSerialNumber: dbModel.SwitchSerialNumber,
		NvosInterfaces:     newAPINvosInterfaces(dbModel),
		NvosMacAddresses:   legacyNvosMacAddresses,
		RackID:             dbModel.RackID,
		BmcIpAddress:       dbModel.BmcIpAddress,
		Name:               dbModel.Name,
		Manufacturer:       dbModel.Manufacturer,
		Model:              dbModel.Model,
		Description:        dbModel.Description,
		SlotID:             dbModel.SlotID,
		TrayIdx:            dbModel.TrayIdx,
		HostID:             dbModel.HostID,
		Labels:             APILabels(dbModel.Labels),
		Created:            dbModel.Created,
		Updated:            dbModel.Updated,
	}

	// Expand Site details if available
	if dbModel.Site != nil {
		site := NewAPISite(*dbModel.Site, []cdbm.StatusDetail{}, nil)
		apies.Site = &site
	}

	return apies
}
