// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"errors"
	"fmt"
	"strings"
	"time"

	camu "github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model/util"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	validation "github.com/go-ozzo/ozzo-validation/v4"
	validationis "github.com/go-ozzo/ozzo-validation/v4/is"
)

// HostFirmwareComponentType is the REST-layer component type for host firmware
// configs. Values use CapitalCase and map to forge.HostFirmwareComponentType.
type HostFirmwareComponentType string

const (
	HostFirmwareComponentTypeBMC             HostFirmwareComponentType = "BMC"
	HostFirmwareComponentTypeCEC             HostFirmwareComponentType = "CEC"
	HostFirmwareComponentTypeUEFI            HostFirmwareComponentType = "UEFI"
	HostFirmwareComponentTypeNIC             HostFirmwareComponentType = "NIC"
	HostFirmwareComponentTypeCpldMb          HostFirmwareComponentType = "CpldMb"
	HostFirmwareComponentTypeCpldPdb         HostFirmwareComponentType = "CpldPdb"
	HostFirmwareComponentTypeHgxBmc          HostFirmwareComponentType = "HgxBmc"
	HostFirmwareComponentTypeCombinedBmcUefi HostFirmwareComponentType = "CombinedBmcUefi"
	HostFirmwareComponentTypeGPU             HostFirmwareComponentType = "GPU"
	HostFirmwareComponentTypeCx7             HostFirmwareComponentType = "Cx7"
	// HostFirmwareComponentTypeDrive is the firmware of one drive model, named by driveModel.
	HostFirmwareComponentTypeDrive HostFirmwareComponentType = "Drive"
)

var hostFirmwareComponentTypeChoiceMap = map[HostFirmwareComponentType]corev1.HostFirmwareComponentType{
	HostFirmwareComponentTypeBMC:             corev1.HostFirmwareComponentType_HOST_FIRMWARE_COMPONENT_TYPE_BMC,
	HostFirmwareComponentTypeCEC:             corev1.HostFirmwareComponentType_HOST_FIRMWARE_COMPONENT_TYPE_CEC,
	HostFirmwareComponentTypeUEFI:            corev1.HostFirmwareComponentType_HOST_FIRMWARE_COMPONENT_TYPE_UEFI,
	HostFirmwareComponentTypeNIC:             corev1.HostFirmwareComponentType_HOST_FIRMWARE_COMPONENT_TYPE_NIC,
	HostFirmwareComponentTypeCpldMb:          corev1.HostFirmwareComponentType_HOST_FIRMWARE_COMPONENT_TYPE_CPLD_MB,
	HostFirmwareComponentTypeCpldPdb:         corev1.HostFirmwareComponentType_HOST_FIRMWARE_COMPONENT_TYPE_CPLD_PDB,
	HostFirmwareComponentTypeHgxBmc:          corev1.HostFirmwareComponentType_HOST_FIRMWARE_COMPONENT_TYPE_HGX_BMC,
	HostFirmwareComponentTypeCombinedBmcUefi: corev1.HostFirmwareComponentType_HOST_FIRMWARE_COMPONENT_TYPE_COMBINED_BMC_UEFI,
	HostFirmwareComponentTypeGPU:             corev1.HostFirmwareComponentType_HOST_FIRMWARE_COMPONENT_TYPE_GPU,
	HostFirmwareComponentTypeCx7:             corev1.HostFirmwareComponentType_HOST_FIRMWARE_COMPONENT_TYPE_CX7,
	HostFirmwareComponentTypeDrive:           corev1.HostFirmwareComponentType_HOST_FIRMWARE_COMPONENT_TYPE_DRIVE,
}

// ToProto converts the REST component type to its forge enum value.
func (t HostFirmwareComponentType) ToProto() corev1.HostFirmwareComponentType {
	if v, ok := hostFirmwareComponentTypeChoiceMap[t]; ok {
		return v
	}
	return corev1.HostFirmwareComponentType_HOST_FIRMWARE_COMPONENT_TYPE_UNSPECIFIED
}

// FromProto populates the REST component type from a forge enum value.
func (t *HostFirmwareComponentType) FromProto(p corev1.HostFirmwareComponentType) {
	if t == nil {
		return
	}
	for rest, proto := range hostFirmwareComponentTypeChoiceMap {
		if proto == p {
			*t = rest
			return
		}
	}
	*t = ""
}

