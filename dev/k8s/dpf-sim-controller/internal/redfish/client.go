// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package redfish is the small slice of a DPU BMC Redfish client the simulator
// needs to install a BFB the way the real DPF operator does: one
// UpdateService.SimpleUpdate POST, then polling the task it returns.
//
// The real operator's client is
// internal/provisioning/controllers/dpu/state/redfish/client/client.go in
// doca-platform (InstallBFB, CheckTaskProgress). That package is internal/ and
// not importable, so the two calls are mirrored here with the same paths and
// request body.
package redfish

import (
	"bytes"
	"context"
	"crypto/tls"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"strings"
	"time"
)

const (
	// simpleUpdatePath is the action the real operator POSTs the BFB to.
	simpleUpdatePath = "/redfish/v1/UpdateService/Actions/UpdateService.SimpleUpdate"
	// tasksPath is the TaskService collection used when the response identifies
	// its task by Id rather than @odata.id.
	tasksPath = "/redfish/v1/TaskService/Tasks"
	// dpuOSTarget is the firmware inventory entry a BFB install targets. It has
	// no leading slash on purpose: the real operator and NICo both send it so.
	dpuOSTarget = "redfish/v1/UpdateService/FirmwareInventory/DPU_OS"

	// maxBody bounds how much of a BMC response is read.
	maxBody = 1 << 20
)

// Task states a BMC reports (Redfish TaskState). Only the ones the simulator
// acts on are named; anything else, such as Starting, means wait.
const (
	TaskStateRunning   = "Running"
	TaskStateCompleted = "Completed"
	TaskStateException = "Exception"
)

// ErrTaskNotFound reports a Redfish error 404 for the task being polled, for
// example after the BMC restarted and forgot it. A transport route miss retries.
var ErrTaskNotFound = errors.New("redfish task not found")

// ErrUpdateRejected reports that the BMC answered SimpleUpdate with something
// other than 202 Accepted. The real operator moves the DPU to Error on this.
var ErrUpdateRejected = errors.New("redfish SimpleUpdate rejected")

// Task is the part of a Redfish Task resource the simulator reads.
type Task struct {
	State           string
	PercentComplete int
	// Messages are the human-readable task messages, for logs and conditions.
	Messages []string
}

// Client talks to one DPU BMC over HTTPS with basic auth.
type Client struct {
	baseURL  string
	user     string
	password string
	http     *http.Client
}

// New returns a client for the BMC at baseURL (for example
// "https://10.0.0.5:443").
//
// TLS verification is skipped, as the real operator does for its basic-auth
// client: DPU BMCs, and every machine-a-tron mock BMC, serve self-signed
// certificates. The simulator is dev tooling and sends only the shared dev BMC
// credential.
func New(baseURL, user, password string, timeout time.Duration) *Client {
	return &Client{
		baseURL:  strings.TrimRight(baseURL, "/"),
		user:     user,
		password: password,
		http: &http.Client{
			Timeout: timeout,
			Transport: &http.Transport{
				TLSClientConfig: &tls.Config{InsecureSkipVerify: true}, //nolint:gosec // see New
				// One short-lived request per call: do not park an idle
				// connection per DPU across thousands of DPUs.
				DisableKeepAlives: true,
			},
		},
	}
}

// SimpleUpdate asks the BMC to install imageURI to the DPU OS and returns the
// path of its Task resource, or its task monitor when the body names no
// resource. A 202 response is required.
func (c *Client) SimpleUpdate(ctx context.Context, imageURI string) (string, error) {
	body, err := json.Marshal(map[string]any{
		"TransferProtocol": "HTTP",
		"ImageURI":         imageURI,
		"Targets":          []string{dpuOSTarget},
	})
	if err != nil {
		return "", err
	}
	resp, data, err := c.do(ctx, http.MethodPost, simpleUpdatePath, bytes.NewReader(body))
	if err != nil {
		return "", err
	}
	if resp.StatusCode != http.StatusAccepted {
		return "", fmt.Errorf("%w: %s: %s", ErrUpdateRejected, resp.Status, snippet(data))
	}
	return taskPath(resp, data)
}

