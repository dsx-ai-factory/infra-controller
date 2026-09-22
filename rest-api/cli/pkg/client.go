// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package cli

import (
	"bytes"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"os"
	"os/exec"
	"regexp"
	"strings"
	"time"

	"github.com/sirupsen/logrus"
)

type Client struct {
	BaseURL         string
	Org             string
	Token           string
	APIName         string
	HTTPClient      *http.Client
	Debug           bool
	Log             *logrus.Entry
	TokenRefresh    func() (string, error)
	AuthRetryMax    int
	AuthRetryNotify func(AuthRetryEvent)
}

// http2StreamError matches net/http's private stream error through errors.As.
type http2StreamError struct {
	StreamID uint32
	Code     uint32
	Cause    error
}

const http2InternalErrorCode uint32 = 0x2

func (hse http2StreamError) Error() string {
	return fmt.Sprintf("HTTP/2 stream %d failed with code %d", hse.StreamID, hse.Code)
}

type AuthRetryAction string

const (
	AuthRetryActionLogin AuthRetryAction = "login"
	AuthRetryActionRetry AuthRetryAction = "retry"
	AuthRetryActionSkip  AuthRetryAction = "skip"
	defaultAuthRetryMax                  = 3
)

type AuthRetryEvent struct {
	Action      AuthRetryAction
	Attempt     int
	MaxAttempts int
	StatusCode  int
	Status      string
	Method      string
}

type APIError struct {
	StatusCode int
	Status     string
	Body       string
	Message    string
	Data       interface{}
}

func (e *APIError) Error() string {
	msg := e.Message
	if msg == "" {
		msg = e.Body
	}
	if e.Data != nil {
		dataJSON, err := json.Marshal(e.Data)
		if err == nil && string(dataJSON) != "null" {
			return fmt.Sprintf("API error %d: %s\nDetails: %s", e.StatusCode, msg, string(dataJSON))
		}
	}
	return fmt.Sprintf("API error %d: %s", e.StatusCode, msg)
}

func NewClient(baseURL, org, token string, log *logrus.Entry, debug bool) *Client {
	if log == nil {
		log = logrus.NewEntry(logrus.StandardLogger())
	}
	if debug {
		log.Logger.SetLevel(logrus.DebugLevel)
	}

	return &Client{
		BaseURL: strings.TrimRight(baseURL, "/"),
		Org:     org,
		Token:   token,
		APIName: "nico",
		HTTPClient: &http.Client{
			Timeout: 30 * time.Second,
		},
		Debug: debug,
		Log:   log,
	}
}

var orgScopedAPIPathPattern = regexp.MustCompile(`(/v[0-9]+/org/[^/]+/)([^/]+)`)

// rewriteAPIName replaces the API path segment after /org/{org}/ with the
// configured API name. This decouples the CLI from the hardcoded path in the spec.
func (c *Client) rewriteAPIName(path string) string {
	if c.APIName == "" || c.APIName == "nico" {
		return path
	}
	return orgScopedAPIPathPattern.ReplaceAllString(path, "${1}"+c.APIName)
}

// Do executes an HTTP request against the API.
func (c *Client) Do(method, pathTemplate string, pathParams, queryParams map[string]string, body []byte) ([]byte, http.Header, error) {
	doClient := c
	respBody, respHeader, err := doClient.do(method, pathTemplate, pathParams, queryParams, body)
	streamErr, isHTTP2StreamError := errors.AsType[http2StreamError](err)
	if method == http.MethodGet && isHTTP2StreamError && streamErr.Code == http2InternalErrorCode {
		var transport *http.Transport
		switch currentTransport := c.HTTPClient.Transport.(type) {
		case nil:
			transport = http.DefaultTransport.(*http.Transport).Clone()
		case *http.Transport:
			transport = currentTransport.Clone()
		}
		if transport != nil {
			protocols := new(http.Protocols)
			protocols.SetHTTP1(true)
			transport.Protocols = protocols
			transport.TLSNextProto = nil
			if transport.TLSClientConfig != nil {
				transport.TLSClientConfig.NextProtos = nil
			}

			httpClient := *c.HTTPClient
			httpClient.Transport = transport
			retryClient := *c
			retryClient.HTTPClient = &httpClient
			doClient = &retryClient
			respBody, respHeader, err = doClient.do(method, pathTemplate, pathParams, queryParams, body)
		}
	}
	if isUnauthorizedError(err) && c.TokenRefresh != nil && !canReplayAfterAuthRefresh(method) {
		apiErr := err.(*APIError)
		c.notifyAuthRetry(AuthRetryEvent{
			Action:      AuthRetryActionSkip,
			Attempt:     0,
			MaxAttempts: c.authRetryMax(),
			StatusCode:  apiErr.StatusCode,
			Status:      apiErr.Status,
			Method:      method,
		})
		return respBody, respHeader, err
	}

	maxAttempts := c.authRetryMax()
	for attempt := 1; attempt <= maxAttempts && isUnauthorizedError(err) && c.TokenRefresh != nil; attempt++ {
		apiErr := err.(*APIError)
		c.notifyAuthRetry(AuthRetryEvent{
			Action:      AuthRetryActionLogin,
			Attempt:     attempt,
			MaxAttempts: maxAttempts,
			StatusCode:  apiErr.StatusCode,
			Status:      apiErr.Status,
			Method:      method,
		})
		token, refreshErr := c.TokenRefresh()
		if refreshErr != nil {
			return nil, nil, fmt.Errorf("refreshing auth token after unauthorized response: %w", refreshErr)
		}
		if token == "" {
			return nil, nil, fmt.Errorf("refreshing auth token after unauthorized response: no token returned")
		}
		c.Token = token
		doClient.Token = token
		c.notifyAuthRetry(AuthRetryEvent{
			Action:      AuthRetryActionRetry,
			Attempt:     attempt,
			MaxAttempts: maxAttempts,
			StatusCode:  apiErr.StatusCode,
			Status:      apiErr.Status,
			Method:      method,
		})
		respBody, respHeader, err = doClient.do(method, pathTemplate, pathParams, queryParams, body)
	}
	return respBody, respHeader, err
}

