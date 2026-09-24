use std::time::Duration;

use crate::device::Device;

pub(crate) const DPI_PRESETS: [u32; 5] = [800, 1200, 1600, 2400, 3200];

#[derive(Clone, Copy)]
pub(crate) enum BatteryFeature {
    LevelStatus(u8),
    Unified(u8),
}

#[derive(Clone, Copy)]
pub(crate) enum DpiFeature {
    Adjustable(u8),
    Extended(u8),
}

#[derive(Clone, Copy)]
pub(crate) enum ReportRateFeature {
    Adjustable(u8),
    Extended(u8),
}

#[derive(Clone, Copy)]
pub(crate) struct Features {
    pub(crate) name: Option<u8>,
    pub(crate) battery: Option<BatteryFeature>,
    pub(crate) dpi: Option<DpiFeature>,
    pub(crate) hits: Option<u8>,
    pub(crate) profiles: Option<u8>,
    pub(crate) report_rate: Option<ReportRateFeature>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct DpiRange {
    pub(crate) min: u16,
    pub(crate) max: u16,
    pub(crate) presets: Vec<u16>,
    step: u16,
}

impl DpiRange {
    fn normalize(&self, target: u32) -> u16 {
        let target = target.clamp(u32::from(self.min), u32::from(self.max)) as u16;
        if self.step > 0 {
            self.min + (target - self.min) / self.step * self.step
        } else {
            self.presets
                .iter()
                .copied()
                .min_by_key(|value| value.abs_diff(target))
                .unwrap_or(target)
        }
    }
}

pub(crate) fn features(device: &mut Device) -> Features {
    let battery = device
        .feature_optional(0x1004)
        .map(BatteryFeature::Unified)
        .or_else(|| {
            device
                .feature_optional(0x1000)
                .map(BatteryFeature::LevelStatus)
        });
    let dpi = device
        .feature_optional(0x2202)
        .map(DpiFeature::Extended)
        .or_else(|| device.feature_optional(0x2201).map(DpiFeature::Adjustable));
    let report_rate = device
        .feature_optional(0x8061)
        .map(ReportRateFeature::Extended)
        .or_else(|| {
            device
                .feature_optional(0x8060)
                .map(ReportRateFeature::Adjustable)
        });
    Features {
        name: device.feature_optional(0x0005),
        battery,
        dpi,
        hits: device.feature_optional(0x1b0c),
        profiles: device.feature_optional(0x8100),
        report_rate,
    }
}

pub(crate) fn device_name(device: &mut Device, feature: u8) -> Result<String, String> {
    let length = (*device
        .request(feature, 0, &[], 3, Duration::from_millis(400))?
        .first()
        .unwrap_or(&0) as usize)
        .min(128);
    let mut bytes = Vec::with_capacity(length);
    for offset in (0..length).step_by(16) {
        let chunk = device.request(feature, 1, &[offset as u8], 3, Duration::from_millis(400))?;
        bytes.extend_from_slice(&chunk[..chunk.len().min(length - offset)]);
    }
    Ok(String::from_utf8_lossy(&bytes)
        .split('\0')
        .next()
        .unwrap_or("")
        .trim()
        .to_owned())
}

pub(crate) fn battery(
    device: &mut Device,
    feature: BatteryFeature,
) -> Result<(u8, &'static str, &'static str), String> {
    let (body, unified) = match feature {
        BatteryFeature::Unified(feature) => (
            device.request(feature, 1, &[0, 0, 0], 3, Duration::from_millis(400))?,
            true,
        ),
        BatteryFeature::LevelStatus(feature) => (
            device.request(feature, 0, &[], 3, Duration::from_millis(400))?,
            false,
        ),
    };
    let percentage = body.first().copied().unwrap_or(0);
    let level = if unified {
        match body.get(1) {
            Some(1) => "critical",
            Some(2) => "low",
            Some(4) => "good",
            Some(8) => "full",
            _ => "unknown",
        }
    } else {
        match percentage {
            0..=10 => "critical",
            11..=30 => "low",
            31..=99 => "good",
            100 => "full",
            _ => "unknown",
        }
    };
    let status = if unified {
        match body.get(2) {
            Some(0) => "discharging",
            Some(1) => "charging",
            Some(2) => "charging_slow",
            Some(3) => "full",
            Some(4) => "error",
            _ => "unknown",
        }
    } else {
        match body.get(2) {
            Some(0) => "discharging",
            Some(1) | Some(2) => "charging",
            Some(3) => "full",
            Some(4) => "charging_slow",
            Some(5..=7) => "error",
            _ => "unknown",
        }
    };
    Ok((percentage, level, status))
}

pub(crate) fn dpi(
    device: &mut Device,
    feature: DpiFeature,
) -> Result<(u16, u16, u16, u16, &'static str), String> {
    match feature {
        DpiFeature::Extended(feature) => {
            let body = device.request(feature, 5, &[0, 0, 0], 3, Duration::from_millis(400))?;
            let word = |index| {
                u16::from_be_bytes([
                    *body.get(index).unwrap_or(&0),
                    *body.get(index + 1).unwrap_or(&0),
                ])
            };
            let lod = match body.get(9) {
                Some(0) => "unsupported",
                Some(1) => "low",
                Some(2) => "medium",
                Some(3) => "high",
                _ => "unknown",
            };
            Ok((word(1), word(3), word(5), word(7), lod))
        }
        DpiFeature::Adjustable(feature) => {
            let body = device.request(feature, 2, &[0], 3, Duration::from_millis(400))?;
            let word = |index| {
                u16::from_be_bytes([
                    *body.get(index).unwrap_or(&0),
                    *body.get(index + 1).unwrap_or(&0),
                ])
            };
            Ok((word(1), word(3), word(1), word(3), "unsupported"))
        }
    }
}

fn parse_adjustable_dpi_range(body: &[u8]) -> DpiRange {
    let mut values = Vec::new();
    let mut step = 0;
    for index in (1..body.len()).step_by(2) {
        let value = u16::from_be_bytes([
            *body.get(index).unwrap_or(&0),
            *body.get(index + 1).unwrap_or(&0),
        ]);
        if value == 0 {
            break;
        }
        if value > 0xe000 {
            step = value - 0xe000;
        } else {
            values.push(value);
        }
    }
    let min = values.iter().copied().min().unwrap_or(100);
    let max = values.iter().copied().max().unwrap_or(32000);
    let presets = if step > 0 {
        DPI_PRESETS
            .into_iter()
            .map(|value| value as u16)
            .filter(|value| *value >= min && *value <= max)
            .collect()
    } else {
        values
    };
    DpiRange {
        min,
        max,
        presets,
        step,
    }
}

pub(crate) fn dpi_range(device: &mut Device, feature: DpiFeature) -> DpiRange {
    match feature {
        DpiFeature::Extended(feature) => {
            let presets = device
                .request(feature, 3, &[0, 0, 0], 3, Duration::from_millis(400))
                .ok()
                .map(|body| {
                    (0..14)
                        .step_by(2)
                        .map(|index| {
                            u16::from_be_bytes([
                                *body.get(index).unwrap_or(&0),
                                *body.get(index + 1).unwrap_or(&0),
                            ])
                        })
                        .filter(|value| *value > 0)
                        .collect::<Vec<_>>()
                })
                .filter(|values| !values.is_empty())
                .unwrap_or_else(|| DPI_PRESETS.map(|value| value as u16).to_vec());
            let max = device
                .request(feature, 2, &[0, 0, 2], 3, Duration::from_millis(400))
                .ok()
                .and_then(|body| {
                    (0..14)
                        .step_by(2)
                        .map(|index| {
                            u16::from_be_bytes([
                                *body.get(index).unwrap_or(&0),
                                *body.get(index + 1).unwrap_or(&0),
                            ]) as u32
                        })
                        .filter(|value| {
                            *value > 1000
                                && *value <= 44000
                                && (*value % 1000 == 0 || *value == 25600)
                        })
                        .max()
                })
                .unwrap_or(32000)
                .max(32000) as u16;
            DpiRange {
                min: 100,
                max,
                presets,
                step: 50,
            }
        }
        DpiFeature::Adjustable(feature) => {
            let body = device
                .request(feature, 1, &[0], 3, Duration::from_millis(400))
                .unwrap_or_default();
            parse_adjustable_dpi_range(&body)
        }
    }
}

pub(crate) fn set_dpi(device: &mut Device, feature: DpiFeature, target: u32) -> Result<(), String> {
    let range = dpi_range(device, feature);
    let value = range.normalize(target);
    match feature {
        DpiFeature::Extended(feature) => {
            let [high, low] = value.to_be_bytes();
            device
                .request(
                    feature,
                    6,
                    &[0, high, low, high, low, 2],
                    3,
                    Duration::from_millis(400),
                )
                .map(|_| ())
        }
        DpiFeature::Adjustable(feature) => {
            let [high, low] = value.to_be_bytes();
            let response =
                device.request(feature, 3, &[0, high, low], 3, Duration::from_millis(400))?;
            let returned = u16::from_be_bytes([
                *response.get(1).unwrap_or(&0),
                *response.get(2).unwrap_or(&0),
            ]);
            if returned != 0 && returned != value {
                return Err(format!("device returned DPI {returned}, requested {value}"));
            }
            Ok(())
        }
    }
}

pub(crate) fn report_rate(device: &mut Device, feature: ReportRateFeature) -> u32 {
    match feature {
        ReportRateFeature::Extended(feature) => {
            for mode in [1, 0] {
                if let Ok(body) = device.request(feature, 2, &[mode], 3, Duration::from_millis(400))
                {
                    return match body.first() {
                        Some(0) => 125,
                        Some(1) => 250,
                        Some(2) => 500,
                        Some(3) => 1000,
                        Some(4) => 2000,
                        Some(5) => 4000,
                        Some(6) => 8000,
                        _ => 1000,
                    };
                }
            }
            1000
        }
        ReportRateFeature::Adjustable(feature) => device
            .request(feature, 1, &[0], 3, Duration::from_millis(400))
            .ok()
            .and_then(|body| body.first().copied())
            .and_then(|milliseconds| match milliseconds {
                1 | 2 | 4 | 8 => Some(1000 / u32::from(milliseconds)),
                _ => None,
            })
            .unwrap_or(1000),
    }
}

pub(crate) fn set_report_rate(device: &mut Device, feature: ReportRateFeature, rate: u32) {
    match feature {
        ReportRateFeature::Extended(feature) => {
            let code = match rate {
                125 => 0,
                250 => 1,
                500 => 2,
                1000 => 3,
                2000 => 4,
                4000 => 5,
                8000 => 6,
                _ => 3,
            };
            for mode in [0, 1] {
                let _ = device.request(feature, 3, &[code, mode, 0], 3, Duration::from_millis(400));
            }
        }
        ReportRateFeature::Adjustable(feature) => {
            let milliseconds = match rate {
                125 => 8,
                250 => 4,
                500 => 2,
                1000 => 1,
                _ => 1,
            };
            let _ = device.request(feature, 2, &[milliseconds], 3, Duration::from_millis(400));
        }
    }
}

pub(crate) fn button(device: &mut Device, feature: u8, index: u8) -> Result<(u8, u8, u8), String> {
    let body = device.request(feature, 2, &[index], 3, Duration::from_millis(400))?;
    Ok((
        body.get(1).unwrap_or(&0) / 4,
        body.get(2).unwrap_or(&0) / 4,
        body.get(3).unwrap_or(&0) / 4,
    ))
}

pub(crate) fn set_button(
    device: &mut Device,
    feature: u8,
    index: u8,
    actuation: Option<u8>,
    rapid_trigger: Option<u8>,
    haptics: Option<u8>,
) -> Result<(), String> {
    let current = device.request(feature, 2, &[index], 3, Duration::from_millis(400))?;
    let values = [
        index,
        actuation.map_or(*current.get(1).unwrap_or(&0), |value| value * 4),
        rapid_trigger.map_or(*current.get(2).unwrap_or(&0), |value| value * 4),
        haptics.map_or(*current.get(3).unwrap_or(&0), |value| value * 4),
    ];
    device
        .request(feature, 1, &values, 3, Duration::from_millis(400))
        .map(|_| ())
}

pub(crate) fn onboard_mode(device: &mut Device, feature: u8) -> Result<(u8, &'static str), String> {
    let mode = *device
        .request(feature, 2, &[], 3, Duration::from_millis(400))?
        .first()
        .ok_or_else(|| "empty onboard-profile mode response".to_owned())?;
    let label = match mode {
        1 => "onboard",
        2 => "host",
        _ => "unknown",
    };
    Ok((mode, label))
}

pub(crate) fn prefer_host_profile_mode(device: &mut Device, feature: u8) -> Result<(), String> {
    if onboard_mode(device, feature)?.0 == 1 {
        set_onboard_mode(device, feature, 2)?;
    }
    Ok(())
}

pub(crate) fn set_onboard_mode(device: &mut Device, feature: u8, mode: u8) -> Result<(), String> {
    if !matches!(mode, 1 | 2) {
        return Err(format!("invalid onboard-profile mode: {mode}"));
    }
    device
        .request(feature, 1, &[mode], 3, Duration::from_millis(400))
        .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_g304_adjustable_dpi_range() {
        let range =
            parse_adjustable_dpi_range(&[0x00, 0x00, 0xc8, 0xe0, 0x32, 0x2e, 0xe0, 0x00, 0x00]);

        assert_eq!(
            range,
            DpiRange {
                min: 200,
                max: 12000,
                presets: vec![800, 1200, 1600, 2400, 3200],
                step: 50,
            }
        );
        assert_eq!(range.normalize(100), 200);
        assert_eq!(range.normalize(1275), 1250);
        assert_eq!(range.normalize(20000), 12000);
    }
}
