// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package cmd

import (
	"bytes"
	"errors"
	"html/template"
	"net/http"
	"net/http/httptest"
	"os"
	"regexp"
	"strings"
	"testing"

	pb "github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/internal/proto/v1"
	nsmlog "github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/pkg/common/log"
	"github.com/google/uuid"
	log "github.com/sirupsen/logrus"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestSwitchesTableEndpointCells(t *testing.T) {
	tmpl, err := template.New("").Funcs(template.FuncMap{
		"formatEndpoint": formatEndpoint,
	}).ParseFS(uiTemplates, "ui_templates/switches.html")
	require.NoError(t, err)
	endpointCells := regexp.MustCompile(`(?s)<td class="mono">\s*(.*?)\s*</td>`)

	tests := []struct {
		name     string
		bmc      *pb.BMCInfo
		nvos     *pb.NVOSInfo
		wantBMC  string
		wantNVOS string
	}{
		{
			name:     "IPv6 with ports",
			bmc:      &pb.BMCInfo{IpAddress: "2001:db8::1", Port: 443},
			nvos:     &pb.NVOSInfo{IpAddress: "2001:db8::2", Port: 22},
			wantBMC:  "[2001:db8::1]:443",
			wantNVOS: "[2001:db8::2]:22",
		},
		{
			name:     "IPv4 with ports",
			bmc:      &pb.BMCInfo{IpAddress: "192.0.2.1", Port: 8443},
			nvos:     &pb.NVOSInfo{IpAddress: "192.0.2.2", Port: 2222},
			wantBMC:  "192.0.2.1:8443",
			wantNVOS: "192.0.2.2:2222",
		},
		{
			name:     "addresses without ports",
			bmc:      &pb.BMCInfo{IpAddress: "2001:db8::1"},
			nvos:     &pb.NVOSInfo{IpAddress: "192.0.2.2"},
			wantBMC:  "2001:db8::1",
			wantNVOS: "192.0.2.2",
		},
		{
			name:     "absent endpoints",
			wantBMC:  "-",
			wantNVOS: "-",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			data := map[string]any{
				"Switches": []SwitchWithStatus{{
					Switch:       &pb.NVSwitchTray{Uuid: "switch-1", Bmc: tt.bmc, Nvos: tt.nvos},
					UpdateStatus: &UpdateStatusSummary{},
				}},
			}
			var rendered bytes.Buffer
			err := tmpl.ExecuteTemplate(&rendered, "switches.html", data)
			require.NoError(t, err)
			assert.Contains(t, rendered.String(), ">BMC IP</th>")
			assert.Contains(t, rendered.String(), ">NVOS IP</th>")
			cells := endpointCells.FindAllStringSubmatch(rendered.String(), -1)
			require.Len(t, cells, 2)
			assert.Equal(t, tt.wantBMC, strings.TrimSpace(cells[0][1]))
			assert.Equal(t, tt.wantNVOS, strings.TrimSpace(cells[1][1]))
		})
	}
}

type uiResponseWriter struct {
	http.ResponseWriter
	writeError   error
	writes       int
	statusWrites int
}

func (w *uiResponseWriter) Write(data []byte) (int, error) {
	w.writes++
	if w.writeError != nil {
		return 0, w.writeError
	}
	return w.ResponseWriter.Write(data)
}

func (w *uiResponseWriter) WriteHeader(status int) {
	w.statusWrites++
	w.ResponseWriter.WriteHeader(status)
}

func captureUILogs(t *testing.T) *bytes.Buffer {
	t.Helper()
	var logs bytes.Buffer
	logger := log.StandardLogger()
	originalOutput := logger.Out
	originalFormatter := logger.Formatter
	logger.SetOutput(&logs)
	logger.SetFormatter(&nsmlog.JSONFormatter{})
	t.Cleanup(func() {
		logger.SetOutput(originalOutput)
		logger.SetFormatter(originalFormatter)
	})
	return &logs
}

func TestUIServer_handleAPIUpdateLog(t *testing.T) {
	writeError := errors.New("test response write failed")
	tests := []struct {
		name       string
		writeError error
		wantBody   string
		wantLog    string
		wantError  string
	}{
		{
			name:     "JSON response",
			wantBody: "{\"error\":\"Log file not found. This update may not use the script strategy or hasn't started yet.\",\"found\":false}\n",
		},
		{
			name:       "JSON write failure",
			writeError: writeError,
			wantLog:    "Failed to write JSON response",
			wantError:  writeError.Error(),
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			logs := captureUILogs(t)
			recorder := httptest.NewRecorder()
			writer := &uiResponseWriter{ResponseWriter: recorder, writeError: tt.writeError}
			server := &UIServer{}
			id := uuid.NewString()
			_, err := os.Stat("/tmp/nsm-script-" + id + ".log")
			require.ErrorIs(t, err, os.ErrNotExist)
			request := httptest.NewRequest(http.MethodGet, "/api/update-log?id="+id, nil)
			server.handleAPIUpdateLog(writer, request)
			assert.Equal(t, "application/json", recorder.Header().Get("Content-Type"))
			assert.Equal(t, http.StatusOK, recorder.Code)
			assert.Zero(t, writer.statusWrites)
			assert.Equal(t, tt.wantBody, recorder.Body.String())
			if tt.wantLog == "" {
				assert.Empty(t, logs.String())
			} else {
				assert.Contains(t, logs.String(), tt.wantLog)
				assert.Contains(t, logs.String(), tt.wantError)
				if tt.writeError != nil {
					assert.Equal(t, 1, writer.writes)
				}
			}
		})
	}
}

func TestUIServer_renderError(t *testing.T) {
	writeError := errors.New("test response write failed")
	tests := []struct {
		name       string
		writeError error
		wantBody   string
		wantLog    string
		wantError  string
		template   string
	}{
		{
			name:     "template response",
			template: `{{define "layout.html"}}{{.ErrorTitle}}: {{.ErrorMessage}}{{end}}`,
			wantBody: "title: message",
		},
		{
			name:       "template write failure",
			template:   `{{define "layout.html"}}{{.ErrorTitle}}: {{.ErrorMessage}}{{end}}`,
			writeError: writeError,
			wantLog:    "Failed to render UI page",
			wantError:  writeError.Error(),
		},
		{
			name:      "template evaluation failure",
			template:  `{{define "layout.html"}}{{call .Missing}}{{end}}`,
			wantLog:   "Failed to render UI page",
			wantError: "call of nil",
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			logs := captureUILogs(t)
			recorder := httptest.NewRecorder()
			writer := &uiResponseWriter{ResponseWriter: recorder, writeError: tt.writeError}
			server := &UIServer{}
			var err error
			server.templates, err = template.New("").Parse(tt.template)
			require.NoError(t, err)
			server.renderError(writer, "title", "message")
			assert.Equal(t, http.StatusOK, recorder.Code)
			assert.Zero(t, writer.statusWrites)
			assert.Equal(t, tt.wantBody, recorder.Body.String())
			if tt.wantLog == "" {
				assert.Empty(t, logs.String())
			} else {
				assert.Contains(t, logs.String(), tt.wantLog)
				assert.Contains(t, logs.String(), tt.wantError)
				if tt.writeError != nil {
					assert.Equal(t, 1, writer.writes)
				}
			}
		})
	}
}
