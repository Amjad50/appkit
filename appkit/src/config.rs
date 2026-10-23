//! Layered configuration, lowest to highest priority:
//! 1. `T::default()` — defaults live in code;
//! 2. environment: `PREFIX__SECTION__KEY=value` (a root `.env` is loaded first and never
//!    overrides variables that are already set);
//! 3. `--set section.key=value` overrides from the command line.

use serde::Serialize;
use serde::de::DeserializeOwned;

pub fn load<T>(prefix: &str, overrides: &[String]) -> Result<T, config::ConfigError>
where
    T: Default + Serialize + DeserializeOwned,
{
    dotenvy::dotenv().ok();
    let mut builder = config::Config::builder()
        .add_source(config::Config::try_from(&T::default())?)
        .add_source(config::Environment::with_prefix(prefix).prefix_separator("__").separator("__").try_parsing(true));
    for item in overrides {
        let (key, value) =
            item.split_once('=').ok_or_else(|| config::ConfigError::Message(format!("--set expects key=value, got `{item}`")))?;
        builder = builder.set_override(key.trim(), value.trim())?;
    }
    builder.build()?.try_deserialize()
}

#[cfg(test)]
mod tests {
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Default, Serialize, Deserialize, PartialEq)]
    #[serde(default)]
    struct Settings {
        name: String,
        mqtt: Mqtt,
    }
    #[derive(Debug, Serialize, Deserialize, PartialEq)]
    #[serde(default)]
    struct Mqtt {
        port: u16,
        allow_plaintext: bool,
    }
    impl Default for Mqtt {
        fn default() -> Self {
            Self { port: 1883, allow_plaintext: false }
        }
    }

    #[test]
    fn layers_defaults_env_and_overrides() {
        // SAFETY: test-only process environment changes with a unique prefix.
        unsafe { std::env::set_var("APPKITTEST__MQTT__ALLOW_PLAINTEXT", "true") };
        let s: Settings = super::load("APPKITTEST", &["mqtt.port=8883".into(), "name = ward".into()]).unwrap();
        assert_eq!(s, Settings { name: "ward".into(), mqtt: Mqtt { port: 8883, allow_plaintext: true } });
        assert!(super::load::<Settings>("APPKITTEST", &["broken".into()]).is_err());
    }
}