// maxHostFirmwareDriveModelLength matches Core's limit on a drive model, in bytes.
const maxHostFirmwareDriveModelLength = 128

// APIHostFirmwareComponentRef names one component: its type and, for a Drive,
// its drive model.
type APIHostFirmwareComponentRef struct {
	Type HostFirmwareComponentType `json:"type"`
	// DriveModel is the drive's Redfish Drive.Model as Site Explorer records
	// it, without surrounding whitespace. Required for Drive and rejected for
	// every other type.
	DriveModel *string `json:"driveModel"`
}

// Validate checks the type and its drive model: a Drive needs a model of at
// most maxHostFirmwareDriveModelLength bytes without surrounding whitespace,
// and no other type takes one.
func (r APIHostFirmwareComponentRef) Validate() error {
	if err := validateHostFirmwareComponentType(r.Type); err != nil {
		return validation.Errors{"type": err}
	}
	if r.Type != HostFirmwareComponentTypeDrive {
		if r.DriveModel != nil {
			return validation.Errors{"driveModel": fmt.Errorf("must not be set for component type %q", r.Type)}
		}
		return nil
	}
	switch {
	case r.DriveModel == nil || *r.DriveModel == "":
		return validation.Errors{"driveModel": errors.New(validationErrorValueRequired)}
	case strings.TrimSpace(*r.DriveModel) != *r.DriveModel:
		return validation.Errors{"driveModel": errors.New("must not have leading or trailing whitespace")}
	case len(*r.DriveModel) > maxHostFirmwareDriveModelLength:
		return validation.Errors{"driveModel": fmt.Errorf("must be at most %d bytes", maxHostFirmwareDriveModelLength)}
	}
	return nil
}

// id identifies the component, telling drive models apart.
func (r APIHostFirmwareComponentRef) id() hostFirmwareComponentID {
	id := hostFirmwareComponentID{componentType: r.Type}
	if r.DriveModel != nil {
		id.driveModel = *r.DriveModel
	}
	return id
}

// ToProto converts one ordering entry to its forge form.
func (r APIHostFirmwareComponentRef) ToProto() *corev1.HostFirmwareComponentRef {
	return &corev1.HostFirmwareComponentRef{
		Type:       r.Type.ToProto(),
		DriveModel: r.DriveModel,
	}
}

// FromProto populates one ordering entry from its forge form.
func (r *APIHostFirmwareComponentRef) FromProto(proto *corev1.HostFirmwareComponentRef) {
	if r == nil || proto == nil {
		return
	}
	r.Type.FromProto(proto.GetType())
	r.DriveModel = proto.DriveModel
}

// hostFirmwareComponentID identifies a component; the drive model tells Drive
// components apart.
type hostFirmwareComponentID struct {
	componentType HostFirmwareComponentType
	driveModel    string
}

func (id hostFirmwareComponentID) String() string {
	if id.componentType == HostFirmwareComponentTypeDrive {
		return fmt.Sprintf("Drive with driveModel %q", id.driveModel)
	}
	return fmt.Sprintf("type %q", id.componentType)
}

// hostFirmwareComponentSet rejects a component listed twice.
type hostFirmwareComponentSet map[hostFirmwareComponentID]struct{}

func (s hostFirmwareComponentSet) add(id hostFirmwareComponentID) error {
	if _, ok := s[id]; ok {
		return fmt.Errorf("duplicate component %s", id)
	}
	s[id] = struct{}{}
	return nil
}

// APIHostFirmwareUpgradeScript is a script Scout downloads and runs on the
// host to install a firmware version.
type APIHostFirmwareUpgradeScript struct {
	URL                            string `json:"url"`
	Sha256                         string `json:"sha256"`
	ExecutionTimeoutSeconds        uint32 `json:"executionTimeoutSeconds"`
	ArtifactDownloadTimeoutSeconds uint32 `json:"artifactDownloadTimeoutSeconds"`
}

// Validate checks one upgrade script in an upsert request.
func (s APIHostFirmwareUpgradeScript) Validate() error {
	return validation.ValidateStruct(&s,
		validation.Field(&s.URL, validation.Required.Error(validationErrorValueRequired)),
		validation.Field(&s.Sha256,
			validation.Required.Error(validationErrorValueRequired),
			validation.Match(camu.Sha256LowercaseHexRegex).Error("must be a 64 character lowercase hexadecimal SHA-256 digest")),
		validation.Field(&s.ExecutionTimeoutSeconds, validation.Required.Error("must be greater than zero")),
		validation.Field(&s.ArtifactDownloadTimeoutSeconds, validation.Required.Error("must be greater than zero")),
	)
}

