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
use std::sync::Arc;

use nv_redfish_server_core::{Enum, FiniteF64};
use rand::{Rng, RngExt};
use serde_json::json;

use super::schema::sensor::{self as schema, Sensor as SensorModel, Threshold, Thresholds};
use crate::json::JsonExt;
use crate::redfish;

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
        odata_type: Cow::Borrowed(redfish::schema::sensor::Sensor::ODATA_TYPE),
        id: Cow::Borrowed(sensor_id),
        name: Cow::Borrowed("Sensor"),
    }
}

pub(crate) fn builder(resource: &redfish::Resource) -> SensorModel {
    SensorModel::new(
        resource.odata_id.as_ref(),
        resource.id.as_ref(),
        resource.name.as_ref(),
    )
    .status(redfish::resource::Status::Ok.into())
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
    value: Arc<SensorModel>,
}

impl Sensor {
    pub(crate) fn to_json(&self) -> serde_json::Value {
        let document =
            serde_json::to_value(&*self.value).expect("generated sensor is serializable");
        let Some(kind) = self
            .value
            .reading_type
            .flatten()
            .and_then(|t| SensorKind::from_reading_type(t.as_str()))
        else {
            return document;
        };
        let reading = kind.random_reading(&mut rand::rng());
        let status = self.value.health_for_reading(reading.as_f64());
        document.patch(json!({"Reading": reading.into_json(), "Status": status.into_json()}))
    }
}

impl SensorModel {
    fn health_for_reading(&self, reading: f64) -> redfish::resource::Status {
        let Some(thresholds) = &self.thresholds else {
            return redfish::resource::Status::Ok;
        };
        let threshold_reading = |value: &Option<schema::Threshold>| {
            value
                .as_ref()
                .and_then(|v| v.reading.flatten())
                .map(FiniteF64::get)
        };

        if let Some(upper_fatal) = threshold_reading(&thresholds.upper_fatal)
            && reading >= upper_fatal
        {
            return redfish::resource::Status::Critical;
        }

        if let Some(lower_fatal) = threshold_reading(&thresholds.lower_fatal)
            && reading <= lower_fatal
        {
            return redfish::resource::Status::Critical;
        }

        if let Some(upper_critical) = threshold_reading(&thresholds.upper_critical)
            && reading >= upper_critical
        {
            return redfish::resource::Status::Critical;
        }

        if let Some(lower_critical) = threshold_reading(&thresholds.lower_critical)
            && reading <= lower_critical
        {
            return redfish::resource::Status::Critical;
        }

        if let Some(upper_caution) = threshold_reading(&thresholds.upper_caution)
            && reading >= upper_caution
        {
            return redfish::resource::Status::Warning;
        }

        if let Some(lower_caution) = threshold_reading(&thresholds.lower_caution)
            && reading <= lower_caution
        {
            return redfish::resource::Status::Warning;
        }

        redfish::resource::Status::Ok
    }

    pub(crate) fn reading_f64(self, value: f64) -> Self {
        self.reading(FiniteF64::try_from(value).expect("sensor reading is finite"))
    }

    fn reading_u32(self, value: u32) -> Self {
        self.reading_f64(f64::from(value))
    }

    pub(crate) fn build(self) -> Sensor {
        let status = self
            .reading
            .flatten()
            .map(|v| self.health_for_reading(v.get()))
            .unwrap_or(redfish::resource::Status::Ok);
        Sensor {
            id: self.id.clone().into(),
            value: Arc::new(self.status(status.into())),
        }
    }
}

