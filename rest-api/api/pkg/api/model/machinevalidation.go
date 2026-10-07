// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model/util"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	validation "github.com/go-ozzo/ozzo-validation/v4"
)

// APIMachineValidationTest data structure to capture MachineValidation test
type APIMachineValidationTest struct {
	// TestID id of the test
	TestID string `json:"testID"`
	// Name test name
	Name string `json:"name"`
	// Description test description
	Description string `json:"description"`
	// Contexts list of test contexts
	Contexts []string `json:"contexts"`
	// ImgName test container image name
	ContainerImgName string `json:"containerImgName"`
	// IsExecuteInHost indicates to run test command using chroot in case of container
	IsExecuteInHost bool `json:"isExecuteInHost"`
	// ContainerArgs test container arguments
	ContainerArgs string `json:"containerArgs"`
	// Command test command
	Command string `json:"command"`
	// Args test arguments
	Args string `json:"args"`
	// ExtraErrFile test command error output file
	ExtraErrFile string `json:"extraErrFile"`
	// ExternalConfigFile test external configuration file
	ExternalConfigFile string `json:"externalConfigFile"`
	// PreCondition test pre-condition
	PreCondition string `json:"preCondition"`
	// Timeout test timeout in seconds (default 7200)
	Timeout int64 `json:"timeout"`
	// ExtraOutputFile test command standard output file
	ExtraOutputFile string `json:"extraOutputFile"`
	// Version test version
	Version string `json:"version"`
	// SupportedPlatforms list of supported platform for a test
	SupportedPlatforms []string `json:"supportedPlatforms"`
	// ModifiedBy user that last modified test
	ModifiedBy string `json:"modifiedBy"`
	// IsVerified indicates if test verified or not
	IsVerified bool `json:"isVerified"`
	// ReadOnly indicates if the test is read-ony or not
	IsReadOnly bool `json:"isReadOnly"`
	// CustomTags list of custom tags for test
	CustomTags []string `json:"customTags"`
	// Components list of system components for test
	Components []string `json:"components"`
	// LastModifiedAt last time test modified
	LastModifiedAt string `json:"lastModifiedAt"`
	// IsEnabled indicates if test is enabled or not
	IsEnabled bool `json:"isEnabled"`
}

func NewAPIMachineValidationTest(proto *corev1.MachineValidationTest) *APIMachineValidationTest {
	return &APIMachineValidationTest{
		TestID:             proto.GetTestId(),
		Name:               proto.GetName(),
		Description:        proto.GetDescription(),
		Contexts:           proto.GetContexts(),
		ContainerImgName:   proto.GetImgName(),
		IsExecuteInHost:    proto.GetExecuteInHost(),
		ContainerArgs:      proto.GetContainerArg(),
		Command:            proto.GetCommand(),
		Args:               proto.GetArgs(),
		ExtraErrFile:       proto.GetExtraErrFile(),
		ExternalConfigFile: proto.GetExternalConfigFile(),
		PreCondition:       proto.GetPreCondition(),
		Timeout:            proto.GetTimeout(),
		ExtraOutputFile:    proto.GetExtraOutputFile(),
		Version:            proto.GetVersion(),
		SupportedPlatforms: proto.GetSupportedPlatforms(),
		ModifiedBy:         proto.GetModifiedBy(),
		IsVerified:         proto.GetVerified(),
		IsReadOnly:         proto.GetReadOnly(),
		CustomTags:         proto.GetCustomTags(),
		Components:         proto.GetComponents(),
		LastModifiedAt:     proto.GetLastModifiedAt(),
		IsEnabled:          proto.GetIsEnabled(),
	}
}

type APIMachineValidationTestCreateRequest struct {
	// Name test name
	Name string `json:"name"`
	// Command test command
	Command string `json:"command"`
	// Args test command arguments
	Args string `json:"args"`
	// Description test description
	Description *string `json:"description"`
	// Contexts list of contexts for a test
	Contexts []string `json:"contexts"`
	// ContainerImgName test container image name
	ContainerImgName *string `json:"containerImgName"`
	// ExecuteInHost run test command using chroot in case of container
	IsExecuteInHost *bool `json:"isExecuteInHost"`
	// ContainerArgs test container arguments
	ContainerArgs *string `json:"containerArgs"`
	// ExtraErrFile test command error output file
	ExtraErrFile *string `json:"extraErrFile"`
	// ExternalConfigFile test external configuration file
	ExternalConfigFile *string `json:"externalConfigFile"`
	// PreCondition test pre-condition
	PreCondition *string `json:"preCondition"`
	// Timeout test command timeout in seconds (default 7200)
	Timeout *int64 `json:"timeout"`
	// ExtraOutputFile test command standard output file
	ExtraOutputFile *string `json:"extraOutputFile"`
	// SupportedPlatforms list of supported platform for a test
	SupportedPlatforms []string `json:"supportedPlatforms"`
	// IsReadOnly indicates if test is read-only or not
	IsReadOnly *bool `json:"isReadOnly"`
	// CustomTags list of custom tags for a test
	CustomTags []string `json:"customTags"`
	// Components list of system components for a test
	Components []string `json:"components"`
	// IsEnabled indicates if test is enabled or not
	IsEnabled *bool `json:"isEnabled"`
}

