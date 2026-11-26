// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package server

import (
	"context"
	"errors"
	"net/http"
	"strconv"
	"time"

	"github.com/modelcontextprotocol/go-sdk/jsonrpc"
	"github.com/modelcontextprotocol/go-sdk/mcp"
	"go.opentelemetry.io/otel"
	"go.opentelemetry.io/otel/attribute"
	"go.opentelemetry.io/otel/codes"
	"go.opentelemetry.io/otel/propagation"
	"go.opentelemetry.io/otel/trace"
)

const (
	// tracingServiceName is service.name when the environment does not set one.
	tracingServiceName = "nico-mcp"
	// tracerName identifies the spans nico-mcp creates.
	tracerName = "github.com/NVIDIA/infra-controller/rest-api/mcp"
	// tracingShutdownTimeout bounds the final span flush, which runs after
	// the HTTP servers have drained.
	tracingShutdownTimeout = 5 * time.Second
)

// methodToolsCall is the MCP method of a tool call.
const methodToolsCall = "tools/call"

// error.type values for a failed tool call that has no JSON-RPC error code.
const (
	errorTypeToolError = "tool_error"
	errorTypeOther     = "_OTHER"
)

// traceToolCalls records one server span for every tools/call request for a
// registered tool, named and attributed after the OpenTelemetry MCP semantic
// conventions. The span continues the trace context in the inbound HTTP
// headers and is the parent of the NICo REST request the tool makes to the
// configured base URL. A call naming any other tool gets no span, so caller
// input never becomes a span name or attribute. Tool arguments, tokens, and
// error text are never recorded.
func traceToolCalls(tools map[string]bool) mcp.Middleware {
	return func(next mcp.MethodHandler) mcp.MethodHandler {
		return func(ctx context.Context, method string, req mcp.Request) (mcp.Result, error) {
			call, ok := req.(*mcp.CallToolRequest)
			if !ok || !tools[call.Params.Name] {
				return next(ctx, method, req)
			}
			if call.Extra != nil {
				ctx = otel.GetTextMapPropagator().Extract(ctx, propagation.HeaderCarrier(call.Extra.Header))
			}
			ctx, span := otel.Tracer(tracerName).Start(ctx, methodToolsCall+" "+call.Params.Name,
				trace.WithSpanKind(trace.SpanKindServer),
				trace.WithAttributes(
					attribute.String("mcp.method.name", methodToolsCall),
					attribute.String("gen_ai.operation.name", "execute_tool"),
					attribute.String("gen_ai.tool.name", call.Params.Name),
				),
			)
			defer span.End()

			result, err := next(ctx, method, req)
			errorType := toolCallErrorType(result, err)
			if errorType != "" {
				// The status description stays empty: the SDK's validation
				// message and a REST error can both repeat caller input.
				span.SetStatus(codes.Error, "")
				span.SetAttributes(attribute.String("error.type", errorType))
			}
			return result, err
		}
	}
}

// toolCallErrorType returns the error.type of a finished tool call, or "" when
// it succeeded. A call fails here exactly when the tool call metric counts it
// as an error. The value is the JSON-RPC error code when the SDK rejects the
// call, such as -32602 for arguments that do not match the input schema, and
// tool_error when the tool returns an error result.
func toolCallErrorType(result mcp.Result, err error) string {
	if err != nil {
		var rpcErr *jsonrpc.Error
		if errors.As(err, &rpcErr) {
			return strconv.FormatInt(rpcErr.Code, 10)
		}
		return errorTypeOther
	}
	toolResult, _ := result.(*mcp.CallToolResult)
	switch {
	case toolResult == nil:
		return errorTypeOther
	case toolResult.IsError:
		return errorTypeToolError
	default:
		return ""
	}
}

// traceHeaders returns the headers that carry ctx's trace context to NICo
// REST. It is empty when ctx has no trace context.
//
// The REST request gets no client span of its own. An HTTP client span records
// the request URL, which holds the tool arguments, and instrumenting the REST
// client's transport would turn off its HTTP/1.1 retry.
func traceHeaders(ctx context.Context) http.Header {
	headers := http.Header{}
	otel.GetTextMapPropagator().Inject(ctx, propagation.HeaderCarrier(headers))
	return headers
}
