/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 * http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

//! `nv show platform`: the hardware identity, and under `environment` the
//! fans, temperature sensors, leak sensors and LEDs.
//!
//! NVOS renders every reading as a string, speeds as integers and
//! temperatures with two decimals; clients parse them, so the mock keeps the
//! spelling.

use std::sync::Arc;

use axum::extract::State;
use axum::response::Response;
use serde_json::{Map, Value, json};

use super::NvueState;
use super::interface::FABRIC_MANAGEMENT_PORTS;
use crate::json::JsonExt;

/// `GET /nvue_v1/platform`: the switch's hardware identity.
pub(super) async fn show(State(state): State<Arc<NvueState>>) -> Response {
    let config = &state.config;
    json!({
        "asic-model": "NVLink-5 switch",
        "manufacturer": "Nvidia",
        "part-number": config.part_number,
        "port-layout": format!("{} x 400G", config.nvl_ports + FABRIC_MANAGEMENT_PORTS),
        "serial-number": config.serial_number,
        "system-type": config.system_type,
    })
    .into_ok_response()
}

/// The status LEDs and their colors. The system `STATUS` LED turns amber on a
/// leak or an unhealthy system, as the switch's platform daemon drives it;
/// the fan LED stays green, since fan faults are not simulated, and the
/// locator `UID` LED is off.
fn leds(state: &NvueState) -> [(&'static str, &'static str); 3] {
    let status = if state.leaking() {
        "amber"
    } else {
        state.health().status.led()
    };
    [("FAN_STATUS", "green"), ("STATUS", status), ("UID", "off")]
}

/// `GET /nvue_v1/platform/environment`: every fan, temperature sensor and
/// LED with its state and type. Leak sensors are not in this summary.
pub(super) async fn environment(State(state): State<Arc<NvueState>>) -> Response {
    let config = &state.config;
    let fans = config.fans.iter().map(|fan| (fan.name.clone(), "fan"));
    let temperatures = config
        .temperatures
        .iter()
        .map(|sensor| (sensor.name.clone(), "temperature"));
    let mut sensors: Map<String, Value> = fans
        .chain(temperatures)
        .map(|(name, kind)| (name, json!({ "state": "ok", "type": kind })))
        .collect();
    for (led, color) in leds(&state) {
        sensors.insert(led.to_string(), json!({ "state": color, "type": "led" }));
    }
    Value::Object(sensors).into_ok_response()
}

/// `GET /nvue_v1/platform/environment/led`.
pub(super) async fn leds_tree(State(state): State<Arc<NvueState>>) -> Response {
    let leds: Map<String, Value> = leds(&state)
        .into_iter()
        .map(|(led, color)| (led.to_string(), json!({ "color": color })))
        .collect();
    Value::Object(leds).into_ok_response()
}

/// `GET /nvue_v1/platform/environment/fan`.
pub(super) async fn fans(State(state): State<Arc<NvueState>>) -> Response {
    let fans: Map<String, Value> = state
        .config
        .fans
        .iter()
        .map(|fan| {
            (
                fan.name.clone(),
                json!({
                    "current-speed": fan.current_speed_rpm.to_string(),
                    "direction": fan.direction,
                    "max-speed": fan.max_speed_rpm.to_string(),
                    "min-speed": fan.min_speed_rpm.to_string(),
                    "state": "ok",
                }),
            )
        })
        .collect();
    Value::Object(fans).into_ok_response()
}

/// `GET /nvue_v1/platform/environment/temperature`. A sensor carries only
/// the thresholds it has.
pub(super) async fn temperatures(State(state): State<Arc<NvueState>>) -> Response {
    let temperatures: Map<String, Value> = state
        .config
        .temperatures
        .iter()
        .map(|sensor| {
            let mut reading = Map::new();
            let thresholds = [
                ("crit", sensor.crit),
                ("current", Some(sensor.current)),
                ("max", sensor.max),
                ("min", sensor.min),
            ];
            for (key, value) in thresholds {
                if let Some(value) = value {
                    reading.insert(key.to_string(), json!(format!("{value:.2}")));
                }
            }
            reading.insert("state".to_string(), json!("ok"));
            (sensor.name.clone(), Value::Object(reading))
        })
        .collect();
    Value::Object(temperatures).into_ok_response()
}

/// `GET /nvue_v1/platform/environment/leakage`.
pub(super) async fn leakage(State(state): State<Arc<NvueState>>) -> Response {
    let sensors: Map<String, Value> = state
        .leaks()
        .iter()
        .map(|(sensor, leak)| (sensor.clone(), json!({ "state": leak.as_str() })))
        .collect();
    Value::Object(sensors).into_ok_response()
}
