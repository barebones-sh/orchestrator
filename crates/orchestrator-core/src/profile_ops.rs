//! Profile CRUD logic operating on an in-memory `Config`, shared between
//! `orchestrator-cli` and `orchestrator-gui` (design spec
//! docs/superpowers/specs/2026-09-13-gui-design.md §5). Notation-string
//! parsing (the CLI's `key:<combo>:<delay_ms>` mini-language) stays in
//! `orchestrator-cli` -- everything here operates on already-typed
//! `Scope`/`Action` values, since the GUI's forms produce those directly
//! and the CLI resolves notation into these same types before calling in.

use orchestrator_hotkey::KeyCombo;
use orchestrator_window::WindowInfo;

use crate::action::Action;
use crate::profile::{Profile, Scope};
use crate::Config;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProfileOpError {
    #[error("no profile named {0:?}")]
    NoSuchProfile(String),
    #[error("a profile named {0:?} already exists")]
    DuplicateProfileName(String),
    // Only ever constructed by `resolve_window_scope`, itself only called
    // from each frontend's Linux-only window picker (CLI's
    // `linux_window_picker`, GUI's `list_windows` command). The logic is
    // deliberately portable/OS-independent (kept unit-testable on any
    // platform) -- `allow(dead_code)` on non-Linux rather than cfg-gating
    // the variants away, so they (and their unit tests) keep compiling and
    // meaning something on every target. (Carried over verbatim from the
    // pre-refactor `orchestrator-cli::profile_commands::ProfileCommandError`.)
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    #[error("no windows are currently open to choose from")]
    NoWindowsAvailable,
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    #[error("chosen window index {chosen} is out of range (0..{available})")]
    WindowIndexOutOfRange { chosen: usize, available: usize },
}

pub fn placeholder_trigger() -> KeyCombo {
    KeyCombo {
        modifiers: vec![],
        key: String::new(),
    }
}

