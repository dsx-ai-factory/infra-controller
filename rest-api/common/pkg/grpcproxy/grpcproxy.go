// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package grpcproxy holds the contract shared between the cloud REST API and
// the on-site agent for the generic gRPC proxies.
//
// Instead of a bespoke Temporal workflow + activity + typed request for every
// site-backed REST operation, curated REST handlers can validate their input,
// build a typed request, and dispatch each invocation through one generic
// workflow per backend. A single REST request may dispatch zero, one, or many
// proxied calls; this package defines the transport for one such call. The
// on-site site-agent worker runs a generic activity that performs the actual
// gRPC call. The REST surface stays curated and designed; this is purely the
// internal cloud-to-site transport.
//
// Requests carry readable, redacted protojson plus Vault Transit protected
// secret fields. Protected workflows forward envelopes without decrypting them;
// site activities restore fields before dispatch and encrypt successful replies.
// The predecessor EncryptedSecrets field is read only during explicit migration.
package grpcproxy

import (
	"encoding/json"
	"time"
)

// Backend describes one proxied on-site gRPC service. The two backends share
// every layer of the proxy except these three names, so each layer holds one
// implementation and a per-backend wrapper that supplies this value.
type Backend struct {
	// Label names the backend in operator-visible errors and log fields.
	Label string

	// ServiceName is the fully qualified gRPC service. Its descriptors are
	// resolved from the global proto registry at runtime, which works because
	// the site-agent binary links the generated bindings.
	ServiceName string

	// WorkflowName is the Temporal workflow type registered by the site-agent
	// and started by the cloud REST API. It must match the workflow function
	// name in site-workflow/pkg/workflow, and it must stay stable: Temporal
	// dispatches by this string, so renaming it strands executions that older
	// workers created and stops newer submissions from ever being picked up.
	WorkflowName string
}

// ProtectedWorkflowName identifies the managed-envelope workflow. A distinct
// type keeps predecessor workers from ignoring new fields and sending redacted
// credential placeholders to a backend during a rolling deployment.
func (b Backend) ProtectedWorkflowName() string { return b.WorkflowName + "Protected" }

var (
	// Core proxies to NICo Core.
	Core = Backend{
		Label:        "Core",
		ServiceName:  "forge.Forge",
		WorkflowName: "InvokeCoreGRPC",
	}

	// Flow proxies to Flow.
	Flow = Backend{
		Label:        "Flow",
		ServiceName:  "v1.Flow",
		WorkflowName: "InvokeFlowGRPC",
	}
)

const (
	// ActivityStartToCloseTimeout bounds the on-site request before the
	// workflow and REST caller time out.
	ActivityStartToCloseTimeout = 40 * time.Second

	// WorkflowExecutionTimeout leaves the REST caller time to observe and
	// translate a terminal workflow result.
	//
	// The whole ladder must stay under the API server's write timeout: the
	// caller waits cutil.WorkflowContextTimeout, and a response written after
	// the server's write deadline never reaches the client. Nothing here may
	// exceed that deadline, however generous the on-site budget looks.
	WorkflowExecutionTimeout = 45 * time.Second
)

// Request is the generic proxy workflow/activity input.
type Request struct {
	// FullMethod is the gRPC method, either fully qualified
	// ("/forge.Forge/CreateCredential") or bare ("CreateCredential").
	FullMethod string `json:"fullMethod"`

	// RequestJSON is the protojson-encoded request message with secret fields
	// redacted. Kept as json.RawMessage so it is human-readable in the
	// Temporal UI.
	RequestJSON json.RawMessage `json:"requestJson,omitempty"`

	// EncryptedSecrets is the AES-GCM ciphertext of the redacted secret fields
	// (a JSON object of fieldName -> value). It is opaque (base64) in Temporal
	// history. This predecessor format is accepted only by the explicit
	// legacy-history reader; new senders use ProtectedSecrets.
	EncryptedSecrets []byte `json:"encryptedSecrets,omitempty"`

	// ProtectedSecrets replaces EncryptedSecrets for new credential-bearing
	// requests. Workflow and activity code forward this envelope unchanged.
	ProtectedSecrets *ProtectedPayload `json:"protectedSecrets,omitempty"`
}

// Response is the generic proxy workflow/activity output.
type Response struct {
	// ResponseJSON is the protojson-encoded response message.
	ResponseJSON json.RawMessage `json:"responseJson,omitempty"`

	// ProtectedResponse contains the complete reply to a protected request.
	// It is mutually exclusive with ResponseJSON.
	ProtectedResponse *ProtectedPayload `json:"protectedResponse,omitempty"`
}