// ToProto converts one upgrade script to its forge form.
func (s APIHostFirmwareUpgradeScript) ToProto() *corev1.HostFirmwareUpgradeScript {
	return &corev1.HostFirmwareUpgradeScript{
		Url:                            s.URL,
		Sha256:                         s.Sha256,
		ExecutionTimeoutSeconds:        s.ExecutionTimeoutSeconds,
		ArtifactDownloadTimeoutSeconds: s.ArtifactDownloadTimeoutSeconds,
	}
}

// APIHostFirmwareArtifact is a firmware artifact location.
type APIHostFirmwareArtifact struct {
	URL    string  `json:"url"`
	Sha256 *string `json:"sha256,omitempty"`
}

// Validate checks one firmware artifact in an upsert request.
func (a APIHostFirmwareArtifact) Validate() error {
	if err := validation.ValidateStruct(&a,
		validation.Field(&a.URL, validation.Required.Error(validationErrorValueRequired)),
	); err != nil {
		return err
	}
	if a.Sha256 != nil {
		if err := validation.Validate(*a.Sha256,
			validation.Match(camu.Sha256LowercaseHexRegex).Error("must be a 64 character lowercase hexadecimal SHA-256 digest"),
		); err != nil {
			return validation.Errors{"sha256": err}
		}
	}
	return nil
}

// APIHostFirmwareVersionConfig describes one firmware version for a component.
type APIHostFirmwareVersionConfig struct {
	Version                     string                    `json:"version"`
	Default                     bool                      `json:"default"`
	Artifacts                   []APIHostFirmwareArtifact `json:"artifacts"`
	InstallOnlySpecified        bool                      `json:"installOnlySpecified"`
	PowerDrainsNeeded           *uint32                   `json:"powerDrainsNeeded,omitempty"`
	PreUpdateResets             bool                      `json:"preUpdateResets"`
	PreingestionExclusiveConfig *bool                     `json:"preingestionExclusiveConfig,omitempty"`
	// UpgradeScript is the script Scout runs to install this version. Without
	// it, Core uses a script packaged for the vendor, model and component; Core
	// rejects a Drive version that has neither.
	UpgradeScript *APIHostFirmwareUpgradeScript `json:"upgradeScript"`
}

// APIHostFirmwareComponentConfig is one component entry in an upsert request.
type APIHostFirmwareComponentConfig struct {
	Type                      HostFirmwareComponentType      `json:"type"`
	DriveModel                *string                        `json:"driveModel,omitempty"`
	Firmware                  []APIHostFirmwareVersionConfig `json:"firmware"`
	PreingestUpgradeWhenBelow *string                        `json:"preingestUpgradeWhenBelow,omitempty"`
}

// APIHostFirmwareComponent is one component entry in a HostFirmwareConfig response.
type APIHostFirmwareComponent struct {
	Type                         HostFirmwareComponentType      `json:"type"`
	DriveModel                   *string                        `json:"driveModel"`
	CurrentVersionDetectionRegEx *string                        `json:"currentVersionDetectionRegEx,omitempty"`
	Firmware                     []APIHostFirmwareVersionConfig `json:"firmware"`
	PreingestUpgradeWhenBelow    *string                        `json:"preingestUpgradeWhenBelow,omitempty"`
}

// APIHostFirmwareConfigCreateOrUpdateRequest is the PUT /firmware-config/host body.
type APIHostFirmwareConfigCreateOrUpdateRequest struct {
	SiteID              string                           `json:"siteId"`
	Vendor              string                           `json:"vendor"`
	Model               string                           `json:"model"`
	Components          []APIHostFirmwareComponentConfig `json:"components"`
	ExplicitStartNeeded *bool                            `json:"explicitStartNeeded,omitempty"`
	// Ordering is the upgrade order, naming each drive model as its own entry.
	Ordering []APIHostFirmwareComponentRef `json:"ordering"`
}

// APIHostFirmwareConfigDeleteRequest is the DELETE /firmware-config/host body.
type APIHostFirmwareConfigDeleteRequest struct {
	SiteID string `json:"siteId"`
	Vendor string `json:"vendor"`
	Model  string `json:"model"`
}

