// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"fmt"

	validation "github.com/go-ozzo/ozzo-validation/v4"
	"google.golang.org/protobuf/types/known/emptypb"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// APISpectrumXMultiplaneRange selects inclusive, ascending rail or plane bounds.
type APISpectrumXMultiplaneRange struct {
	// Start is the first value of the range, inclusive
	Start *int `json:"start"`
	// End is the last value of the range, inclusive. It must be greater than or equal to Start.
	End *int `json:"end"`
}

// APIMachineSpectrumXMultiplaneUpdateRequest changes the Cartesian product of
// exactly one rail selector and one plane selector.
// MaintenanceMode=true disables the selected links; false enables them.
type APIMachineSpectrumXMultiplaneUpdateRequest struct {
	// MachineID is the ID of the Machine whose planes are updated. It comes from the request path, not the body.
	MachineID string `json:"-"`
	// Rail selects a single rail (0-3). Exactly one of Rail, AllRails, or RailRange is required.
	Rail *int `json:"rail"`
	// AllRails selects every rail when true. If present, it must be true.
	AllRails *bool `json:"allRails"`
	// RailRange selects an inclusive range of rails, each between 0 and 3
	RailRange *APISpectrumXMultiplaneRange `json:"railRange"`
	// Plane selects a single plane (0-7). Exactly one of Plane, AllPlanes, or PlaneRange is required.
	Plane *int `json:"plane"`
	// AllPlanes selects every plane when true. If present, it must be true.
	AllPlanes *bool `json:"allPlanes"`
	// PlaneRange selects an inclusive range of planes, each between 0 and 7
	PlaneRange *APISpectrumXMultiplaneRange `json:"planeRange"`
	// MaintenanceMode is required. True puts the selected planes in maintenance mode (disabled), false takes them out of it (enabled).
	MaintenanceMode *bool `json:"maintenanceMode"`
	// Reference is an optional reference for the change, such as a ticket URL.
	// Omission or null preserves the stored reference; an empty string clears it.
	Reference *string `json:"reference"`
	// AcknowledgeAttachedInstance must be true when an Instance is attached to the Machine.
	// Maintenance changes may disrupt the Instance's Tenant workload.
	AcknowledgeAttachedInstance *bool `json:"acknowledgeAttachedInstance"`
}

// Validate rejects missing, ambiguous, or out-of-bounds selections before proxying.
func (r *APIMachineSpectrumXMultiplaneUpdateRequest) Validate() error {
	return validation.ValidateStruct(r,
		validation.Field(&r.Rail, validation.By(r.validateRailSelector)),
		validation.Field(&r.Plane, validation.By(r.validatePlaneSelector)),
		validation.Field(&r.MaintenanceMode, validation.NotNil.Error("maintenanceMode is required")),
	)
}

func (r *APIMachineSpectrumXMultiplaneUpdateRequest) validateRailSelector(_ interface{}) error {
	return validateMultiplaneSelection(r.Rail, r.AllRails, r.RailRange, 3)
}

func (r *APIMachineSpectrumXMultiplaneUpdateRequest) validatePlaneSelector(_ interface{}) error {
	return validateMultiplaneSelection(r.Plane, r.AllPlanes, r.PlaneRange, 7)
}

func validateMultiplaneSelection(index *int, all *bool, bounds *APISpectrumXMultiplaneRange, maximum int) error {
	count := 0
	for _, present := range []bool{index != nil, all != nil, bounds != nil} {
		if present {
			count++
		}
	}
	if count != 1 {
		return fmt.Errorf("exactly one individual, all, or range selector is required")
	}
	if all != nil && !*all {
		return fmt.Errorf("all selector must be true")
	}
	if index != nil {
		return validation.Validate(index, validation.Min(0), validation.Max(maximum))
	}
	if bounds != nil {
		err := validation.ValidateStruct(bounds,
			validation.Field(&bounds.Start, validation.NotNil, validation.Min(0), validation.Max(maximum)),
			validation.Field(&bounds.End, validation.NotNil, validation.Min(0), validation.Max(maximum)),
		)
		if err != nil {
			return err
		}
		if *bounds.Start > *bounds.End {
			return validation.Errors{"end": fmt.Errorf("must be greater than or equal to start")}
		}
	}
	return nil
}

