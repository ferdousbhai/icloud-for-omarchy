//! Devices as the app sees them, parsed from the `content` array of Find My's
//! `initClient` / `refreshClient` responses (field names as in pyicloud's
//! `AppleDevice`).

use serde::Deserialize;

/// One device from Find My.
#[derive(Debug, Clone, PartialEq)]
pub struct Device {
    pub id: String,
    /// The owner's name for it, e.g. "Ferdous's iPhone".
    pub name: String,
    /// Apple's model name, e.g. "iPhone 15 Pro".
    pub model_name: String,
    pub class: DeviceClass,
    /// 0.0..=1.0, `None` when Apple does not know (status "Unknown").
    pub battery: Option<f64>,
    pub charging: bool,
    pub online: bool,
    pub location: Option<Fix>,
    pub can_play_sound: bool,
    pub can_lost_mode: bool,
    pub lost_mode_enabled: bool,
}

/// A position report.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Fix {
    pub lat: f64,
    pub lon: f64,
    /// Horizontal accuracy radius in metres.
    pub accuracy: f64,
    /// Unix milliseconds of the fix.
    pub ts_ms: i64,
    /// Apple marks a fix old when the device has not reported for a while.
    pub is_old: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceClass {
    IPhone,
    IPad,
    Mac,
    Watch,
    AirPods,
    Other,
}

impl DeviceClass {
    fn parse(class: &str) -> Self {
        match class {
            "iPhone" | "iPod" => Self::IPhone,
            "iPad" => Self::IPad,
            "MacBookPro" | "MacBookAir" | "MacBook" | "iMac" | "Mac" | "MacPro" | "MacMini" => Self::Mac,
            "Watch" => Self::Watch,
            "Accessory" | "AirPods" => Self::AirPods,
            c if c.starts_with("Mac") || c.starts_with("iMac") => Self::Mac,
            _ => Self::Other,
        }
    }

    /// A stable lowercase name, for the command line and its JSON.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::IPhone => "iphone",
            Self::IPad => "ipad",
            Self::Mac => "mac",
            Self::Watch => "watch",
            Self::AirPods => "airpods",
            Self::Other => "other",
        }
    }

    /// A symbolic icon from the Adwaita theme.
    pub fn icon_name(self) -> &'static str {
        match self {
            Self::IPhone => "phone-symbolic",
            Self::IPad => "tablet-symbolic",
            Self::Mac => "computer-symbolic",
            Self::Watch => "preferences-system-time-symbolic",
            Self::AirPods => "audio-headphones-symbolic",
            Self::Other => "find-location-symbolic",
        }
    }
}

/// The raw shape of one `content` entry; only the fields we use.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawDevice {
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    device_display_name: Option<String>,
    #[serde(default)]
    device_class: Option<String>,
    #[serde(default)]
    battery_level: Option<f64>,
    #[serde(default)]
    battery_status: Option<String>,
    #[serde(default)]
    device_status: Option<String>,
    #[serde(default)]
    location: Option<RawLocation>,
    #[serde(default)]
    features: Option<std::collections::HashMap<String, serde_json::Value>>,
    #[serde(default)]
    lost_mode_capable: Option<bool>,
    #[serde(default)]
    lost_mode_enabled: Option<bool>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawLocation {
    latitude: f64,
    longitude: f64,
    #[serde(default)]
    horizontal_accuracy: Option<f64>,
    #[serde(default)]
    time_stamp: Option<i64>,
    #[serde(default)]
    is_old: Option<bool>,
}