// Validate enforces the REST-layer contract before ToProto.
func (req APIHostFirmwareConfigCreateOrUpdateRequest) Validate() error {
	if err := validation.ValidateStruct(&req,
		validation.Field(&req.SiteID,
			validation.Required.Error(validationErrorValueRequired),
			validationis.UUID.Error(validationErrorInvalidUUID)),
		validation.Field(&req.Vendor, validation.Required.Error(validationErrorValueRequired)),
		validation.Field(&req.Model, validation.Required.Error(validationErrorValueRequired)),
		validation.Field(&req.Components, validation.Required.Error(validationErrorValueRequired), validation.Length(1, 0)),
	); err != nil {
		return err
	}

	components := hostFirmwareComponentSet{}
	for i, component := range req.Components {
		if err := component.Validate(); err != nil {
			return validation.Errors{fmt.Sprintf("components[%d]", i): err}
		}
		if err := components.add(component.ref().id()); err != nil {
			return validation.Errors{fmt.Sprintf("components[%d].type", i): err}
		}
	}

	ordering := hostFirmwareComponentSet{}
	for i, ref := range req.Ordering {
		path := fmt.Sprintf("ordering[%d]", i)
		if err := ref.Validate(); err != nil {
			return validation.Errors{path: err}
		}
		if err := ordering.add(ref.id()); err != nil {
			return validation.Errors{path: err}
		}
	}

	return nil
}

// ToProto converts the validated request to forge.UpsertHostFirmwareConfigRequest.
func (req APIHostFirmwareConfigCreateOrUpdateRequest) ToProto() *corev1.UpsertHostFirmwareConfigRequest {
	protoReq := &corev1.UpsertHostFirmwareConfigRequest{
		Vendor: req.Vendor,
		Model:  req.Model,
	}
	if req.ExplicitStartNeeded != nil {
		protoReq.ExplicitStartNeeded = req.ExplicitStartNeeded
	}

	for _, component := range req.Components {
		protoReq.Components = append(protoReq.Components, component.ToProto())
	}
	for _, ref := range req.Ordering {
		protoReq.Ordering = append(protoReq.Ordering, ref.ToProto())
	}
	return protoReq
}

// ref names the configured component.
func (c APIHostFirmwareComponentConfig) ref() APIHostFirmwareComponentRef {
	return APIHostFirmwareComponentRef{Type: c.Type, DriveModel: c.DriveModel}
}

// Validate checks one component config in an upsert request.
func (c APIHostFirmwareComponentConfig) Validate() error {
	if err := c.ref().Validate(); err != nil {
		return err
	}
	if c.PreingestUpgradeWhenBelow != nil && c.Type == HostFirmwareComponentTypeDrive {
		return validation.Errors{"preingestUpgradeWhenBelow": errors.New("is not supported for a Drive, which pre-ingestion cannot upgrade")}
	}
	if c.PreingestUpgradeWhenBelow != nil && strings.TrimSpace(*c.PreingestUpgradeWhenBelow) == "" {
		return validation.Errors{"preingestUpgradeWhenBelow": errors.New("must not be empty when provided")}
	}
	if len(c.Firmware) == 0 {
		return validation.Errors{"firmware": errors.New("at least one firmware version is required")}
	}
	for i, firmware := range c.Firmware {
		if err := firmware.Validate(); err != nil {
			return validation.Errors{fmt.Sprintf("firmware[%d]", i): err}
		}
		// Every Drive version installs through Scout.
		if c.Type == HostFirmwareComponentTypeDrive {
			if err := firmware.validateScoutArtifacts(); err != nil {
				return validation.Errors{fmt.Sprintf("firmware[%d]", i): err}
			}
		}
	}
	return nil
}

// ToProto converts one upsert component config to its forge form.
func (c APIHostFirmwareComponentConfig) ToProto() *corev1.UpsertHostFirmwareComponentConfig {
	protoComponent := &corev1.UpsertHostFirmwareComponentConfig{
		Type:       c.Type.ToProto(),
		DriveModel: c.DriveModel,
	}
	if c.PreingestUpgradeWhenBelow != nil {
		trimmed := strings.TrimSpace(*c.PreingestUpgradeWhenBelow)
		protoComponent.PreingestUpgradeWhenBelow = &trimmed
	}
	for _, firmware := range c.Firmware {
		protoComponent.Firmware = append(protoComponent.Firmware, firmware.ToProto())
	}
	return protoComponent
}