// GetTask reads the task resource or monitor at path, as returned by SimpleUpdate.
// Monitor 202 means running and 204 means completed. A 200 response must
// contain TaskState, so a malformed resource cannot be mistaken for success.
func (c *Client) GetTask(ctx context.Context, path string) (Task, error) {
	resp, data, err := c.do(ctx, http.MethodGet, path, nil)
	if err != nil {
		return Task{}, err
	}
	switch resp.StatusCode {
	case http.StatusAccepted:
		return Task{State: TaskStateRunning}, nil
	case http.StatusNoContent:
		return Task{State: TaskStateCompleted, PercentComplete: 100}, nil
	case http.StatusOK:
		// A task resource supplies its explicit lifecycle state below.
	default:
		if resp.StatusCode == http.StatusNotFound {
			var envelope struct {
				Error map[string]any `json:"error"`
			}
			if json.Unmarshal(data, &envelope) == nil && envelope.Error != nil {
				return Task{}, fmt.Errorf("%w: %s", ErrTaskNotFound, path)
			}
		}
		return Task{}, fmt.Errorf("get task %s: %s: %s", path, resp.Status, snippet(data))
	}
	var raw struct {
		TaskState       string
		PercentComplete int
		Messages        []struct{ Message string }
	}
	if err := json.Unmarshal(data, &raw); err != nil {
		return Task{}, fmt.Errorf("decode task %s: %w", path, err)
	}
	if raw.TaskState == "" {
		return Task{}, fmt.Errorf("task %s response has no TaskState", path)
	}
	t := Task{State: raw.TaskState, PercentComplete: raw.PercentComplete}
	for _, m := range raw.Messages {
		if m.Message != "" {
			t.Messages = append(t.Messages, m.Message)
		}
	}
	return t, nil
}

func (c *Client) do(ctx context.Context, method, path string, body io.Reader) (*http.Response, []byte, error) {
	req, err := http.NewRequestWithContext(ctx, method, c.baseURL+path, body)
	if err != nil {
		return nil, nil, err
	}
	req.SetBasicAuth(c.user, c.password)
	req.Header.Set("Accept", "application/json")
	if body != nil {
		req.Header.Set("Content-Type", "application/json")
	}
	resp, err := c.http.Do(req)
	if err != nil {
		return nil, nil, err
	}
	defer resp.Body.Close()
	data, err := io.ReadAll(io.LimitReader(resp.Body, maxBody))
	if err != nil {
		return nil, nil, err
	}
	return resp, data, nil
}

// taskPath prefers the durable Task resource in the response body. Location
// is a task monitor, whose 202/204 responses are handled by GetTask when the
// body supplies neither @odata.id nor Id.
func taskPath(resp *http.Response, data []byte) (string, error) {
	var body struct {
		ODataID string `json:"@odata.id"`
		ID      string `json:"Id"`
	}
	if err := json.Unmarshal(data, &body); err == nil {
		if body.ODataID != "" {
			return taskURI(body.ODataID)
		}
		if body.ID != "" {
			return tasksPath + "/" + url.PathEscape(body.ID), nil
		}
	}
	if loc := resp.Header.Get("Location"); loc != "" {
		return taskURI(loc)
	}
	return "", fmt.Errorf("SimpleUpdate accepted but response names no task: %s", snippet(data))
}

// taskURI retains escaped path and query components but always uses the
// configured BMC origin, including when a BMC returns an absolute reference.
func taskURI(reference string) (string, error) {
	u, err := url.Parse(reference)
	if err != nil {
		return "", fmt.Errorf("bad task reference %q: %w", reference, err)
	}
	if u.Opaque != "" || !strings.HasPrefix(u.EscapedPath(), "/") || u.Fragment != "" {
		return "", fmt.Errorf("task reference %q has no absolute path or includes a fragment", reference)
	}
	return u.RequestURI(), nil
}

// snippet trims a response body for inclusion in an error message.
func snippet(data []byte) string {
	s := strings.TrimSpace(string(data))
	if len(s) > 200 {
		s = s[:200] + "..."
	}
	return s
}
