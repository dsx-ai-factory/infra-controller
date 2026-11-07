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
// The request/response payloads are carried as protojson (json.RawMessage) so
// they render as readable JSON in the Temporal UI. Secret fields (e.g. a BMC
// credential password) are redacted from that readable JSON by RedactSecrets
// and carried separately as an AES-GCM ciphertext (EncryptedSecrets) so they
// never appear in Temporal history in cleartext; the site decrypts them and
// calls MergeSecrets before the call.
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
	// history; the site decrypts it with the shared site key and merges the
	// values back into RequestJSON before invoking the backend.
	EncryptedSecrets []byte `json:"encryptedSecrets,omitempty"`
}

// Response is the generic proxy workflow/activity output.
type Response struct {
	// ResponseJSON is the protojson-encoded response message.
	ResponseJSON json.RawMessage `json:"responseJson,omitempty"`
}