pub fn focus_steal_for(scope: &Scope) -> bool {
    matches!(scope, Scope::Window { .. })
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn resolve_window_scope(
    windows: &[WindowInfo],
    chosen_index: usize,
) -> Result<Scope, ProfileOpError> {
    if windows.is_empty() {
        return Err(ProfileOpError::NoWindowsAvailable);
    }
    let window = windows
        .get(chosen_index)
        .ok_or(ProfileOpError::WindowIndexOutOfRange {
            chosen: chosen_index,
            available: windows.len(),
        })?;
    Ok(Scope::Window {
        process_name: window.process_name.clone().unwrap_or_default(),
        window_title_hint: window.title.clone(),
        backend_hint_id: Some(window.handle.0.clone()),
    })
}

#[derive(Debug, Clone)]
pub struct AddProfileArgs {
    pub name: String,
    pub scope: Scope,
    pub action: Action,
    pub debounce_ms: Option<u32>,
}

#[derive(Debug, Clone, Default)]
pub struct EditProfileArgs {
    pub scope: Option<Scope>,
    pub action: Option<Action>,
    pub debounce_ms: Option<u32>,
}

pub fn add_profile(config: &mut Config, args: AddProfileArgs) -> Result<(), ProfileOpError> {
    if config.profiles.iter().any(|p| p.name == args.name) {
        return Err(ProfileOpError::DuplicateProfileName(args.name));
    }
    let profile = Profile {
        name: args.name,
        trigger: placeholder_trigger(),
        focus_steal: focus_steal_for(&args.scope),
        scope: args.scope,
        action: args.action,
        debounce_ms: args.debounce_ms.unwrap_or(400),
    };
    config.profiles.push(profile);
    Ok(())
}

pub fn edit_profile(
    config: &mut Config,
    name: &str,
    args: EditProfileArgs,
) -> Result<(), ProfileOpError> {
    // Unlike the pre-refactor CLI version, there is no fallible parsing step
    // here (the caller resolves notation, if any, before calling this) --
    // the only failure mode is NoSuchProfile, checked before any mutation,
    // so atomicity is automatic.
    let profile = config
        .profiles
        .iter_mut()
        .find(|p| p.name == name)
        .ok_or_else(|| ProfileOpError::NoSuchProfile(name.to_string()))?;

    if let Some(scope) = args.scope {
        profile.focus_steal = focus_steal_for(&scope);
        profile.scope = scope;
    }
    if let Some(action) = args.action {
        profile.action = action;
    }
    if let Some(debounce_ms) = args.debounce_ms {
        profile.debounce_ms = debounce_ms;
    }
    Ok(())
}

pub fn remove_profile(config: &mut Config, name: &str) -> Result<(), ProfileOpError> {
    let before = config.profiles.len();
    config.profiles.retain(|p| p.name != name);
    if config.profiles.len() == before {
        return Err(ProfileOpError::NoSuchProfile(name.to_string()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action::{Jitter, MacroStep};
    use orchestrator_hotkey::Modifier;
    use orchestrator_input::InputEvent;
    use orchestrator_window::WindowHandle;

    fn empty_config() -> Config {
        Config {
            schema_version: crate::config::CURRENT_SCHEMA_VERSION,
            profiles: vec![],
        }
    }

    fn sample_windows() -> Vec<WindowInfo> {
        vec![
            WindowInfo {
                handle: WindowHandle("h1".to_string()),
                pid: Some(1),
                process_name: Some("firefox".to_string()),
                title: "Mozilla Firefox".to_string(),
            },
            WindowInfo {
                handle: WindowHandle("h2".to_string()),
                pid: Some(2),
                process_name: Some("code".to_string()),
                title: "orchestrator - VS Code".to_string(),
            },
        ]
    }

    // -- resolve_window_scope ----------------------------------------------

    #[test]
    fn resolve_window_scope_picks_the_chosen_index() {
        let scope = resolve_window_scope(&sample_windows(), 1).unwrap();
        assert_eq!(
            scope,
            Scope::Window {
                process_name: "code".to_string(),
                window_title_hint: "orchestrator - VS Code".to_string(),
                backend_hint_id: Some("h2".to_string()),
            }
        );
    }

    #[test]
    fn resolve_window_scope_rejects_out_of_range_index() {
        let err = resolve_window_scope(&sample_windows(), 5).unwrap_err();
        assert_eq!(
            err,
            ProfileOpError::WindowIndexOutOfRange {
                chosen: 5,
                available: 2
            }
        );
    }

    #[test]
    fn resolve_window_scope_rejects_empty_window_list() {
        let err = resolve_window_scope(&[], 0).unwrap_err();
        assert_eq!(err, ProfileOpError::NoWindowsAvailable);
    }

    // -- add_profile: Desktop + Repeat ---------------------------------------

    #[test]
    fn add_profile_desktop_repeat_writes_placeholder_trigger_and_derived_focus_steal() {
        let mut config = empty_config();
        let args = AddProfileArgs {
            name: "clicker".to_string(),
            scope: Scope::Desktop,
            action: Action::Repeat {
                input: InputEvent::KeyPress(KeyCombo {
                    modifiers: vec![],
                    key: "A".to_string(),
                }),
                interval_ms: 100,
                jitter: Jitter::None,
            },
            debounce_ms: None,
        };
        add_profile(&mut config, args).unwrap();

        assert_eq!(config.profiles.len(), 1);
        let profile = &config.profiles[0];
        assert_eq!(profile.name, "clicker");
        assert_eq!(
            profile.trigger,
            KeyCombo {
                modifiers: vec![],
                key: String::new()
            }
        );
        assert!(
            !profile.focus_steal,
            "Scope::Desktop must derive focus_steal = false"
        );
        assert_eq!(
            profile.debounce_ms, 400,
            "omitted debounce_ms must default to 400"
        );
        assert_eq!(
            profile.action,
            Action::Repeat {
                input: InputEvent::KeyPress(KeyCombo {
                    modifiers: vec![],
                    key: "A".to_string()
                }),
                interval_ms: 100,
                jitter: Jitter::None
            }
        );
    }

    #[test]
    fn add_profile_rejects_duplicate_names() {
        let mut config = empty_config();
        let args = |name: &str| AddProfileArgs {
            name: name.to_string(),
            scope: Scope::Desktop,
            action: Action::Repeat {
                input: InputEvent::Scroll { dx: 0, dy: 1 },
                interval_ms: 50,
                jitter: Jitter::None,
            },
            debounce_ms: None,
        };
        add_profile(&mut config, args("dup")).unwrap();
        let err = add_profile(&mut config, args("dup")).unwrap_err();
        assert_eq!(err, ProfileOpError::DuplicateProfileName("dup".to_string()));
    }

    #[test]
    fn add_profile_window_scope_derives_focus_steal_true() {
        let mut config = empty_config();
        let args = AddProfileArgs {
            name: "windowed".to_string(),
            scope: Scope::Window {
                process_name: "firefox".to_string(),
                window_title_hint: "Mozilla Firefox".to_string(),
                backend_hint_id: Some("h1".to_string()),
            },
            action: Action::Repeat {
                input: InputEvent::Scroll { dx: 0, dy: -1 },
                interval_ms: 100,
                jitter: Jitter::None,
            },
            debounce_ms: Some(250),
        };
        add_profile(&mut config, args).unwrap();
        assert!(config.profiles[0].focus_steal);
        assert_eq!(config.profiles[0].debounce_ms, 250);
    }

    #[test]
    fn add_profile_macro_with_steps_and_loop() {
        let mut config = empty_config();
        let args = AddProfileArgs {
            name: "macro-one".to_string(),
            scope: Scope::Desktop,
            action: Action::Macro {
                steps: vec![
                    MacroStep::Scroll {
                        delay_ms: 20,
                        dx: 0,
                        dy: 1,
                    },
                    MacroStep::KeyPress {
                        combo: KeyCombo {
                            modifiers: vec![Modifier::Ctrl],
                            key: "C".to_string(),
                        },
                        delay_ms: 50,
                    },
                ],
                loop_: true,
            },
            debounce_ms: None,
        };
        add_profile(&mut config, args).unwrap();
        match &config.profiles[0].action {
            Action::Macro { steps, loop_ } => {
                assert_eq!(steps.len(), 2);
                assert!(*loop_);
            }
            other => panic!("expected Action::Macro, got {other:?}"),
        }
    }

    // -- edit_profile ---------------------------------------------------------

    #[test]
    fn edit_profile_overwrites_only_given_fields() {
        let mut config = empty_config();
        add_profile(
            &mut config,
            AddProfileArgs {
                name: "target".to_string(),
                scope: Scope::Desktop,
                action: Action::Repeat {
                    input: InputEvent::KeyPress(KeyCombo {
                        modifiers: vec![],
                        key: "A".to_string(),
                    }),
                    interval_ms: 100,
                    jitter: Jitter::None,
                },
                debounce_ms: Some(400),
            },
        )
        .unwrap();

        edit_profile(
            &mut config,
            "target",
            EditProfileArgs {
                action: Some(Action::Repeat {
                    input: InputEvent::KeyPress(KeyCombo {
                        modifiers: vec![],
                        key: "B".to_string(),
                    }),
                    interval_ms: 200,
                    jitter: Jitter::None,
                }),
                scope: None,
                debounce_ms: None,
            },
        )
        .unwrap();

        let profile = &config.profiles[0];
        assert_eq!(
            profile.debounce_ms, 400,
            "debounce_ms must be unchanged when not given to edit"
        );
        assert_eq!(
            profile.action,
            Action::Repeat {
                input: InputEvent::KeyPress(KeyCombo {
                    modifiers: vec![],
                    key: "B".to_string()
                }),
                interval_ms: 200,
                jitter: Jitter::None
            }
        );
    }

    #[test]
    fn edit_profile_errors_for_unknown_name() {
        let mut config = empty_config();
        let err = edit_profile(
            &mut config,
            "nope",
            EditProfileArgs {
                action: None,
                scope: None,
                debounce_ms: None,
            },
        )
        .unwrap_err();
        assert_eq!(err, ProfileOpError::NoSuchProfile("nope".to_string()));
    }

    #[test]
    fn edit_profile_scope_only_change_flips_focus_steal() {
        let mut config = empty_config();
        add_profile(
            &mut config,
            AddProfileArgs {
                name: "target".to_string(),
                scope: Scope::Desktop,
                action: Action::Repeat {
                    input: InputEvent::KeyPress(KeyCombo {
                        modifiers: vec![],
                        key: "A".to_string(),
                    }),
                    interval_ms: 100,
                    jitter: Jitter::None,
                },
                debounce_ms: None,
            },
        )
        .unwrap();
        assert!(!config.profiles[0].focus_steal);

        edit_profile(
            &mut config,
            "target",
            EditProfileArgs {
                scope: Some(Scope::Window {
                    process_name: "firefox".to_string(),
                    window_title_hint: "Mozilla Firefox".to_string(),
                    backend_hint_id: Some("h1".to_string()),
                }),
                action: None,
                debounce_ms: None,
            },
        )
        .unwrap();
        assert!(
            config.profiles[0].focus_steal,
            "Desktop -> Window edit must flip focus_steal to true"
        );

        edit_profile(
            &mut config,
            "target",
            EditProfileArgs {
                scope: Some(Scope::Desktop),
                action: None,
                debounce_ms: None,
            },
        )
        .unwrap();
        assert!(
            !config.profiles[0].focus_steal,
            "Window -> Desktop edit must flip focus_steal back to false"
        );
    }

    // -- remove_profile --------------------------------------------------------

    #[test]
    fn remove_profile_removes_by_name() {
        let mut config = empty_config();
        add_profile(
            &mut config,
            AddProfileArgs {
                name: "gone".to_string(),
                scope: Scope::Desktop,
                action: Action::Repeat {
                    input: InputEvent::KeyPress(KeyCombo {
                        modifiers: vec![],
                        key: "A".to_string(),
                    }),
                    interval_ms: 100,
                    jitter: Jitter::None,
                },
                debounce_ms: None,
            },
        )
        .unwrap();
        remove_profile(&mut config, "gone").unwrap();
        assert!(config.profiles.is_empty());
    }

    #[test]
    fn remove_profile_errors_for_unknown_name() {
        let mut config = empty_config();
        let err = remove_profile(&mut config, "nope").unwrap_err();
        assert_eq!(err, ProfileOpError::NoSuchProfile("nope".to_string()));
    }
}
