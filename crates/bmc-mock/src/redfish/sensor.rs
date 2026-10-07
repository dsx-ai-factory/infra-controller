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

use std::borrow::Cow;

use rand::{Rng, RngExt};
use serde_json::json;

use crate::json::{JsonExt, JsonPatch};
use crate::redfish;
use crate::redfish::Builder;

pub(super) fn chassis_collection(chassis_id: &str) -> redfish::Collection<'static> {
    let odata_id = format!("/redfish/v1/Chassis/{chassis_id}/Sensors");
    redfish::Collection {
        odata_id: Cow::Owned(odata_id),
        odata_type: Cow::Borrowed("#SensorCollection.SensorCollection"),
        name: Cow::Borrowed("Sensors Collection"),
    }
}

pub(crate) fn chassis_resource<'a>(chassis_id: &str, sensor_id: &'a str) -> redfish::Resource<'a> {
    let odata_id = format!("{}/{sensor_id}", chassis_collection(chassis_id).odata_id);
    redfish::Resource {
        odata_id: Cow::Owned(odata_id),
        odata_type: Cow::Borrowed("#Sensor.v1_6_0.Sensor"),
        id: Cow::Borrowed(sensor_id),
        name: Cow::Borrowed("Sensor"),
    }
}

pub(crate) fn builder(resource: &redfish::Resource) -> SensorBuilder {
    SensorBuilder {
        id: Cow::Owned(resource.id.to_string()),
        value: resource.json_patch().patch(json!({
            "Status": redfish::resource::Status::Ok.into_json(),
        })),
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct Layout {
    pub(crate) temperature: usize,
    pub(crate) fan: usize,
    pub(crate) power: usize,
    pub(crate) current: usize,
    pub(crate) voltage: usize,
}

impl Layout {
    const fn total(&self) -> usize {
        self.temperature + self.fan + self.power + self.current + self.voltage
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Sensor {
    pub(crate) id: Cow<'static, str>,
    value: serde_json::Value,
}

impl Sensor {
    pub(crate) fn to_json(&self) -> serde_json::Value {
        let Some(reading_type) = self
            .value
            .get("ReadingType")
            .and_then(serde_json::Value::as_str)
        else {
            return self.value.clone();
        };
        let Some(kind) = SensorKind::from_reading_type(reading_type) else {
            return self.value.clone();
        };

        let mut rng = rand::rng();

        let reading = kind.random_reading(&mut rng);
        let refreshed_builder = SensorBuilder {
            id: self.id.clone(),
            value: self
                .value
                .clone()
                .patch(json!({ "Reading": reading.into_json() })),
        };

        refreshed_builder.build().value
    }
}

pub(crate) struct SensorBuilder {
    id: Cow<'static, str>,
    value: serde_json::Value,
}

#[derive(Debug, Clone, Default)]
struct Thresholds {
    lower_fatal: Option<f64>,
    lower_critical: Option<f64>,
    lower_caution: Option<f64>,
    upper_caution: Option<f64>,
    upper_critical: Option<f64>,
    upper_fatal: Option<f64>,
}

impl Builder for SensorBuilder {
    fn apply_patch(self, patch: serde_json::Value) -> Self {
        Self {
            value: self.value.patch(patch),
            id: self.id,
        }
    }
}

impl SensorBuilder {
    fn reading(&self) -> Option<f64> {
        self.value
            .get("Reading")
            .and_then(serde_json::Value::as_f64)
    }

    fn threshold(&self, threshold_name: &str) -> Option<f64> {
        self.value
            .get("Thresholds")
            .and_then(|thresholds| thresholds.get(threshold_name))
            .and_then(|threshold| threshold.get("Reading"))
            .and_then(serde_json::Value::as_f64)
    }

    fn health_status(&self) -> redfish::resource::Status {
        let Some(reading) = self.reading() else {
            return redfish::resource::Status::Ok;
        };

        if let Some(upper_fatal) = self.threshold("UpperFatal")
            && reading >= upper_fatal
        {
            return redfish::resource::Status::Critical;
        }

        if let Some(lower_fatal) = self.threshold("LowerFatal")
            && reading <= lower_fatal
        {
            return redfish::resource::Status::Critical;
        }

        if let Some(upper_critical) = self.threshold("UpperCritical")
            && reading >= upper_critical
        {
            return redfish::resource::Status::Critical;
        }

        if let Some(lower_critical) = self.threshold("LowerCritical")
            && reading <= lower_critical
        {
            return redfish::resource::Status::Critical;
        }

        if let Some(upper_caution) = self.threshold("UpperCaution")
            && reading >= upper_caution
        {
            return redfish::resource::Status::Warning;
        }

        if let Some(lower_caution) = self.threshold("LowerCaution")
            && reading <= lower_caution
        {
            return redfish::resource::Status::Warning;
        }

        redfish::resource::Status::Ok
    }

    pub(crate) fn name(self, value: &str) -> Self {
        self.add_str_field("Name", value)
    }

    pub(crate) fn reading_f64(self, value: f64) -> Self {
        self.apply_patch(json!({ "Reading": value }))
    }

    fn reading_u32(self, value: u32) -> Self {
        self.apply_patch(json!({ "Reading": value }))
    }

    pub(crate) fn reading_type(self, value: &str) -> Self {
        self.add_str_field("ReadingType", value)
    }

    pub(crate) fn reading_units(self, value: &str) -> Self {
        self.add_str_field("ReadingUnits", value)
    }

    fn physical_context(self, value: &str) -> Self {
        self.add_str_field("PhysicalContext", value)
    }

    fn threshold_lower_critical(self, value: f64) -> Self {
        self.apply_patch(json!({
            "Thresholds": {
                "LowerCritical": {
                    "Reading": value
                }
            }
        }))
    }

    fn threshold_lower_fatal(self, value: f64) -> Self {
        self.apply_patch(json!({
            "Thresholds": {
                "LowerFatal": {
                    "Reading": value
                }
            }
        }))
    }

    pub(crate) fn threshold_lower_caution(self, value: f64) -> Self {
        self.apply_patch(json!({
            "Thresholds": {
                "LowerCaution": {
                    "Reading": value
                }
            }
        }))
    }

    pub(crate) fn threshold_upper_caution(self, value: f64) -> Self {
        self.apply_patch(json!({
            "Thresholds": {
                "UpperCaution": {
                    "Reading": value
                }
            }
        }))
    }

    pub(crate) fn threshold_upper_critical(self, value: f64) -> Self {
        self.apply_patch(json!({
            "Thresholds": {
                "UpperCritical": {
                    "Reading": value
                }
            }
        }))
    }

    fn threshold_upper_fatal(self, value: f64) -> Self {
        self.apply_patch(json!({
            "Thresholds": {
                "UpperFatal": {
                    "Reading": value
                }
            }
        }))
    }

    fn thresholds(mut self, thresholds: Thresholds) -> Self {
        if let Some(value) = thresholds.lower_fatal {
            self = self.threshold_lower_fatal(value);
        }
        if let Some(value) = thresholds.lower_critical {
            self = self.threshold_lower_critical(value);
        }
        if let Some(value) = thresholds.lower_caution {
            self = self.threshold_lower_caution(value);
        }
        if let Some(value) = thresholds.upper_caution {
            self = self.threshold_upper_caution(value);
        }
        if let Some(value) = thresholds.upper_critical {
            self = self.threshold_upper_critical(value);
        }
        if let Some(value) = thresholds.upper_fatal {
            self = self.threshold_upper_fatal(value);
        }
        self
    }

    pub(crate) fn build(self) -> Sensor {
        let status = self.health_status();
        Sensor {
            id: self.id,
            value: self.value.patch(json!({
                "Status": status.into_json(),
            })),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum SensorKind {
    Temperature,
    Fan,
    Power,
    Current,
    Voltage,
}

enum SensorReading {
    Float(f64),
    Unsigned(u32),
}

impl SensorReading {
    fn into_json(self) -> serde_json::Value {
        match self {
            Self::Float(value) => json!(value),
            Self::Unsigned(value) => json!(value),
        }
    }

    fn apply(self, sensor: SensorBuilder) -> SensorBuilder {
        match self {
            Self::Float(value) => sensor.reading_f64(value),
            Self::Unsigned(value) => sensor.reading_u32(value),
        }
    }
}

impl SensorKind {
    fn from_reading_type(value: &str) -> Option<Self> {
        match value {
            "Temperature" => Some(Self::Temperature),
            "Rotational" => Some(Self::Fan),
            "Power" => Some(Self::Power),
            "Current" => Some(Self::Current),
            "Voltage" => Some(Self::Voltage),
            _ => None,
        }
    }

    fn id_prefix(&self) -> &'static str {
        match self {
            SensorKind::Temperature => "Temp",
            SensorKind::Fan => "Fan",
            SensorKind::Power => "Power",
            SensorKind::Current => "Current",
            SensorKind::Voltage => "Voltage",
        }
    }

    fn name_prefix(&self) -> &'static str {
        match self {
            SensorKind::Temperature => "Temperature Sensor",
            SensorKind::Fan => "Fan Sensor",
            SensorKind::Power => "Power Sensor",
            SensorKind::Current => "Current Sensor",
            SensorKind::Voltage => "Voltage Sensor",
        }
    }

    fn reading_type(&self) -> &'static str {
        match self {
            SensorKind::Temperature => "Temperature",
            SensorKind::Fan => "Rotational",
            SensorKind::Power => "Power",
            SensorKind::Current => "Current",
            SensorKind::Voltage => "Voltage",
        }
    }

    fn reading_units(&self) -> &'static str {
        match self {
            SensorKind::Temperature => "Cel",
            SensorKind::Fan => "RPM",
            SensorKind::Power => "W",
            SensorKind::Current => "A",
            SensorKind::Voltage => "V",
        }
    }

    fn physical_context(&self) -> &'static str {
        match self {
            SensorKind::Temperature => "Intake",
            SensorKind::Fan => "Fan",
            SensorKind::Power => "PowerSupply",
            SensorKind::Current => "SystemBoard",
            SensorKind::Voltage => "SystemBoard",
        }
    }

    fn random_reading(self, rng: &mut impl Rng) -> SensorReading {
        match self {
            Self::Temperature => SensorReading::Float(random_tenths(rng, 25.0..=37.0)),
            Self::Fan => SensorReading::Unsigned(rng.random_range(5000..=9400)),
            Self::Power => SensorReading::Float(random_tenths(rng, 220.0..=680.0)),
            Self::Current => SensorReading::Float(random_tenths(rng, 1.5..=42.0)),
            Self::Voltage => SensorReading::Float(random_tenths(rng, 1.55..=1.85)),
        }
    }

    fn default_thresholds(self) -> Thresholds {
        match self {
            Self::Temperature => Thresholds {
                upper_caution: Some(38.0),
                upper_critical: Some(40.0),
                upper_fatal: Some(42.0),
                ..Default::default()
            },
            Self::Fan => Thresholds {
                lower_critical: Some(500.0),
                lower_fatal: Some(10.0),
                ..Default::default()
            },
            Self::Power => Thresholds {
                lower_caution: Some(200.0),
                upper_caution: Some(700.0),
                ..Default::default()
            },
            Self::Current => Thresholds::default(),
            Self::Voltage => Thresholds {
                lower_critical: Some(1.5),
                ..Default::default()
            },
        }
    }

    fn with_random_reading(self, sensor: SensorBuilder, rng: &mut impl Rng) -> SensorBuilder {
        self.random_reading(rng).apply(sensor)
    }
}

pub(crate) fn generate_chassis_sensors(chassis_id: &str, layout: Layout) -> Vec<Sensor> {
    let mut rng = rand::rng();
    let mut sensors = Vec::with_capacity(layout.total());
    append_sensors(
        &mut sensors,
        chassis_id,
        layout.temperature,
        SensorKind::Temperature,
        &mut rng,
    );
    append_sensors(
        &mut sensors,
        chassis_id,
        layout.fan,
        SensorKind::Fan,
        &mut rng,
    );
    append_sensors(
        &mut sensors,
        chassis_id,
        layout.power,
        SensorKind::Power,
        &mut rng,
    );
    append_sensors(
        &mut sensors,
        chassis_id,
        layout.current,
        SensorKind::Current,
        &mut rng,
    );
    append_sensors(
        &mut sensors,
        chassis_id,
        layout.voltage,
        SensorKind::Voltage,
        &mut rng,
    );
    sensors
}

pub(crate) fn sensor_id(kind: SensorKind, index: usize) -> String {
    format!("{}_{}", kind.id_prefix(), index)
}

fn append_sensors(
    sensors: &mut Vec<Sensor>,
    chassis_id: &str,
    count: usize,
    kind: SensorKind,
    rng: &mut impl Rng,
) {
    for index in 1..=count {
        let sensor_id = sensor_id(kind, index);
        let sensor_name = format!("{} {}", kind.name_prefix(), index);
        let sensor = builder(&chassis_resource(chassis_id, &sensor_id))
            .name(&sensor_name)
            .reading_type(kind.reading_type())
            .reading_units(kind.reading_units())
            .physical_context(kind.physical_context());
        let sensor = kind
            .with_random_reading(sensor, rng)
            .thresholds(kind.default_thresholds());
        sensors.push(sensor.build());
    }
}

fn random_tenths(rng: &mut impl Rng, range: std::ops::RangeInclusive<f64>) -> f64 {
    let value = rng.random_range(range);
    (value * 10.0).round() / 10.0
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::{Layout, Thresholds, builder, chassis_resource, generate_chassis_sensors};

    #[test]
    fn generated_sensors_follow_layout_and_ranges() {
        let sensors = generate_chassis_sensors(
            "System.Embedded.1",
            Layout {
                temperature: 10,
                fan: 10,
                power: 20,
                current: 10,
                voltage: 4,
            },
        );
        assert_eq!(sensors.len(), 54);

        let mut by_type: HashMap<String, usize> = HashMap::new();
        for sensor in sensors {
            let json = sensor.to_json();
            let reading_type = json["ReadingType"].as_str().unwrap().to_string();
            let reading = json["Reading"].as_f64().unwrap();
            *by_type.entry(reading_type.clone()).or_default() += 1;

            match reading_type.as_str() {
                "Temperature" => assert!((25.0..=45.0).contains(&reading)),
                "Rotational" => assert!((0.0..=9400.0).contains(&reading)),
                "Power" => assert!((130.0..=780.0).contains(&reading)),
                "Current" => assert!((1.5..=42.0).contains(&reading)),
                "Voltage" => assert!((1.2..=1.85).contains(&reading)),
                _ => panic!("unexpected ReadingType"),
            }
        }

        assert_eq!(by_type.get("Temperature").copied().unwrap_or_default(), 10);
        assert_eq!(by_type.get("Rotational").copied().unwrap_or_default(), 10);
        assert_eq!(by_type.get("Power").copied().unwrap_or_default(), 20);
        assert_eq!(by_type.get("Current").copied().unwrap_or_default(), 10);
        assert_eq!(by_type.get("Voltage").copied().unwrap_or_default(), 4);
    }

    #[test]
    fn sensor_status_is_ok_when_reading_within_thresholds() {
        let sensor = builder(&chassis_resource("System.Embedded.1", "Voltage_1"))
            .reading_f64(1.7)
            .thresholds(Thresholds {
                lower_critical: Some(1.5),
                upper_critical: Some(1.8),
                ..Default::default()
            })
            .build();

        let json = sensor.to_json();
        assert_eq!(json["Status"]["Health"], "OK");
    }

    #[test]
    fn sensor_status_is_warning_when_crossing_caution_threshold() {
        let sensor = builder(&chassis_resource("System.Embedded.1", "Temp_1"))
            .reading_f64(39.0)
            .thresholds(Thresholds {
                upper_caution: Some(38.0),
                upper_critical: Some(40.0),
                ..Default::default()
            })
            .build();

        let json = sensor.to_json();
        assert_eq!(json["Status"]["Health"], "Warning");
    }

    #[test]
    fn sensor_status_is_critical_when_crossing_critical_threshold() {
        let sensor = builder(&chassis_resource("System.Embedded.1", "Voltage_1"))
            .reading_f64(1.4)
            .thresholds(Thresholds {
                lower_critical: Some(1.5),
                ..Default::default()
            })
            .build();

        let json = sensor.to_json();
        assert_eq!(json["Status"]["Health"], "Critical");
    }

    #[test]
    fn sensor_status_is_fatal_when_crossing_fatal_threshold() {
        let sensor = builder(&chassis_resource("System.Embedded.1", "Temp_1"))
            .reading_f64(45.0)
            .thresholds(Thresholds {
                upper_critical: Some(40.0),
                upper_fatal: Some(44.0),
                ..Default::default()
            })
            .build();

        let json = sensor.to_json();
        assert_eq!(json["Status"]["Health"], "Critical");
        assert_eq!(json["Thresholds"]["UpperFatal"]["Reading"], 44.0);
    }
}