func (c *Client) authRetryMax() int {
	if c.AuthRetryMax > 0 {
		return c.AuthRetryMax
	}
	return defaultAuthRetryMax
}

func (c *Client) notifyAuthRetry(event AuthRetryEvent) {
	if c.AuthRetryNotify != nil {
		c.AuthRetryNotify(event)
	}
}

func canReplayAfterAuthRefresh(method string) bool {
	switch strings.ToUpper(method) {
	case http.MethodGet, http.MethodHead, http.MethodOptions:
		return true
	default:
		return false
	}
}

func (c *Client) do(method, pathTemplate string, pathParams, queryParams map[string]string, body []byte) ([]byte, http.Header, error) {
	path := pathTemplate
	path = strings.ReplaceAll(path, "{org}", url.PathEscape(c.Org))
	for k, v := range pathParams {
		path = strings.ReplaceAll(path, "{"+k+"}", url.PathEscape(v))
	}

	path = c.rewriteAPIName(path)
	reqURL := c.BaseURL + path
	if len(queryParams) > 0 {
		q := url.Values{}
		for k, v := range queryParams {
			q.Set(k, v)
		}
		reqURL += "?" + q.Encode()
	}

	var bodyReader io.Reader
	if body != nil {
		bodyReader = bytes.NewReader(body)
	}

	req, err := http.NewRequest(method, reqURL, bodyReader)
	if err != nil {
		return nil, nil, fmt.Errorf("creating request: %w", err)
	}

	if body != nil {
		req.Header.Set("Content-Type", "application/json")
	}
	req.Header.Set("Accept", "application/json")
	if c.Token != "" {
		req.Header.Set("Authorization", "Bearer "+c.Token)
	}

	if c.Debug {
		c.Log.Debugf("API request: %s %s", method, reqURL)
		c.Log.Debugf("Request headers: %s", formatDebugHeaders(req.Header))
		c.Log.Debugf("Request body: %s", formatDebugBody(body))
	}

	resp, err := c.HTTPClient.Do(req)
	if err != nil {
		return nil, nil, fmt.Errorf("executing request: %w", err)
	}
	defer resp.Body.Close()

	respBody, err := io.ReadAll(resp.Body)
	if err != nil {
		return nil, nil, fmt.Errorf("reading response: %w", err)
	}

	if c.Debug {
		c.Log.Debugf("API response: %s %s -> %s", method, reqURL, resp.Status)
		c.Log.Debugf("Response headers: %s", formatDebugHeaders(resp.Header))
		c.Log.Debugf("Response body: %s", formatDebugBody(respBody))
	}

	if resp.StatusCode >= 400 {
		apiErr := &APIError{
			StatusCode: resp.StatusCode,
			Status:     resp.Status,
			Body:       string(respBody),
		}
		var errResp struct {
			Source  string      `json:"source"`
			Message string      `json:"message"`
			Error   string      `json:"error"`
			Data    interface{} `json:"data"`
		}
		if json.Unmarshal(respBody, &errResp) == nil {
			if errResp.Message != "" {
				apiErr.Message = errResp.Message
			} else if errResp.Error != "" {
				apiErr.Message = errResp.Error
			}
			apiErr.Data = errResp.Data
		}
		return nil, nil, apiErr
	}

	return respBody, resp.Header, nil
}