// Validate checks one firmware version config.
func (v APIHostFirmwareVersionConfig) Validate() error {
	if err := validation.ValidateStruct(&v,
		validation.Field(&v.Version, validation.Required.Error(validationErrorValueRequired)),
		validation.Field(&v.Artifacts, validation.Required.Error(validationErrorValueRequired), validation.Length(1, 0)),
	); err != nil {
		return err
	}
	for i, artifact := range v.Artifacts {
		if err := artifact.Validate(); err != nil {
			return validation.Errors{fmt.Sprintf("artifacts[%d]", i): err}
		}
	}
	if v.UpgradeScript != nil {
		if err := v.UpgradeScript.Validate(); err != nil {
			return validation.Errors{"upgradeScript": err}
		}
		if err := v.validateScoutArtifacts(); err != nil {
			return err
		}
	}
	return nil
}

// validateScoutArtifacts requires a sha256 on every artifact of a version
// Scout installs, since Scout verifies each artifact it downloads. Core also
// applies this to versions it installs with a packaged script.
func (v APIHostFirmwareVersionConfig) validateScoutArtifacts() error {
	for i, artifact := range v.Artifacts {
		if artifact.Sha256 == nil {
			return validation.Errors{fmt.Sprintf("artifacts[%d]", i): validation.Errors{
				"sha256": errors.New("is required for a version Scout installs: one with an upgradeScript, or any Drive version"),
			}}
		}
	}
	return nil
}

// ToProto converts one firmware version config to its forge form.
func (v APIHostFirmwareVersionConfig) ToProto() *corev1.HostFirmwareVersionConfig {
	protoVersion := &corev1.HostFirmwareVersionConfig{
		Version:              v.Version,
		Default:              v.Default,
		InstallOnlySpecified: v.InstallOnlySpecified,
		PreUpdateResets:      v.PreUpdateResets,
	}
	protoVersion.PowerDrainsNeeded = v.PowerDrainsNeeded
	if v.PreingestionExclusiveConfig != nil {
		protoVersion.PreingestionExclusiveConfig = v.PreingestionExclusiveConfig
	}
	for _, artifact := range v.Artifacts {
		protoVersion.Artifacts = append(protoVersion.Artifacts, artifact.ToProto())
	}
	if v.UpgradeScript != nil {
		protoVersion.UpgradeScript = v.UpgradeScript.ToProto()
	}
	return protoVersion
}

// ToProto converts one artifact to its forge form.
func (a APIHostFirmwareArtifact) ToProto() *corev1.HostFirmwareArtifact {
	protoArtifact := &corev1.HostFirmwareArtifact{Url: a.URL}
	if a.Sha256 != nil {
		protoArtifact.Sha256 = a.Sha256
	}
	return protoArtifact
}

// APIHostFirmwareConfig is the HostFirmwareConfig response body.
type APIHostFirmwareConfig struct {
	Vendor              string                     `json:"vendor"`
	Model               string                     `json:"model"`
	Components          []APIHostFirmwareComponent `json:"components"`
	ExplicitStartNeeded bool                       `json:"explicitStartNeeded"`
	// Ordering is the upgrade order, naming each drive model as its own entry.
	Ordering []APIHostFirmwareComponentRef `json:"ordering"`
	Created  time.Time                     `json:"created"`
	Updated  time.Time                     `json:"updated"`
}

// FromProto populates the response from forge.HostFirmwareConfigResponse.
func (resp *APIHostFirmwareConfig) FromProto(proto *corev1.HostFirmwareConfigResponse) {
	if resp == nil || proto == nil {
		return
	}
	resp.Vendor = proto.GetVendor()
	resp.Model = proto.GetModel()
	resp.ExplicitStartNeeded = proto.GetExplicitStartNeeded()

	resp.Ordering = make([]APIHostFirmwareComponentRef, 0, len(proto.GetOrdering()))
	for _, protoRef := range proto.GetOrdering() {
		if protoRef == nil {
			continue
		}
		ref := APIHostFirmwareComponentRef{}
		ref.FromProto(protoRef)
		resp.Ordering = append(resp.Ordering, ref)
	}

	resp.Components = make([]APIHostFirmwareComponent, 0, len(proto.GetComponents()))
	for _, component := range proto.GetComponents() {
		if component == nil {
			continue
		}
		apiComponent := APIHostFirmwareComponent{}
		apiComponent.FromProto(component)
		resp.Components = append(resp.Components, apiComponent)
	}

	if ts := proto.GetCreatedAt(); ts != nil {
		resp.Created = ts.AsTime().UTC()
	}
	if ts := proto.GetUpdatedAt(); ts != nil {
		resp.Updated = ts.AsTime().UTC()
	}
}

