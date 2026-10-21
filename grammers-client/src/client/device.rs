//! Device metadata supplied during connection initialization.

/// Grouped device and language settings for [`super::ClientBuilder::device`].
/// Omitted fields can use the same platform and locale defaults as
/// [`grammers_mtsender::ConnectionParams`].
///
/// ```
/// use grammers_client::{Client, DeviceConfig};
/// let builder = Client::builder(12345).device(DeviceConfig {
///     device_model: "Desktop application".into(),
///     app_version: "1.0".into(),
///     ..Default::default()
/// });
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceConfig {
    /// Device model presented to Telegram.
    pub device_model: String,
    /// Operating system version presented to Telegram.
    pub system_version: String,
    /// Application version presented to Telegram.
    pub app_version: String,
    /// Operating system language code.
    pub system_lang_code: String,
    /// Application language code or language pack name.
    pub lang_code: String,
}

impl Default for DeviceConfig {
    fn default() -> Self {
        let defaults = grammers_mtsender::ConnectionParams::default();
        Self {
            device_model: defaults.device_model,
            system_version: defaults.system_version,
            app_version: defaults.app_version,
            system_lang_code: defaults.system_lang_code,
            lang_code: defaults.lang_code,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_connection_platform_and_locale_defaults() {
        let device = DeviceConfig::default();
        let params = grammers_mtsender::ConnectionParams::default();
        assert_eq!(device.device_model, params.device_model);
        assert_eq!(device.system_version, params.system_version);
        assert_eq!(device.app_version, params.app_version);
        assert_eq!(device.system_lang_code, params.system_lang_code);
        assert_eq!(device.lang_code, params.lang_code);
    }
}