impl Device {
    /// Parses one `content` entry. `None` if it has no `id`.
    pub fn from_json(v: &serde_json::Value) -> Option<Device> {
        let raw: RawDevice = serde_json::from_value(v.clone()).ok()?;
        let feature = |k: &str| {
            raw.features
                .as_ref()
                .and_then(|f| f.get(k))
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
        };
        let status = raw.battery_status.as_deref().unwrap_or("Unknown");
        let battery = match (raw.battery_level, status) {
            (_, "Unknown") => None,
            (Some(b), _) if (0.0..=1.0).contains(&b) => Some(b),
            _ => None,
        };
        // pyicloud: location is only usable when LOC is on and it is present.
        let location = raw.location.filter(|_| feature("LOC")).map(|l| Fix {
            lat: l.latitude,
            lon: l.longitude,
            accuracy: l.horizontal_accuracy.unwrap_or(0.0),
            ts_ms: l.time_stamp.unwrap_or(0),
            is_old: l.is_old.unwrap_or(false),
        });
        let model_name = raw.device_display_name.unwrap_or_default();
        Some(Device {
            name: raw.name.filter(|n| !n.is_empty()).unwrap_or_else(|| model_name.clone()),
            class: DeviceClass::parse(raw.device_class.as_deref().unwrap_or("")),
            model_name,
            battery,
            charging: matches!(status, "Charging" | "Charged"),
            // deviceStatus: 200 online, 201 offline, 203 pending, 204 unregistered.
            online: raw.device_status.as_deref() == Some("200"),
            location,
            can_play_sound: feature("SND"),
            can_lost_mode: raw.lost_mode_capable.unwrap_or(false),
            lost_mode_enabled: raw.lost_mode_enabled.unwrap_or(false),
            id: raw.id,
        })
    }

    /// "iPhone 15 Pro · 82% · 5 min ago", for the device list.
    pub fn summary(&self, now_ms: i64) -> String {
        let mut parts = vec![];
        if !self.model_name.is_empty() && self.model_name != self.name {
            parts.push(self.model_name.clone());
        }
        if let Some(b) = self.battery {
            let pct = (b * 100.0).round() as i64;
            parts.push(if self.charging {
                format!("{pct}% charging")
            } else {
                format!("{pct}%")
            });
        }
        match &self.location {
            Some(fix) if fix.ts_ms > 0 => parts.push(last_seen(now_ms, fix.ts_ms)),
            _ => parts.push("No location".into()),
        }
        parts.join(" · ")
    }
}

/// An Adwaita `battery-level-N[-charging]-symbolic` icon for a 0..=1 level.
pub fn battery_icon_name(level: f64, charging: bool) -> String {
    let tens = ((level.clamp(0.0, 1.0) * 10.0).round() as u32) * 10;
    match (tens, charging) {
        (100, true) => "battery-level-100-charged-symbolic".into(),
        (n, true) => format!("battery-level-{n}-charging-symbolic"),
        (n, false) => format!("battery-level-{n}-symbolic"),
    }
}

/// "just now", "5 min ago", "3 h ago", "2 days ago".
pub fn last_seen(now_ms: i64, ts_ms: i64) -> String {
    let secs = (now_ms - ts_ms).max(0) / 1000;
    match secs {
        0..60 => "just now".into(),
        60..3600 => format!("{} min ago", secs / 60),
        3600..86_400 => format!("{} h ago", secs / 3600),
        86_400..172_800 => "yesterday".into(),
        _ => format!("{} days ago", secs / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn battery_icons() {
        assert_eq!(battery_icon_name(0.82, false), "battery-level-80-symbolic");
        assert_eq!(battery_icon_name(0.04, true), "battery-level-0-charging-symbolic");
        assert_eq!(battery_icon_name(1.0, true), "battery-level-100-charged-symbolic");
        assert_eq!(battery_icon_name(1.7, false), "battery-level-100-symbolic");
    }

    #[test]
    fn last_seen_buckets() {
        let now = 10_000_000_000;
        assert_eq!(last_seen(now, now - 5_000), "just now");
        assert_eq!(last_seen(now, now - 5 * 60_000), "5 min ago");
        assert_eq!(last_seen(now, now - 3 * 3_600_000), "3 h ago");
        assert_eq!(last_seen(now, now - 30 * 3_600_000), "yesterday");
        assert_eq!(last_seen(now, now - 5 * 86_400_000), "5 days ago");
        assert_eq!(last_seen(now, now + 60_000), "just now");
    }

    #[test]
    fn unknown_battery_is_none_and_no_loc_feature_hides_location() {
        let v = serde_json::json!({
            "id": "x", "name": "", "deviceDisplayName": "MacBook Air",
            "deviceClass": "MacBookAir", "batteryLevel": 0.0, "batteryStatus": "Unknown",
            "features": {"LOC": false},
            "location": {"latitude": 1.0, "longitude": 2.0}
        });
        let d = Device::from_json(&v).unwrap();
        assert_eq!(d.battery, None);
        assert_eq!(d.location, None);
        assert_eq!(d.name, "MacBook Air");
        assert_eq!(d.class, DeviceClass::Mac);
        assert!(!d.online);
    }
}
