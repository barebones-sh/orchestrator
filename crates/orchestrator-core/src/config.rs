//! `Config` — the on-disk config schema, I/O, and validation (design spec §3
//! "Configuration Format").

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::action::{Action, Jitter};
use crate::error::ConfigError;
use crate::profile::{Profile, Scope};

/// Current on-disk schema version. Loading a config with a newer
/// `schema_version` than this is rejected (design spec §3 "Schema
/// migration").
pub const CURRENT_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Config {
    pub schema_version: u32,
    pub profiles: Vec<Profile>,
}

impl Config {
    /// Resolves the platform default config file path via the `dirs`
    /// crate: `dirs::config_dir()` joined with `orchestrator/config.json`.
    ///
    /// On Linux, `dirs::config_dir()` resolves to `$XDG_CONFIG_HOME` (or
    /// `~/.config` if unset), giving `~/.config/orchestrator/config.json`.
    /// On macOS, `dirs::config_dir()` resolves to
    /// `~/Library/Application Support`, giving
    /// `~/Library/Application Support/orchestrator/config.json` — matching
    /// the design doc's table for both platforms with the same call.
    ///
    /// Panics only if `dirs::config_dir()` returns `None` (extremely rare —
    /// no resolvable home/user profile directory).
    pub fn config_path() -> PathBuf {
        dirs::config_dir()
            .expect("could not determine config directory")
            .join("orchestrator")
            .join("config.json")
    }

    /// Reads and deserializes the config at `path`, migrates it (a no-op
    /// today, see [`migrate`]), and validates it.
    pub fn load(path: &Path) -> Result<Config, ConfigError> {
        let contents = std::fs::read_to_string(path)?;
        let config: Config = serde_json::from_str(&contents)?;
        let config = migrate(config)?;
        config.validate()?;
        Ok(config)
    }

    /// Serializes and atomically writes `self` to `path`: the config is
    /// written to a temp file in the same directory as `path`, then
    /// `std::fs::rename`d into place. Creates the parent directory if it
    /// doesn't already exist.
    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self)?;

        let mut tmp_path = path.as_os_str().to_owned();
        tmp_path.push(".tmp");
        let tmp_path = PathBuf::from(tmp_path);

        std::fs::write(&tmp_path, json)?;
        std::fs::rename(&tmp_path, path)?;
        Ok(())
    }

    /// Validates this config, returning the first validation failure found:
    ///
    /// 1. All `profiles[].name` values are unique (case-sensitive).
    /// 2. Every `Action::Repeat.interval_ms > 0`.
    /// 3. Every `Action::Repeat` with `Jitter::Uniform { range_ms }` has
    ///    `range_ms <= interval_ms`.
    /// 4. Every `Scope::Window` has a non-empty `process_name`
    ///    (`window_title_hint` may legitimately be empty).
    /// 5. Every `Action::Macro.steps` is non-empty.
    pub fn validate(&self) -> Result<(), ConfigError> {
        let mut seen_names = std::collections::HashSet::new();
        for profile in &self.profiles {
            if !seen_names.insert(profile.name.as_str()) {
                return Err(ConfigError::DuplicateProfileName(profile.name.clone()));
            }
        }

        for profile in &self.profiles {
            match &profile.action {
                Action::Repeat {
                    interval_ms,
                    jitter,
                    ..
                } => {
                    if *interval_ms == 0 {
                        return Err(ConfigError::InvalidInterval {
                            profile: profile.name.clone(),
                        });
                    }
                    if let Jitter::Uniform { range_ms } = jitter {
                        if *range_ms > *interval_ms {
                            return Err(ConfigError::JitterRangeExceedsInterval {
                                profile: profile.name.clone(),
                            });
                        }
                    }
                }
                Action::Macro { steps, .. } => {
                    if steps.is_empty() {
                        return Err(ConfigError::EmptyMacroSteps {
                            profile: profile.name.clone(),
                        });
                    }
                }
            }

            if let Scope::Window { process_name, .. } = &profile.scope {
                if process_name.is_empty() {
                    return Err(ConfigError::EmptyWindowProcessName {
                        profile: profile.name.clone(),
                    });
                }
            }
        }

        Ok(())
    }
}