// Validate ensures that the values passed in request are acceptable
func (req APIMachineValidationTestCreateRequest) Validate() error {
	err := validation.ValidateStruct(&req,
		validation.Field(&req.Name,
			validation.Required.Error(validationErrorStringLength64),
			validation.By(util.ValidateNameCharacters),
			validation.Length(2, 64).Error(validationErrorStringLength64)),
		validation.Field(&req.Command,
			validation.Required.Error(validationErrorStringLength),
			validation.By(util.ValidateNameCharacters),
			validation.Length(2, 256).Error(validationErrorStringLength)),
		validation.Field(&req.Args,
			validation.Required.Error(validationErrorValueRequired),
			validation.By(util.ValidateNameCharacters)),
	)
	if err != nil {
		return err
	}
	return nil
}

func (req APIMachineValidationTestCreateRequest) ToProto() *corev1.MachineValidationTestAddRequest {
	return &corev1.MachineValidationTestAddRequest{
		Name:               req.Name,
		Command:            req.Command,
		Args:               req.Args,
		Description:        req.Description,
		Contexts:           req.Contexts,
		ImgName:            req.ContainerImgName,
		ExecuteInHost:      req.IsExecuteInHost,
		ContainerArg:       req.ContainerArgs,
		ExtraErrFile:       req.ExtraErrFile,
		ExternalConfigFile: req.ExternalConfigFile,
		PreCondition:       req.PreCondition,
		Timeout:            req.Timeout,
		ExtraOutputFile:    req.ExtraOutputFile,
		SupportedPlatforms: req.SupportedPlatforms,
		ReadOnly:           req.IsReadOnly,
		CustomTags:         req.CustomTags,
		Components:         req.Components,
		IsEnabled:          req.IsEnabled,
	}
}

type APIMachineValidationTestUpdateRequest struct {
	// Name test name
	Name *string `json:"name"`
	// Command test command
	Command *string `json:"command"`
	// Args test command arguments
	Args *string `json:"args"`
	// Description test description
	Description *string `json:"description"`
	// Contexts list of contexts for a test
	Contexts []string `json:"contexts"`
	// ContainerImgName test container image name
	ContainerImgName *string `json:"containerImgName"`
	// ExecuteInHost run test command using chroot in case of container
	IsExecuteInHost *bool `json:"isExecuteInHost"`
	// ContainerArgs test container arguments
	ContainerArgs *string `json:"containerArgs"`
	// ExtraErrFile test command error output file
	ExtraErrFile *string `json:"extraErrFile"`
	// ExternalConfigFile test external configuration file
	ExternalConfigFile *string `json:"externalConfigFile"`
	// PreCondition test pre-condition
	PreCondition *string `json:"preCondition"`
	// Timeout test command timeout in seconds (default 7200)
	Timeout *int64 `json:"timeout"`
	// ExtraOutputFile test command standard output file
	ExtraOutputFile *string `json:"extraOutputFile"`
	// SupportedPlatforms list of supported platform for a test
	SupportedPlatforms []string `json:"supportedPlatforms"`
	// IsVerified indicates if test verified or not
	IsVerified *bool `json:"isVerified"`
	// CustomTags list of custom tags for a test
	CustomTags []string `json:"customTags"`
	// Components list of system components for a test
	Components []string `json:"components"`
	// IsEnabled indicates if test is enabled or not
	IsEnabled *bool `json:"isEnabled"`
}

func (req APIMachineValidationTestUpdateRequest) ToProto(testID string, testVersion string) *corev1.MachineValidationTestUpdateRequest {
	return &corev1.MachineValidationTestUpdateRequest{
		TestId:  testID,
		Version: testVersion,
		Payload: &corev1.MachineValidationTestUpdateRequest_Payload{
			Name:               req.Name,
			Command:            req.Command,
			Args:               req.Args,
			Description:        req.Description,
			Contexts:           req.Contexts,
			ImgName:            req.ContainerImgName,
			ExecuteInHost:      req.IsExecuteInHost,
			ContainerArg:       req.ContainerArgs,
			ExtraErrFile:       req.ExtraErrFile,
			ExternalConfigFile: req.ExternalConfigFile,
			PreCondition:       req.PreCondition,
			Timeout:            req.Timeout,
			ExtraOutputFile:    req.ExtraOutputFile,
			SupportedPlatforms: req.SupportedPlatforms,
			Verified:           req.IsVerified,
			CustomTags:         req.CustomTags,
			Components:         req.Components,
			IsEnabled:          req.IsEnabled,
		},
	}
}