// FromProto populates one response component from forge.HostFirmwareComponentConfigResponse.
func (c *APIHostFirmwareComponent) FromProto(proto *corev1.HostFirmwareComponentConfigResponse) {
	if c == nil || proto == nil {
		return
	}
	c.Type.FromProto(proto.GetType())
	c.DriveModel = proto.DriveModel
	if v := proto.CurrentVersionReportedAs; v != nil {
		c.CurrentVersionDetectionRegEx = v
	}
	if v := proto.PreingestUpgradeWhenBelow; v != nil {
		c.PreingestUpgradeWhenBelow = v
	}
	for _, firmware := range proto.GetFirmware() {
		if firmware == nil {
			continue
		}
		apiFirmware := APIHostFirmwareVersionConfig{}
		apiFirmware.FromProto(firmware)
		c.Firmware = append(c.Firmware, apiFirmware)
	}
}

// FromProto populates one firmware version from forge.HostFirmwareVersionConfig.
func (v *APIHostFirmwareVersionConfig) FromProto(proto *corev1.HostFirmwareVersionConfig) {
	if v == nil || proto == nil {
		return
	}
	v.Version = proto.GetVersion()
	v.Default = proto.GetDefault()
	v.InstallOnlySpecified = proto.GetInstallOnlySpecified()
	v.PreUpdateResets = proto.GetPreUpdateResets()
	if proto.PreingestionExclusiveConfig != nil {
		v.PreingestionExclusiveConfig = proto.PreingestionExclusiveConfig
	}
	v.PowerDrainsNeeded = proto.PowerDrainsNeeded
	for _, artifact := range proto.GetArtifacts() {
		if artifact == nil {
			continue
		}
		apiArtifact := APIHostFirmwareArtifact{URL: artifact.GetUrl()}
		if sha256 := artifact.Sha256; sha256 != nil {
			apiArtifact.Sha256 = sha256
		}
		v.Artifacts = append(v.Artifacts, apiArtifact)
	}
	if script := proto.GetUpgradeScript(); script != nil {
		v.UpgradeScript = &APIHostFirmwareUpgradeScript{
			URL:                            script.GetUrl(),
			Sha256:                         script.GetSha256(),
			ExecutionTimeoutSeconds:        script.GetExecutionTimeoutSeconds(),
			ArtifactDownloadTimeoutSeconds: script.GetArtifactDownloadTimeoutSeconds(),
		}
	}
}

// IsCreated reports whether the object was created (rather than updated) by the upserting call.
func (resp *APIHostFirmwareConfig) IsCreated() bool {
	if resp == nil || resp.Created.IsZero() || resp.Updated.IsZero() {
		return false
	}
	return resp.Created.Equal(resp.Updated)
}

func validateHostFirmwareComponentType(componentType HostFirmwareComponentType) error {
	if _, ok := hostFirmwareComponentTypeChoiceMap[componentType]; !ok {
		return fmt.Errorf("invalid component type %q", componentType)
	}
	return nil
}

// Validate enforces the REST-layer contract before ToProto.
func (req APIHostFirmwareConfigDeleteRequest) Validate() error {
	return validation.ValidateStruct(&req,
		validation.Field(&req.SiteID,
			validation.Required.Error(validationErrorValueRequired),
			validationis.UUID.Error(validationErrorInvalidUUID)),
		validation.Field(&req.Vendor, validation.Required.Error(validationErrorValueRequired)),
		validation.Field(&req.Model, validation.Required.Error(validationErrorValueRequired)),
	)
}

// ToProto converts the validated request to forge.DeleteHostFirmwareConfigRequest.
func (req APIHostFirmwareConfigDeleteRequest) ToProto() *corev1.DeleteHostFirmwareConfigRequest {
	return &corev1.DeleteHostFirmwareConfigRequest{
		Vendor: req.Vendor,
		Model:  req.Model,
	}
}
