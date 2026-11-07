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
}

// APIHostFirmwareComponentConfig is one component entry in an upsert request.
type APIHostFirmwareComponentConfig struct {
	Type                      HostFirmwareComponentType      `json:"type"`
	Firmware                  []APIHostFirmwareVersionConfig `json:"firmware"`
	PreingestUpgradeWhenBelow *string                        `json:"preingestUpgradeWhenBelow,omitempty"`
}

// APIHostFirmwareComponent is one component entry in a HostFirmwareConfig response.
type APIHostFirmwareComponent struct {
	Type                         HostFirmwareComponentType      `json:"type"`
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
	Ordering            []HostFirmwareComponentType      `json:"ordering"`
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

	seenComponents := make(map[HostFirmwareComponentType]struct{}, len(req.Components))
	for i, component := range req.Components {
		if err := component.Validate(); err != nil {
			return validation.Errors{fmt.Sprintf("components[%d]", i): err}
		}
		if _, ok := seenComponents[component.Type]; ok {
			return validation.Errors{
				fmt.Sprintf("components[%d].type", i): fmt.Errorf("duplicate component type %q", component.Type),
			}
		}
		seenComponents[component.Type] = struct{}{}
	}

	seenOrdering := make(map[HostFirmwareComponentType]struct{}, len(req.Ordering))
	for i, componentType := range req.Ordering {
		if err := validateHostFirmwareComponentType(componentType); err != nil {
			return validation.Errors{fmt.Sprintf("ordering[%d]", i): err}
		}
		if _, ok := seenOrdering[componentType]; ok {
			return validation.Errors{
				fmt.Sprintf("ordering[%d]", i): fmt.Errorf("duplicate component type %q", componentType),
			}
		}
		seenOrdering[componentType] = struct{}{}
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
	for _, componentType := range req.Ordering {
		protoReq.Ordering = append(protoReq.Ordering, componentType.ToProto())
	}
	return protoReq
}

// Validate checks one component config in an upsert request.
func (c APIHostFirmwareComponentConfig) Validate() error {
	if err := validateHostFirmwareComponentType(c.Type); err != nil {
		return validation.Errors{"type": err}
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
	}
	return nil
}

// ToProto converts one upsert component config to its forge form.
func (c APIHostFirmwareComponentConfig) ToProto() *corev1.UpsertHostFirmwareComponentConfig {
	protoComponent := &corev1.UpsertHostFirmwareComponentConfig{
		Type: c.Type.ToProto(),
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
	Vendor              string                      `json:"vendor"`
	Model               string                      `json:"model"`
	Components          []APIHostFirmwareComponent  `json:"components"`
	ExplicitStartNeeded bool                        `json:"explicitStartNeeded"`
	Ordering            []HostFirmwareComponentType `json:"ordering"`
	Created             time.Time                   `json:"created"`
	Updated             time.Time                   `json:"updated"`
}

// FromProto populates the response from forge.HostFirmwareConfigResponse.
func (resp *APIHostFirmwareConfig) FromProto(proto *corev1.HostFirmwareConfigResponse) {
	if resp == nil || proto == nil {
		return
	}
	resp.Vendor = proto.GetVendor()
	resp.Model = proto.GetModel()
	resp.ExplicitStartNeeded = proto.GetExplicitStartNeeded()

	resp.Ordering = make([]HostFirmwareComponentType, 0, len(proto.GetOrdering()))
	for _, componentType := range proto.GetOrdering() {
		var restType HostFirmwareComponentType
		restType.FromProto(componentType)
		resp.Ordering = append(resp.Ordering, restType)
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