/// Migration hook: today `CURRENT_SCHEMA_VERSION` is 1 (the first shipped
/// version), so there are no `schema_version < CURRENT_SCHEMA_VERSION`
/// migrations to run — configs are accepted as-is. `schema_version >
/// CURRENT_SCHEMA_VERSION` is rejected. This is structured as a distinct
/// step so a future schema bump has an obvious place to add real migration
/// logic without restructuring `Config::load`.
fn migrate(config: Config) -> Result<Config, ConfigError> {
    if config.schema_version > CURRENT_SCHEMA_VERSION {
        return Err(ConfigError::UnsupportedSchemaVersion(config.schema_version));
    }
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action::{Action, Jitter, MacroStep};
    use crate::profile::{Profile, Scope};
    use orchestrator_hotkey::{KeyCombo, Modifier};
    use orchestrator_input::InputEvent;

    fn desktop_repeat_profile(name: &str) -> Profile {
        Profile {
            name: name.to_string(),
            trigger: KeyCombo {
                modifiers: vec![Modifier::Ctrl],
                key: "F9".to_string(),
            },
            action: Action::Repeat {
                input: InputEvent::Scroll { dx: 0, dy: 1 },
                interval_ms: 100,
                jitter: Jitter::None,
            },
            scope: Scope::Desktop,
            focus_steal: false,
            debounce_ms: 400,
        }
    }

    #[test]
    fn validate_passes_for_valid_config() {
        let config = Config {
            schema_version: CURRENT_SCHEMA_VERSION,
            profiles: vec![desktop_repeat_profile("a"), desktop_repeat_profile("b")],
        };
        assert!(config.validate().is_ok());
    }

    #[test]
    fn validate_rejects_duplicate_profile_names() {
        let config = Config {
            schema_version: CURRENT_SCHEMA_VERSION,
            profiles: vec![desktop_repeat_profile("dup"), desktop_repeat_profile("dup")],
        };
        let err = config.validate().unwrap_err();
        assert!(matches!(err, ConfigError::DuplicateProfileName(name) if name == "dup"));
    }

    #[test]
    fn validate_rejects_zero_interval() {
        let mut profile = desktop_repeat_profile("zero-interval");
        profile.action = Action::Repeat {
            input: InputEvent::Scroll { dx: 0, dy: 1 },
            interval_ms: 0,
            jitter: Jitter::None,
        };
        let config = Config {
            schema_version: CURRENT_SCHEMA_VERSION,
            profiles: vec![profile],
        };
        let err = config.validate().unwrap_err();
        assert!(
            matches!(err, ConfigError::InvalidInterval { profile } if profile == "zero-interval")
        );
    }

    #[test]
    fn validate_rejects_jitter_range_exceeding_interval() {
        let mut profile = desktop_repeat_profile("bad-jitter");
        profile.action = Action::Repeat {
            input: InputEvent::Scroll { dx: 0, dy: 1 },
            interval_ms: 100,
            jitter: Jitter::Uniform { range_ms: 200 },
        };
        let config = Config {
            schema_version: CURRENT_SCHEMA_VERSION,
            profiles: vec![profile],
        };
        let err = config.validate().unwrap_err();
        assert!(
            matches!(err, ConfigError::JitterRangeExceedsInterval { profile } if profile == "bad-jitter")
        );
    }

    #[test]
    fn validate_rejects_empty_window_process_name() {
        let mut profile = desktop_repeat_profile("empty-process");
        profile.scope = Scope::Window {
            process_name: String::new(),
            window_title_hint: "Some Window".to_string(),
            backend_hint_id: None,
        };
        let config = Config {
            schema_version: CURRENT_SCHEMA_VERSION,
            profiles: vec![profile],
        };
        let err = config.validate().unwrap_err();
        assert!(
            matches!(err, ConfigError::EmptyWindowProcessName { profile } if profile == "empty-process")
        );
    }

    #[test]
    fn validate_rejects_empty_macro_steps() {
        let mut profile = desktop_repeat_profile("empty-macro");
        profile.action = Action::Macro {
            steps: vec![],
            loop_: false,
        };
        let config = Config {
            schema_version: CURRENT_SCHEMA_VERSION,
            profiles: vec![profile],
        };
        let err = config.validate().unwrap_err();
        assert!(
            matches!(err, ConfigError::EmptyMacroSteps { profile } if profile == "empty-macro")
        );
    }

    #[test]
    fn validate_accepts_empty_window_title_hint() {
        let mut profile = desktop_repeat_profile("only-one-window");
        profile.scope = Scope::Window {
            process_name: "some-app".to_string(),
            window_title_hint: String::new(),
            backend_hint_id: None,
        };
        let config = Config {
            schema_version: CURRENT_SCHEMA_VERSION,
            profiles: vec![profile],
        };
        assert!(config.validate().is_ok());
    }

    #[test]
    fn load_rejects_unsupported_schema_version() {
        let json = r#"{ "schema_version": 2, "profiles": [] }"#;
        let mut path = std::env::temp_dir();
        path.push(format!(
            "orchestrator-core-test-unsupported-schema-{}.json",
            std::process::id()
        ));
        std::fs::write(&path, json).unwrap();

        let result = Config::load(&path);
        let _ = std::fs::remove_file(&path);

        match result {
            Err(ConfigError::UnsupportedSchemaVersion(2)) => {}
            other => panic!("expected UnsupportedSchemaVersion(2), got {other:?}"),
        }
    }

    #[test]
    fn save_then_load_round_trips_through_disk() {
        let config = Config {
            schema_version: CURRENT_SCHEMA_VERSION,
            profiles: vec![
                desktop_repeat_profile("first"),
                Profile {
                    name: "second".to_string(),
                    trigger: KeyCombo {
                        modifiers: vec![Modifier::Alt, Modifier::Shift],
                        key: "Tab".to_string(),
                    },
                    action: Action::Macro {
                        steps: vec![MacroStep::Scroll {
                            delay_ms: 10,
                            dx: 0,
                            dy: -1,
                        }],
                        loop_: true,
                    },
                    scope: Scope::Window {
                        process_name: "firefox".to_string(),
                        window_title_hint: String::new(),
                        backend_hint_id: None,
                    },
                    focus_steal: true,
                    debounce_ms: 250,
                },
            ],
        };

        let mut path = std::env::temp_dir();
        path.push(format!(
            "orchestrator-core-test-round-trip-{}.json",
            std::process::id()
        ));

        config.save(&path).unwrap();
        let loaded = Config::load(&path).unwrap();
        let _ = std::fs::remove_file(&path);

        assert_eq!(config, loaded);
    }
}