// ToProto preserves explicit zero selections, false maintenance mode, and reference presence.
func (r APIMachineSpectrumXMultiplaneUpdateRequest) ToProto() *corev1.MachineSpxMultiplaneMaintenanceUpdateRequest {
	req := &corev1.MachineSpxMultiplaneMaintenanceUpdateRequest{
		MachineId: &corev1.MachineId{Id: r.MachineID}, MaintenanceMode: r.MaintenanceMode, Reference: r.Reference,
	}
	switch {
	case r.Rail != nil:
		req.RailSelector = &corev1.MachineSpxMultiplaneMaintenanceUpdateRequest_Rail{Rail: uint32(*r.Rail)}
	case r.AllRails != nil:
		req.RailSelector = &corev1.MachineSpxMultiplaneMaintenanceUpdateRequest_AllRails{AllRails: &emptypb.Empty{}}
	case r.RailRange != nil:
		req.RailSelector = &corev1.MachineSpxMultiplaneMaintenanceUpdateRequest_RailRange{RailRange: r.RailRange.ToProto()}
	}
	switch {
	case r.Plane != nil:
		req.PlaneSelector = &corev1.MachineSpxMultiplaneMaintenanceUpdateRequest_Plane{Plane: uint32(*r.Plane)}
	case r.AllPlanes != nil:
		req.PlaneSelector = &corev1.MachineSpxMultiplaneMaintenanceUpdateRequest_AllPlanes{AllPlanes: &emptypb.Empty{}}
	case r.PlaneRange != nil:
		req.PlaneSelector = &corev1.MachineSpxMultiplaneMaintenanceUpdateRequest_PlaneRange{PlaneRange: r.PlaneRange.ToProto()}
	}
	return req
}

// ToProto converts already-validated inclusive bounds.
func (r APISpectrumXMultiplaneRange) ToProto() *corev1.SpxMultiplaneRange {
	start, end := uint32(*r.Start), uint32(*r.End)
	return &corev1.SpxMultiplaneRange{Start: &start, End: &end}
}

// APIMachineSpectrumXMultiplane contains desired host configuration and separate DPU observations.
type APIMachineSpectrumXMultiplane struct {
	// MachineID is the ID of the Machine
	MachineID string `json:"machineId"`
	// Interfaces is the desired configuration of each declared rail/plane interface of the Machine
	Interfaces []APIMachineSpectrumXMultiplaneInterface `json:"interfaces"`
	// DPUStatuses is the observed state reported by each associated BlueField-4 DPU
	DPUStatuses []APIMachineSpectrumXMultiplaneDPUStatus `json:"dpuStatuses"`
}

// APIMachineSpectrumXMultiplaneInterface is one declared rail/plane's desired configuration.
type APIMachineSpectrumXMultiplaneInterface struct {
	// LogicalName is the logical name of the interface, in the form eth_r<rail>_p<plane>
	LogicalName string `json:"logicalName"`
	// Rail is the rail number of the interface (0-3)
	Rail uint32 `json:"rail"`
	// Plane is the plane number of the interface (0-7)
	Plane uint32 `json:"plane"`
	// ConfiguredMaintenanceMode is the requested mode: true for maintenance mode (disabled), false for enabled.
	// Null means no mode has been requested, so the DPU observes the plane without changing its state.
	ConfiguredMaintenanceMode *bool `json:"configuredMaintenanceMode"`
	// Reference is the reference, such as a ticket URL, recorded with the last change. Null if none was recorded.
	Reference *string `json:"reference"`
	// Revision identifies the version of this interface's configuration
	Revision string `json:"revision"`
}