impl Threshold {
    pub(crate) fn reading_f64(self, value: f64) -> Self {
        self.reading(FiniteF64::try_from(value).expect("sensor threshold is finite"))
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
    fn as_f64(&self) -> f64 {
        match self {
            Self::Float(v) => *v,
            Self::Unsigned(v) => f64::from(*v),
        }
    }

    fn into_json(self) -> serde_json::Value {
        match self {
            Self::Float(value) => json!(value),
            Self::Unsigned(value) => json!(value),
        }
    }

    fn apply(self, sensor: SensorModel) -> SensorModel {
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

    fn reading_type(&self) -> schema::ReadingType {
        match self {
            SensorKind::Temperature => schema::ReadingType::Temperature,
            SensorKind::Fan => schema::ReadingType::Rotational,
            SensorKind::Power => schema::ReadingType::Power,
            SensorKind::Current => schema::ReadingType::Current,
            SensorKind::Voltage => schema::ReadingType::Voltage,
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

    fn physical_context(&self) -> super::schema::physical_context::PhysicalContext {
        match self {
            SensorKind::Temperature => super::schema::physical_context::PhysicalContext::Intake,
            SensorKind::Fan => super::schema::physical_context::PhysicalContext::Fan,
            SensorKind::Power => super::schema::physical_context::PhysicalContext::PowerSupply,
            SensorKind::Current => super::schema::physical_context::PhysicalContext::SystemBoard,
            SensorKind::Voltage => super::schema::physical_context::PhysicalContext::SystemBoard,
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

    fn default_thresholds(self) -> Option<Thresholds> {
        match self {
            Self::Temperature => Some(
                Thresholds::new()
                    .upper_caution(Threshold::new().reading_f64(38.0))
                    .upper_critical(Threshold::new().reading_f64(40.0))
                    .upper_fatal(Threshold::new().reading_f64(42.0)),
            ),
            Self::Fan => Some(
                Thresholds::new()
                    .lower_critical(Threshold::new().reading_f64(500.0))
                    .lower_fatal(Threshold::new().reading_f64(10.0)),
            ),
            Self::Power => Some(
                Thresholds::new()
                    .lower_caution(Threshold::new().reading_f64(200.0))
                    .upper_caution(Threshold::new().reading_f64(700.0)),
            ),
            Self::Current => None,
            Self::Voltage => {
                Some(Thresholds::new().lower_critical(Threshold::new().reading_f64(1.5)))
            }
        }
    }

    fn with_random_reading(self, sensor: SensorModel, rng: &mut impl Rng) -> SensorModel {
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
        let sensor = SensorModel {
            thresholds: kind.default_thresholds(),
            ..builder(&chassis_resource(chassis_id, &sensor_id))
                .name(&sensor_name)
                .reading_type(kind.reading_type())
                .reading_units(kind.reading_units())
                .physical_context(kind.physical_context())
        };
        let sensor = kind.with_random_reading(sensor, rng);
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

    use super::{
        Layout, Threshold, Thresholds, builder, chassis_resource, generate_chassis_sensors,
    };

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
                "Current" => {
                    assert!((1.5..=42.0).contains(&reading));
                    assert!(json.get("Thresholds").is_none());
                }
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
            .thresholds(
                Thresholds::new()
                    .lower_critical(Threshold::new().reading_f64(1.5))
                    .upper_critical(Threshold::new().reading_f64(1.8)),
            )
            .build();

        let json = sensor.to_json();
        assert_eq!(json["Status"]["Health"], "OK");
    }

    #[test]
    fn sensor_status_is_warning_when_crossing_caution_threshold() {
        let sensor = builder(&chassis_resource("System.Embedded.1", "Temp_1"))
            .reading_f64(39.0)
            .thresholds(
                Thresholds::new()
                    .upper_caution(Threshold::new().reading_f64(38.0))
                    .upper_critical(Threshold::new().reading_f64(40.0)),
            )
            .build();

        let json = sensor.to_json();
        assert_eq!(json["Status"]["Health"], "Warning");
    }

    #[test]
    fn sensor_status_is_critical_when_crossing_critical_threshold() {
        let sensor = builder(&chassis_resource("System.Embedded.1", "Voltage_1"))
            .reading_f64(1.4)
            .thresholds(Thresholds::new().lower_critical(Threshold::new().reading_f64(1.5)))
            .build();

        let json = sensor.to_json();
        assert_eq!(json["Status"]["Health"], "Critical");
    }

    #[test]
    fn sensor_status_is_fatal_when_crossing_fatal_threshold() {
        let sensor = builder(&chassis_resource("System.Embedded.1", "Temp_1"))
            .reading_f64(45.0)
            .thresholds(
                Thresholds::new()
                    .upper_critical(Threshold::new().reading_f64(40.0))
                    .upper_fatal(Threshold::new().reading_f64(44.0)),
            )
            .build();

        let json = sensor.to_json();
        assert_eq!(json["Status"]["Health"], "Critical");
        assert_eq!(json["Thresholds"]["UpperFatal"]["Reading"], 44.0);
    }
}