type APIMachineValidationTestsFilter struct {
	// SupportedPlatforms list of supported platform for a test
	SupportedPlatforms []string `query:"supportedPlatforms"`
	// Contexts list of contexts for a test
	Contexts []string `query:"contexts"`
	// IsReadOnly indicates if test is read-only or not
	IsReadOnly *bool `query:"isReadOnly"`
	// CustomTags list of custom tags for a test
	CustomTags []string `query:"customTags"`
	// IsEnabled indicates if test is enabled or not
	IsEnabled *bool `query:"isEnabled"`
	// IsVerified indicates if test verified or not
	IsVerified *bool `query:"isVerified"`
}

func (filter APIMachineValidationTestsFilter) ToProto() *corev1.MachineValidationTestsGetRequest {
	return &corev1.MachineValidationTestsGetRequest{
		SupportedPlatforms: filter.SupportedPlatforms,
		Contexts:           filter.Contexts,
		ReadOnly:           filter.IsReadOnly,
		CustomTags:         filter.CustomTags,
		IsEnabled:          filter.IsEnabled,
		Verified:           filter.IsVerified,
	}
}

type APIMachineValidationResult struct {
	Name         string    `json:"name"`
	Description  string    `json:"description"`
	Command      string    `json:"command"`
	Args         string    `json:"args"`
	StdOut       string    `json:"stdOut"`
	StdErr       string    `json:"stdErr"`
	Context      string    `json:"context"`
	ExitCode     int       `json:"exitCode"`
	StartTime    time.Time `json:"startTime"`
	EndTime      time.Time `json:"endTime"`
	ValidationID string    `json:"validationID"`
	TestID       string    `json:"testID"`
}

func NewAPIMachineValidationResult(proto *corev1.MachineValidationResult) *APIMachineValidationResult {
	apio := &APIMachineValidationResult{
		Name:         proto.GetName(),
		Description:  proto.GetDescription(),
		Command:      proto.GetCommand(),
		Args:         proto.GetArgs(),
		StdOut:       proto.GetStdOut(),
		StdErr:       proto.GetStdErr(),
		Context:      proto.GetContext(),
		ExitCode:     int(proto.GetExitCode()),
		ValidationID: proto.GetValidationId().GetValue(),
		TestID:       proto.GetTestId(),
	}
	if proto.GetStartTime() != nil {
		apio.StartTime = proto.GetStartTime().AsTime()
	}
	if proto.GetEndTime() != nil {
		apio.EndTime = proto.GetEndTime().AsTime()
	}
	return apio
}

type APIMachineValidationRun struct {
	ValidationID           string                     `json:"validationID"`
	MachineID              string                     `json:"machineID"`
	StartTime              time.Time                  `json:"startTime"`
	EndTime                *time.Time                 `json:"endTime"`
	Name                   string                     `json:"name"`
	Context                string                     `json:"context"`
	Status                 APIMachineValidationStatus `json:"status"`
	DurationToCompleteSecs int64                      `json:"durationToCompleteSecs"`
}

type APIMachineValidationState string

const (
	MachineValidationStarted    APIMachineValidationState = "Started"
	MachineValidationInProgress APIMachineValidationState = "InProgress"
	MachineValidationSuccess    APIMachineValidationState = "Success"
	MachineValidationFailed     APIMachineValidationState = "Failed"
	MachineValidationSkipped    APIMachineValidationState = "Skipped"
)

type APIMachineValidationStatus struct {
	State     APIMachineValidationState `json:"state"`
	Total     uint32                    `json:"total"`
	Completed uint32                    `json:"completed"`
}

func NewAPIMachineValidationRun(proto *corev1.MachineValidationRun) *APIMachineValidationRun {
	apio := &APIMachineValidationRun{
		ValidationID: proto.GetValidationId().GetValue(),
		MachineID:    proto.GetMachineId().GetId(),
		Name:         proto.GetName(),
		Context:      proto.GetContext(),
	}
	if protoStart := proto.GetStartTime(); protoStart != nil {
		apio.StartTime = protoStart.AsTime()
	}
	if protoEnd := proto.GetEndTime(); protoEnd != nil {
		endTime := protoEnd.AsTime()
		apio.EndTime = &endTime
	}
	if protoStatus := proto.GetStatus(); protoStatus != nil {
		apio.Status = APIMachineValidationStatus{
			Total:     protoStatus.GetTotal(),
			Completed: protoStatus.GetCompletedTests(),
		}
		if sts, ok := protoStatus.GetMachineValidationState().(*corev1.MachineValidationStatus_Started); ok {
			if sts.Started == corev1.MachineValidationStarted_Started {
				apio.Status.State = MachineValidationStarted
			}
		} else if sts, ok := protoStatus.GetMachineValidationState().(*corev1.MachineValidationStatus_InProgress); ok {
			if sts.InProgress == corev1.MachineValidationInProgress_InProgress {
				apio.Status.State = MachineValidationInProgress
			}
		} else if sts, ok := protoStatus.GetMachineValidationState().(*corev1.MachineValidationStatus_Completed); ok {
			if sts.Completed == corev1.MachineValidationCompleted_Success {
				apio.Status.State = MachineValidationSuccess
			} else if sts.Completed == corev1.MachineValidationCompleted_Failed {
				apio.Status.State = MachineValidationFailed
			} else if sts.Completed == corev1.MachineValidationCompleted_Skipped {
				apio.Status.State = MachineValidationSkipped
			}
		}
	}
	if protoDuration := proto.GetDurationToComplete(); protoDuration != nil {
		apio.DurationToCompleteSecs = protoDuration.GetSeconds()
	}
	return apio
}

