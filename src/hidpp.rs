use std::time::Duration;

use crate::device::Device;

pub(crate) const DPI_PRESETS: [u32; 5] = [800, 1200, 1600, 2400, 3200];
pub(crate) const REPORT_RATES: [u32; 7] = [125, 250, 500, 1000, 2000, 4000, 8000];

const TIMEOUT: Duration = Duration::from_millis(400);

/// Ceiling assumed for a legacy sensor whose DPI list cannot be read.
const LEGACY_MAX_DPI: u32 = 12000;

/// A HID++ capability Logitech ships in two generations. Recent G mice expose
/// the extended feature id; older ones (G305, G Pro Wireless, ...) expose only
/// the legacy id, which uses a different function layout for the same job.
#[derive(Clone, Copy)]
pub(crate) struct Capability {
    pub(crate) index: u8,
    pub(crate) legacy: bool,
}

#[derive(Clone, Copy)]
pub(crate) struct Features {
    pub(crate) name: Option<u8>,
    pub(crate) battery: Option<Capability>,
    pub(crate) dpi: Option<Capability>,
    pub(crate) hits: Option<u8>,
    pub(crate) profiles: Option<u8>,
    pub(crate) report_rate: Option<Capability>,
}

/// Resolve a feature to its index on this device, preferring the extended
/// generation. A feature the device does not implement stays `None` — never
/// guess an index, because an unrelated feature almost certainly occupies it.
fn capability(device: &mut Device, extended: u16, legacy: u16) -> Option<Capability> {
    if let Some(index) = device.feature_optional(extended) {
        return Some(Capability {
            index,
            legacy: false,
        });
    }
    device.feature_optional(legacy).map(|index| Capability {
        index,
        legacy: true,
    })
}

pub(crate) fn features(device: &mut Device) -> Features {
    Features {
        name: device.feature_optional(0x0005),
        battery: capability(device, 0x1004, 0x1000),
        dpi: capability(device, 0x2202, 0x2201),
        hits: device.feature_optional(0x1b0c),
        profiles: device.feature_optional(0x8100),
        report_rate: capability(device, 0x8061, 0x8060),
    }
}