// APIMachineSpectrumXMultiplaneDPUStatus keeps readback separate for each associated BlueField-4 DPU.
type APIMachineSpectrumXMultiplaneDPUStatus struct {
	// DPUMachineID is the ID of the BlueField-4 DPU that reported these observations
	DPUMachineID string `json:"dpuMachineId"`
	// Interfaces is the DPU's observation of each interface
	Interfaces []APIMachineSpectrumXMultiplaneObservation `json:"interfaces"`
}

// APIMachineSpectrumXMultiplaneObservation describes an attempt, which may precede the desired revision.
type APIMachineSpectrumXMultiplaneObservation struct {
	// LogicalName is the logical name of the interface, in the form eth_r<rail>_p<plane>
	LogicalName string `json:"logicalName"`
	// MaintenanceMode is the mode the DPU observed on the plane: true for maintenance mode, false for enabled.
	// Null if the DPU has not reported a mode.
	MaintenanceMode *bool `json:"maintenanceMode"`
	// Revision is the configuration revision of the DPU's attempt, which may be older than the desired revision
	Revision string `json:"revision"`
	// Netdev is the network device name the DPU resolved for the interface. Null if none was resolved.
	Netdev *string `json:"netdev"`
	// Error describes why the DPU could not apply or observe the interface. Null if there was no error.
	Error *string `json:"error"`
	// Observed is when the DPU made the observation. Null if it has not reported.
	Observed *string `json:"observed"`
}

// FromProto copies the authoritative response, retaining null values and empty arrays.
func (m *APIMachineSpectrumXMultiplane) FromProto(in *corev1.MachineSpxMultiplane) {
	m.MachineID = in.GetMachineId().GetId()
	m.Interfaces = make([]APIMachineSpectrumXMultiplaneInterface, 0, len(in.GetInterfaces()))
	for _, item := range in.GetInterfaces() {
		var iface APIMachineSpectrumXMultiplaneInterface
		iface.FromProto(item)
		m.Interfaces = append(m.Interfaces, iface)
	}
	m.DPUStatuses = make([]APIMachineSpectrumXMultiplaneDPUStatus, 0, len(in.GetDpuStatuses()))
	for _, item := range in.GetDpuStatuses() {
		var status APIMachineSpectrumXMultiplaneDPUStatus
		status.FromProto(item)
		m.DPUStatuses = append(m.DPUStatuses, status)
	}
}

// FromProto copies a declared interface without inferring a configured mode from observations.
func (m *APIMachineSpectrumXMultiplaneInterface) FromProto(in *corev1.MachineSpxMultiplaneInterface) {
	*m = APIMachineSpectrumXMultiplaneInterface{}
	if in == nil {
		return
	}
	m.LogicalName, m.Rail, m.Plane = in.GetLogicalIfname(), in.GetRail(), in.GetPlane()
	m.ConfiguredMaintenanceMode, m.Reference, m.Revision = in.CfgMaintenanceMode, in.Reference, in.GetRevision()
}

// FromProto copies each DPU's observations in the order supplied by the site.
func (m *APIMachineSpectrumXMultiplaneDPUStatus) FromProto(in *corev1.MachineSpxMultiplaneDpuStatus) {
	m.DPUMachineID = in.GetDpuMachineId().GetId()
	m.Interfaces = make([]APIMachineSpectrumXMultiplaneObservation, 0, len(in.GetInterfaces()))
	for _, item := range in.GetInterfaces() {
		var observation APIMachineSpectrumXMultiplaneObservation
		observation.FromProto(item)
		m.Interfaces = append(m.Interfaces, observation)
	}
}

// FromProto preserves unknown readback as null rather than treating it as maintenance off.
func (m *APIMachineSpectrumXMultiplaneObservation) FromProto(in *corev1.AstraMultiplaneInterfaceStatus) {
	*m = APIMachineSpectrumXMultiplaneObservation{}
	if in == nil {
		return
	}
	m.LogicalName, m.Revision = in.GetLogicalIfname(), in.GetRevision()
	m.MaintenanceMode, m.Netdev, m.Error = in.MaintenanceMode, in.Netdev, in.Error
	m.Observed = cutil.ProtoTimePtrToStrPtr(in.GetObservedAt())
}
