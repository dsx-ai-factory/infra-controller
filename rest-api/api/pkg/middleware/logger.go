// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package middleware

import (
	"encoding/json"
	"strconv"
	"time"

	"github.com/labstack/echo/v4"
	"github.com/rs/zerolog"
	"github.com/rs/zerolog/log"

	cotel "github.com/NVIDIA/infra-controller/rest-api/common/pkg/otel"
)

// Logger returns a middleware that logs HTTP requests
func Logger() echo.MiddlewareFunc {
	return func(next echo.HandlerFunc) echo.HandlerFunc {
		return func(c echo.Context) error {
			req := c.Request()
			res := c.Response()
			start := time.Now()

			err := next(c)
			errMsg := ""
			if err != nil {
				c.Error(err)

				// Error may contain invalid JSON e.g. `"`
				errBytes, _ := json.Marshal(err.Error())
				errBytes = errBytes[1 : len(errBytes)-1]
				errMsg = string(errBytes)
			}

			stop := time.Now()

			latency := stop.Sub(start)

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

			// Correlate the access log with the request trace (no-op fields
			// when tracing is disabled)
			reqLogger := cotel.LoggerWithTrace(req.Context(), log.Logger)

			reqLogger.WithLevel(level).
				Str("Method", c.Request().Method).
				Str("ID", id).
				Str("Path", c.Request().URL.Path).
				Str("Remote IP", c.RealIP()).
				Str("Host", req.Host).
				Str("URI", req.RequestURI).
				Str("User Agent", req.UserAgent()).
				Int("Status", res.Status).
				Str("Timestamp", time.Now().Format(time.RFC3339Nano)).
				Str("Latency", strconv.FormatInt(int64(latency), 10)).
				Str("Latency Human", stop.Sub(start).String()).
				Str("Bytes In", req.Header.Get(echo.HeaderContentLength)).
				Str("Bytes Out", strconv.FormatInt(res.Size, 10)).
				Str("Error", errMsg).
				Msg("HTTP request")

			return err
		}
	}
}