pub(crate) fn device_name(device: &mut Device, feature: u8) -> Result<String, String> {
    let length = (*device
        .request(feature, 0, &[], 3, TIMEOUT)?
        .first()
        .unwrap_or(&0) as usize)
        .min(128);
    let mut bytes = Vec::with_capacity(length);
    for offset in (0..length).step_by(16) {
        let chunk = device.request(feature, 1, &[offset as u8], 3, TIMEOUT)?;
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
    capability: Capability,
) -> Result<(u8, &'static str, &'static str), String> {
    if capability.legacy {
        return legacy_battery(device, capability.index);
    }
    let body = device.request(capability.index, 1, &[0, 0, 0], 3, TIMEOUT)?;
    let level = match body.get(1) {
        Some(1) => "critical",
        Some(2) => "low",
        Some(4) => "good",
        Some(8) => "full",
        _ => "unknown",
    };
    let status = match body.get(2) {
        Some(0) => "discharging",
        Some(1) => "charging",
        Some(2) => "charging_slow",
        Some(3) => "full",
        Some(4) => "error",
        _ => "unknown",
    };
    Ok((*body.first().unwrap_or(&0), level, status))
}

/// 0x1000 getBatteryLevelStatus: discharge percentage, next level, charge state.
/// It carries no discrete level bitmap, so derive the label from the percentage.
fn legacy_battery(
    device: &mut Device,
    feature: u8,
) -> Result<(u8, &'static str, &'static str), String> {
    let body = device.request(feature, 0, &[], 3, TIMEOUT)?;
    let percentage = *body.first().unwrap_or(&0);
    let status = match body.get(2) {
        Some(0) => "discharging",
        Some(1 | 2) => "charging",
        Some(3) => "full",
        Some(4) => "charging_slow",
        Some(5..=7) => "error",
        _ => "unknown",
    };
    let level = match percentage {
        0..=10 => "critical",
        11..=30 => "low",
        31..=80 => "good",
        _ => "full",
    };
    Ok((percentage, level, status))
}

pub(crate) fn dpi(
    device: &mut Device,
    capability: Capability,
) -> Result<(u16, u16, u16, u16, &'static str), String> {
    if capability.legacy {
        // 0x2201 getSensorDpi: sensor index, current DPI, default DPI. The
        // legacy sensor is single-axis and reports no lift-off distance.
        let body = device.request(capability.index, 2, &[0], 3, TIMEOUT)?;
        let word = |index: usize| {
            u16::from_be_bytes([
                *body.get(index).unwrap_or(&0),
                *body.get(index + 1).unwrap_or(&0),
            ])
        };
        let (current, default) = (word(1), word(3));
        return Ok((current, default, current, default, "unsupported"));
    }
    let body = device.request(capability.index, 5, &[0, 0, 0], 3, TIMEOUT)?;
    let word = |index: usize| {
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

/// What 0x2201 getSensorDpiList reports: either a discrete set of DPI values,
/// or a `min, step-marker, max` triple describing a continuous range.
struct LegacyDpiList {
    values: Vec<u16>,
    step: u16,
}

impl LegacyDpiList {
    fn min(&self) -> u16 {
        self.values.iter().copied().min().unwrap_or(0)
    }

    fn max(&self) -> u16 {
        self.values.iter().copied().max().unwrap_or(0)
    }
}

fn legacy_dpi_list(device: &mut Device, feature: u8) -> Option<LegacyDpiList> {
    let body = device.request(feature, 1, &[0], 3, TIMEOUT).ok()?;
    let mut values = Vec::new();
    let mut step = 0;
    // Byte 0 is the sensor index; 16-bit big-endian entries follow.
    let mut index = 1;
    while index + 1 < body.len() {
        let word = u16::from_be_bytes([body[index], body[index + 1]]);
        index += 2;
        if word == 0 {
            continue;
        }
        if word & 0xe000 == 0xe000 {
            step = word & 0x1fff;
        } else {
            values.push(word);
        }
    }
    (!values.is_empty()).then_some(LegacyDpiList { values, step })
}

fn preset_fallback() -> Vec<u16> {
    DPI_PRESETS.map(|value| value as u16).to_vec()
}

pub(crate) fn dpi_presets(device: &mut Device, capability: Capability) -> Vec<u16> {
    if capability.legacy {
        let Some(list) = legacy_dpi_list(device, capability.index) else {
            return preset_fallback();
        };
        if list.step == 0 {
            return list.values;
        }
        // A continuous range: offer the standard presets that fall inside it.
        let (min, max) = (list.min(), list.max());
        let presets: Vec<u16> = preset_fallback()
            .into_iter()
            .filter(|value| *value >= min && *value <= max)
            .collect();
        return if presets.is_empty() {
            vec![min, max]
        } else {
            presets
        };
    }
    let Ok(body) = device.request(capability.index, 3, &[0, 0, 0], 3, TIMEOUT) else {
        return preset_fallback();
    };
    let values: Vec<u16> = (0..14)
        .step_by(2)
        .map(|index| {
            u16::from_be_bytes([
                *body.get(index).unwrap_or(&0),
                *body.get(index + 1).unwrap_or(&0),
            ])
        })
        .filter(|value| *value > 0)
        .collect();
    if values.is_empty() {
        preset_fallback()
    } else {
        values
    }
}

pub(crate) fn min_dpi(device: &mut Device, capability: Capability) -> u32 {
    if capability.legacy {
        return legacy_dpi_list(device, capability.index).map_or(100, |list| u32::from(list.min()));
    }
    100
}

pub(crate) fn max_dpi(device: &mut Device, capability: Capability) -> u32 {
    if capability.legacy {
        return legacy_dpi_list(device, capability.index)
            .map_or(LEGACY_MAX_DPI, |list| u32::from(list.max()));
    }
    let Ok(body) = device.request(capability.index, 2, &[0, 0, 2], 3, TIMEOUT) else {
        return 32000;
    };
    (0..14)
        .step_by(2)
        .map(|index| {
            u16::from_be_bytes([
                *body.get(index).unwrap_or(&0),
                *body.get(index + 1).unwrap_or(&0),
            ]) as u32
        })
        .filter(|value| *value > 1000 && *value <= 44000 && (*value % 1000 == 0 || *value == 25600))
        .max()
        .unwrap_or(32000)
        .max(32000)
}

pub(crate) fn report_rate(device: &mut Device, capability: Capability) -> u32 {
    if capability.legacy {
        // 0x8060 getReportRate answers with the interval in milliseconds.
        return device
            .request(capability.index, 1, &[], 3, TIMEOUT)
            .ok()
            .and_then(|body| body.first().copied())
            .filter(|interval| *interval > 0)
            .map_or(1000, |interval| 1000 / u32::from(interval));
    }
    for mode in [1, 0] {
        if let Ok(body) = device.request(capability.index, 2, &[mode], 3, TIMEOUT) {
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

/// Report rates the device accepts, ascending. Only the legacy feature
/// advertises a list; extended devices fall back to the standard ladder.
pub(crate) fn report_rates(device: &mut Device, capability: Capability) -> Vec<u32> {
    if !capability.legacy {
        return REPORT_RATES.to_vec();
    }
    let mut rates: Vec<u32> = legacy_supported_intervals(device, capability.index)
        .into_iter()
        .map(|interval| 1000 / u32::from(interval))
        .collect();
    rates.sort_unstable();
    rates.dedup();
    if rates.is_empty() { vec![1000] } else { rates }
}

/// 0x8060 getReportRateList: bit N set means an interval of N+1 milliseconds
/// is supported.
fn legacy_supported_intervals(device: &mut Device, feature: u8) -> Vec<u8> {
    device
        .request(feature, 0, &[], 3, TIMEOUT)
        .ok()
        .and_then(|body| body.first().copied())
        .map(|bits| {
            (0..8)
                .filter(|bit| bits & (1 << bit) != 0)
                .map(|bit| bit + 1)
                .collect()
        })
        .unwrap_or_default()
}

pub(crate) fn button(device: &mut Device, feature: u8, index: u8) -> Result<(u8, u8, u8), String> {
    let body = device.request(feature, 2, &[index], 3, TIMEOUT)?;
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
    let current = device.request(feature, 2, &[index], 3, TIMEOUT)?;
    let values = [
        index,
        actuation.map_or(*current.get(1).unwrap_or(&0), |value| value * 4),
        rapid_trigger.map_or(*current.get(2).unwrap_or(&0), |value| value * 4),
        haptics.map_or(*current.get(3).unwrap_or(&0), |value| value * 4),
    ];
    device.request(feature, 1, &values, 3, TIMEOUT).map(|_| ())
}

pub(crate) fn set_dpi(
    device: &mut Device,
    capability: Capability,
    target: u32,
) -> Result<(), String> {
    if capability.legacy {
        let list = legacy_dpi_list(device, capability.index);
        let (min, max, step) = list
            .as_ref()
            .map_or((100, LEGACY_MAX_DPI as u16, 50), |list| {
                (
                    list.min(),
                    list.max(),
                    if list.step == 0 { 1 } else { list.step },
                )
            });
        let clamped = target.clamp(u32::from(min), u32::from(max)) as u16;
        let value = min + (clamped - min) / step * step;
        let [high, low] = value.to_be_bytes();
        // 0x2201 setSensorDpi: sensor index, DPI.
        return device
            .request(capability.index, 3, &[0, high, low], 3, TIMEOUT)
            .map(|_| ());
    }
    let value = target.clamp(100, 32000) / 50 * 50;
    let [high, low] = (value as u16).to_be_bytes();
    device
        .request(
            capability.index,
            6,
            &[0, high, low, high, low, 2],
            3,
            TIMEOUT,
        )
        .map(|_| ())
}

pub(crate) fn set_report_rate(device: &mut Device, capability: Capability, rate: u32) {
    if capability.legacy {
        let wanted = 1000 / rate.clamp(1, 1000).max(1);
        let supported = legacy_supported_intervals(device, capability.index);
        let interval = supported
            .iter()
            .copied()
            .min_by_key(|value| u32::from(*value).abs_diff(wanted))
            .unwrap_or(wanted.clamp(1, 255) as u8);
        // 0x8060 setReportRate takes the interval in milliseconds.
        let _ = device.request(capability.index, 2, &[interval], 3, TIMEOUT);
        return;
    }
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
        let _ = device.request(capability.index, 3, &[code, mode, 0], 3, TIMEOUT);
    }
}

pub(crate) fn onboard_mode(device: &mut Device, feature: u8) -> Result<(u8, &'static str), String> {
    let mode = *device
        .request(feature, 2, &[], 3, TIMEOUT)?
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
    device.request(feature, 1, &[mode], 3, TIMEOUT).map(|_| ())
}