func formatDebugBody(body []byte) string {
	if len(body) == 0 {
		return "<empty>"
	}
	var decoded interface{}
	decoder := json.NewDecoder(bytes.NewReader(body))
	decoder.UseNumber()
	if err := decoder.Decode(&decoded); err != nil {
		return string(body)
	}
	if err := decoder.Decode(&struct{}{}); !errors.Is(err, io.EOF) {
		return string(body)
	}
	redactSensitiveJSONFields(decoded)
	var buf bytes.Buffer
	encoder := json.NewEncoder(&buf)
	encoder.SetEscapeHTML(false)
	if err := encoder.Encode(decoded); err != nil {
		return "<JSON body unavailable>"
	}
	return strings.TrimSpace(buf.String())
}

func redactSensitiveJSONFields(value interface{}) {
	redactSensitiveJSONValue(value, false)
}

func redactSensitiveJSONValue(value interface{}, inherited bool) {
	switch typed := value.(type) {
	case map[string]interface{}:
		for key, nested := range typed {
			sensitive := inherited || isSensitiveJSONField(key)
			if sensitive && !isJSONContainer(nested) {
				typed[key] = "<redacted>"
				continue
			}
			redactSensitiveJSONValue(
				nested,
				inherited || isSensitiveJSONContainerField(key),
			)
		}
	case []interface{}:
		for i, nested := range typed {
			if inherited && !isJSONContainer(nested) {
				typed[i] = "<redacted>"
				continue
			}
			redactSensitiveJSONValue(nested, inherited)
		}
	}
}

func isJSONContainer(value interface{}) bool {
	switch value.(type) {
	case map[string]interface{}, []interface{}:
		return true
	default:
		return false
	}
}

func isSensitiveJSONField(name string) bool {
	normalized := strings.NewReplacer("-", "", "_", "").Replace(strings.ToLower(name))
	for _, fragment := range []string{"credential", "password", "secret", "privatekey", "apikey"} {
		if strings.Contains(normalized, fragment) {
			return true
		}
	}
	return normalized == "token" || strings.HasSuffix(normalized, "token")
}

func isSensitiveJSONContainerField(name string) bool {
	normalized := strings.NewReplacer("-", "", "_", "").Replace(strings.ToLower(name))
	for _, suffix := range []string{
		"credential", "credentials",
		"password", "passwords",
		"secret", "secrets",
		"privatekey", "privatekeys",
		"apikey", "apikeys",
		"token", "tokens",
	} {
		if strings.HasSuffix(normalized, suffix) {
			return true
		}
	}
	return false
}

func formatDebugHeaders(headers http.Header) string {
	redacted := make(http.Header, len(headers))
	for key, values := range headers {
		copiedValues := append([]string(nil), values...)
		if isSensitiveHeader(key) {
			for i, value := range copiedValues {
				copiedValues[i] = redactHeaderValue(key, value)
			}
		}
		redacted[http.CanonicalHeaderKey(key)] = copiedValues
	}

	var buf bytes.Buffer
	encoder := json.NewEncoder(&buf)
	encoder.SetEscapeHTML(false)
	if err := encoder.Encode(redacted); err != nil {
		return fmt.Sprintf("%v", redacted)
	}
	return strings.TrimSpace(buf.String())
}

func isSensitiveHeader(key string) bool {
	switch strings.ToLower(key) {
	case "authorization", "proxy-authorization", "cookie", "set-cookie", "x-api-key", "x-auth-token":
		return true
	default:
		return false
	}
}

func redactHeaderValue(key, value string) string {
	if strings.EqualFold(key, "authorization") || strings.EqualFold(key, "proxy-authorization") {
		if scheme, _, ok := strings.Cut(value, " "); ok && scheme != "" {
			return scheme + " <redacted>"
		}
	}
	return "<redacted>"
}

func isUnauthorizedError(err error) bool {
	apiErr, ok := err.(*APIError)
	if !ok {
		return false
	}
	return apiErr.StatusCode == http.StatusUnauthorized
}

// ResolveToken returns the token or executes the token command.
func ResolveToken(token, tokenCommand string) (string, error) {
	if token != "" {
		return token, nil
	}
	if tokenCommand != "" {
		out, err := exec.Command("sh", "-c", tokenCommand).Output()
		if err != nil {
			return "", fmt.Errorf("executing token command: %w", err)
		}
		return strings.TrimSpace(string(out)), nil
	}
	return "", nil
}

// ReadBodyInput reads request body from --data flag or --data-file flag.
// Use "--data-file -" to read from stdin.
func ReadBodyInput(data, dataFile string) ([]byte, error) {
	if data != "" && dataFile != "" {
		return nil, fmt.Errorf("specify either --data or --data-file, not both")
	}
	if data != "" {
		return []byte(data), nil
	}
	if dataFile != "" {
		if dataFile == "-" {
			b, err := io.ReadAll(os.Stdin)
			if err != nil {
				return nil, fmt.Errorf("reading stdin: %w", err)
			}
			return b, nil
		}
		b, err := os.ReadFile(dataFile)
		if err != nil {
			return nil, fmt.Errorf("reading data file: %w", err)
		}
		return b, nil
	}
	return nil, nil
}