// APIMachineValidationRunCreateRequest contains optional filters for an
// on-demand Machine validation run.
type APIMachineValidationRunCreateRequest struct {
	Tags               []string `json:"tags,omitempty"`
	AllowedTests       []string `json:"allowedTests,omitempty"`
	RunUnverifiedTests bool     `json:"runUnverifiedTests,omitempty"`
	Contexts           []string `json:"contexts,omitempty"`
}

// Validate ensures the on-demand Machine validation filters are well-formed.
func (r APIMachineValidationRunCreateRequest) Validate() error {
	return validation.ValidateStruct(&r,
		validation.Field(&r.Tags, validation.Each(validation.Required)),
		validation.Field(&r.AllowedTests, validation.Each(validation.Required)),
		validation.Field(&r.Contexts, validation.Each(validation.Required)),
	)
}

// ToProto converts an on-demand Machine validation request to the Core API model.
func (r APIMachineValidationRunCreateRequest) ToProto(machineID string) *corev1.MachineValidationOnDemandRequest {
	return &corev1.MachineValidationOnDemandRequest{
		MachineId:         &corev1.MachineId{Id: machineID},
		Tags:              r.Tags,
		Action:            corev1.MachineValidationOnDemandRequest_Start,
		AllowedTests:      r.AllowedTests,
		RunUnverfiedTests: r.RunUnverifiedTests,
		Contexts:          r.Contexts,
	}
}

// NewAPIMachineValidationRunFromOnDemandResponse converts the Core response to
// the REST API model. The validation ID fallback supports older Core versions
// that do not populate the run field.
func NewAPIMachineValidationRunFromOnDemandResponse(response *corev1.MachineValidationOnDemandResponse) *APIMachineValidationRun {
	run := response.GetRun()
	if run == nil {
		run = &corev1.MachineValidationRun{ValidationId: response.GetValidationId()}
	}
	return NewAPIMachineValidationRun(run)
}

type APIMachineValidationExternalConfig struct {
	Name        string    `json:"name"`
	Description string    `json:"description"`
	Config      []byte    `json:"config"`
	Version     string    `json:"version"`
	Timestamp   time.Time `json:"timestamp"`
}

func NewAPIMachineValidationExternalConfig(proto *corev1.MachineValidationExternalConfig) *APIMachineValidationExternalConfig {
	apio := &APIMachineValidationExternalConfig{
		Name:        proto.GetName(),
		Description: proto.GetDescription(),
		Config:      proto.GetConfig(),
		Version:     proto.GetVersion(),
	}
	if protoTime := proto.GetTimestamp(); protoTime != nil {
		apio.Timestamp = protoTime.AsTime()
	}
	return apio
}

type APIMachineValidationExternalConfigCreateRequest struct {
	Name        string  `json:"name"`
	Description *string `json:"description"`
	Config      []byte  `json:"config"`
}

// Validate ensures that the values passed in request are acceptable
func (req APIMachineValidationExternalConfigCreateRequest) Validate() error {
	err := validation.ValidateStruct(&req,
		validation.Field(&req.Name,
			validation.Required.Error(validationErrorStringLength64),
			validation.By(util.ValidateNameCharacters),
			validation.Length(2, 64).Error(validationErrorStringLength64)),
		validation.Field(&req.Config,
			validation.Required.Error(validationErrorValueRequired)),
	)
	if err != nil {
		return err
	}
	return nil
}

func (req APIMachineValidationExternalConfigCreateRequest) ToProto() *corev1.AddUpdateMachineValidationExternalConfigRequest {
	return &corev1.AddUpdateMachineValidationExternalConfigRequest{
		Name:        req.Name,
		Description: req.Description,
		Config:      req.Config,
	}
}

type APIMachineValidationExternalConfigUpdateRequest struct {
	Description *string `json:"description"`
	Config      []byte  `json:"config"`
}
