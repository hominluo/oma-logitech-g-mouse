use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::OpenOptionsExt,
    thread,
    time::{Duration, Instant},
};

const VID: u32 = 0x046d;
const REPORT_LONG: u8 = 0x11;
const LONG_LEN: usize = 20;
// HID++ software id, echoed in the low nibble of the response's function
// byte. Cycled per request so a late reply to an earlier call cannot be
// mistaken for the answer to this one — every getFeature call otherwise
// shares an identical header and only the params differ.
const SWID_MIN: u8 = 1;
const SWID_MAX: u8 = 15;

pub(crate) struct Device {
    file: File,
    path: String,
    index: u8,
    swid: u8,
}

impl Device {
    fn open(path: String) -> Result<Self, String> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(0o4000)
            .open(&path)
            .map_err(|e| format!("open {path}: {e}"))?;
        Ok(Self {
            file,
            path,
            index: 1,
            swid: SWID_MIN,
        })
    }

    fn drain(&mut self) {
        let mut buf = [0u8; 64];
        for _ in 0..32 {
            match self.file.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
        }
    }

    pub(crate) fn request(
        &mut self,
        feature: u8,
        function: u8,
        params: &[u8],
        tries: u8,
        timeout: Duration,
    ) -> Result<Vec<u8>, String> {
        self.swid = if self.swid >= SWID_MAX {
            SWID_MIN
        } else {
            self.swid + 1
        };
        let swid = self.swid;
        let mut packet = [0u8; LONG_LEN];
        packet[0] = REPORT_LONG;
        packet[1] = self.index;
        packet[2] = feature;
        packet[3] = (function << 4) | swid;
        for (i, value) in params.iter().take(LONG_LEN - 4).enumerate() {
            packet[4 + i] = *value;
        }
        let mut last_error = None;
        let mut response = [0u8; 64];
        for _ in 0..tries {
            self.drain();
            self.file
                .write_all(&packet)
                .map_err(|e| format!("write {}: {e}", self.path))?;
            let deadline = Instant::now() + timeout;
            while Instant::now() < deadline {
                match self.file.read(&mut response) {
                    Ok(n) if n >= 4 => {
                        // Error report: [_, index, 0xff|0x8f, feature, fn|swid, code].
                        if response[2] == 0x8f || response[2] == 0xff {
                            if n >= 6
                                && response[3] == feature
                                && response[4] >> 4 == function
                                && response[4] & 0x0f == swid
                            {
                                last_error = response.get(5).copied();
                                break;
                            }
                            continue;
                        }
                        if response[1] == self.index
                            && response[2] == feature
                            && response[3] >> 4 == function
                            && response[3] & 0x0f == swid
                        {
                            return Ok(response[4..n].to_vec());
                        }
                    }
                    Ok(_) | Err(_) => {}
                }
                thread::sleep(Duration::from_millis(4));
            }
            thread::sleep(Duration::from_millis(15));
        }
        Err(format!(
            "no response feat=0x{feature:02x} fn={function} (last error={last_error:?})"
        ))
    }

    fn ping(&mut self, index: u8) -> bool {
        let previous = self.index;
        self.index = index;
        if self
            .request(0, 1, &[0, 0, 0x5a], 2, Duration::from_millis(150))
            .is_ok()
        {
            true
        } else {
            self.index = previous;
            false
        }
    }

    pub(crate) fn feature_optional(&mut self, id: u16) -> Option<u8> {
        self.request(
            0,
            0,
            &[(id >> 8) as u8, id as u8],
            2,
            Duration::from_millis(150),
        )
        .ok()
        .and_then(|body| body.first().copied())
        .filter(|index| *index > 0)
    }
}

fn is_logitech_hidraw(name: &str) -> bool {
    let Ok(text) = fs::read_to_string(format!("/sys/class/hidraw/{name}/device/uevent")) else {
        return false;
    };
    text.lines()
        .find_map(|line| line.strip_prefix("HID_ID="))
        .and_then(|id| id.split(':').nth(1))
        .and_then(|vid| u32::from_str_radix(vid, 16).ok())
        == Some(VID)
}

fn has_hidpp_usage(name: &str) -> bool {
    fs::read(format!("/sys/class/hidraw/{name}/device/report_descriptor"))
        .ok()
        .is_some_and(|bytes| bytes.windows(3).any(|chunk| chunk == [0x06, 0x00, 0xff]))
}

fn find_device() -> Result<Device, String> {
    let mut names: Vec<String> = fs::read_dir("/dev")
        .map_err(|e| e.to_string())?
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| name.starts_with("hidraw") && is_logitech_hidraw(name))
        .collect();
    names.sort();
    names.sort_by_key(|name| !has_hidpp_usage(name));
    let mut denied = Vec::new();
    let mut seen = false;
    for name in names {
        seen = true;
        let mut device = match Device::open(format!("/dev/{name}")) {
            Ok(device) => device,
            Err(error) => {
                if error.contains("Permission denied") {
                    denied.push(name);
                }
                continue;
            }
        };
        if [1, 2, 3, 4, 5, 6, 0xff]
            .into_iter()
            .any(|index| device.ping(index))
        {
            return Ok(device);
        }
    }
    if !denied.is_empty() {
        return Err(format!(
            "Permission denied opening {}. Your session has no access to the \
             Logitech HID nodes; see the udev setup in the README.",
            denied.join(", ")
        ));
    }
    if !seen {
        return Err("No Logitech HID device present.".into());
    }
    Err("No Logitech HID++ device found.".into())
}

pub(crate) fn find_ready_device() -> Result<Device, String> {
    let deadline = Instant::now() + Duration::from_secs(5);
    let last_error = loop {
        match find_device() {
            Ok(device) => return Ok(device),
            Err(error) if Instant::now() >= deadline => break error,
            Err(_) => thread::sleep(Duration::from_millis(200)),
        }
    };
    Err(format!(
        "Logitech receiver did not become ready within 5000ms: {last_error}"
    ))
}
