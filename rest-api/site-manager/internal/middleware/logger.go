// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package middleware holds Site Manager's Echo middleware: request logging
// and request metrics.
package middleware

import (
	"strconv"
	"time"

	"github.com/labstack/echo/v4"
	"github.com/rs/zerolog"
	"github.com/rs/zerolog/log"

	cotel "github.com/NVIDIA/infra-controller/rest-api/common/pkg/otel"
)

// Logger logs each request once it's answered, with the same fields as the
// REST API's request log. A `4xx` logs at warn level and a `5xx` at error
// level.
func Logger() echo.MiddlewareFunc {
	return func(next echo.HandlerFunc) echo.HandlerFunc {
		return func(c echo.Context) error {
			req := c.Request()
			res := c.Response()
			start := time.Now()

			err := next(c)
			errMsg := ""
			if err != nil {
				// Writes the error response, so the logged status is final.
				c.Error(err)
				errMsg = err.Error()
			}

			latency := time.Since(start)

			id := req.Header.Get(echo.HeaderXRequestID)
			if id == "" {
				id = res.Header().Get(echo.HeaderXRequestID)
			}

			level := zerolog.InfoLevel
			if res.Status >= 500 {
				level = zerolog.ErrorLevel
			} else if res.Status >= 400 {
				level = zerolog.WarnLevel
			}

			reqLogger := cotel.LoggerWithTrace(req.Context(), log.Logger)

			reqLogger.WithLevel(level).
				Str("Method", req.Method).
				Str("ID", id).
				Str("Path", req.URL.Path).
				Str("Remote IP", c.RealIP()).
				Str("Host", req.Host).
				Str("URI", req.RequestURI).
				Str("User Agent", req.UserAgent()).
				Int("Status", res.Status).
				Str("Timestamp", time.Now().Format(time.RFC3339Nano)).
				Str("Latency", strconv.FormatInt(int64(latency), 10)).
				Str("Latency Human", latency.String()).
				Str("Bytes In", req.Header.Get(echo.HeaderContentLength)).
				Str("Bytes Out", strconv.FormatInt(res.Size, 10)).
				Str("Error", errMsg).
				Msg("HTTP request")

			return err
		}
	}
}
