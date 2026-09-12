//! Profile CRUD logic operating on an in-memory `Config`. No I/O beyond what
//! `Config::load`/`save` already do, and no live D-Bus/window-listing here —
//! see design spec §5 for why this layer stays synchronous and pure.
//! docs/superpowers/specs/2026-09-12-cli-ux-design.md

use orchestrator_core::action::{Action, Jitter};
use orchestrator_core::profile::{Profile, Scope};
use orchestrator_core::Config;
use orchestrator_hotkey::KeyCombo;
use orchestrator_input::InputEvent;
use orchestrator_window::WindowInfo;

use crate::notation::{parse_macro_step, parse_repeat_input, NotationError};

#[derive(Debug, PartialEq, Eq)]
pub enum ProfileCommandError {
    Notation(String),
    NoSuchProfile(String),
    DuplicateProfileName(String),
    NoWindowsAvailable,
    WindowIndexOutOfRange { chosen: usize, available: usize },
    MissingRequiredField(&'static str),
    RepeatAndMacroBothOrNeitherSpecified,
}

impl std::fmt::Display for ProfileCommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Notation(msg) => write!(f, "{msg}"),
            Self::NoSuchProfile(name) => write!(f, "no profile named {name:?}"),
            Self::DuplicateProfileName(name) => write!(f, "a profile named {name:?} already exists"),
            Self::NoWindowsAvailable => write!(f, "no windows are currently open to choose from"),
            Self::WindowIndexOutOfRange { chosen, available } => {
                write!(f, "chosen window index {chosen} is out of range (0..{available})")
            }
            Self::MissingRequiredField(field) => write!(f, "--{field} is required"),
            Self::RepeatAndMacroBothOrNeitherSpecified => {
                write!(f, "specify exactly one of --action repeat or --action macro")
            }
        }
    }
}

impl std::error::Error for ProfileCommandError {}

impl From<NotationError> for ProfileCommandError {
    fn from(e: NotationError) -> Self {
        Self::Notation(e.to_string())
    }
}

fn placeholder_trigger() -> KeyCombo {
    KeyCombo { modifiers: vec![], key: String::new() }
}

#[derive(Debug, Clone)]
pub enum ProfileActionArgs {
    Repeat { input: String, interval_ms: u64, jitter_ms: Option<u64> },
    Macro { steps: Vec<String>, loop_: bool },
}

#[derive(Debug, Clone)]
pub struct ProfileAddArgs {
    pub name: String,
    pub scope: Scope,
    pub action: ProfileActionArgs,
    pub debounce_ms: Option<u32>,
}

#[derive(Debug, Clone, Default)]
pub struct ProfileEditArgs {
    pub scope: Option<Scope>,
    pub action: Option<ProfileActionArgs>,
    pub debounce_ms: Option<u32>,
}

pub fn resolve_window_scope(windows: &[WindowInfo], chosen_index: usize) -> Result<Scope, ProfileCommandError> {
    if windows.is_empty() {
        return Err(ProfileCommandError::NoWindowsAvailable);
    }
    let window = windows.get(chosen_index).ok_or(ProfileCommandError::WindowIndexOutOfRange {
        chosen: chosen_index,
        available: windows.len(),
    })?;
    Ok(Scope::Window {
        process_name: window.process_name.clone().unwrap_or_default(),
        window_title_hint: window.title.clone(),
        backend_hint_id: Some(window.handle.0.clone()),
    })
}

fn build_action(args: &ProfileActionArgs) -> Result<Action, ProfileCommandError> {
    match args {
        ProfileActionArgs::Repeat { input, interval_ms, jitter_ms } => {
            let event = parse_repeat_input(input)?;
            let jitter = match jitter_ms {
                Some(range_ms) => Jitter::Uniform { range_ms: *range_ms },
                None => Jitter::None,
            };
            Ok(Action::Repeat { input: event, interval_ms: *interval_ms, jitter })
        }
        ProfileActionArgs::Macro { steps, loop_ } => {
            let parsed_steps = steps.iter().map(|s| parse_macro_step(s)).collect::<Result<Vec<_>, _>>()?;
            Ok(Action::Macro { steps: parsed_steps, loop_: *loop_ })
        }
    }
}

fn focus_steal_for(scope: &Scope) -> bool {
    matches!(scope, Scope::Window { .. })
}

pub fn add_profile(config: &mut Config, args: ProfileAddArgs) -> Result<(), ProfileCommandError> {
    if config.profiles.iter().any(|p| p.name == args.name) {
        return Err(ProfileCommandError::DuplicateProfileName(args.name));
    }
    let action = build_action(&args.action)?;
    let profile = Profile {
        name: args.name,
        trigger: placeholder_trigger(),
        action,
        focus_steal: focus_steal_for(&args.scope),
        scope: args.scope,
        debounce_ms: args.debounce_ms.unwrap_or(400),
    };
    config.profiles.push(profile);
    Ok(())
}

pub fn edit_profile(config: &mut Config, name: &str, args: ProfileEditArgs) -> Result<(), ProfileCommandError> {
    // Resolve every fallible field BEFORE writing anything to the profile,
    // so a rejected edit (e.g. bad notation in `args.action`) is a true
    // no-op rather than leaving other fields (like scope/focus_steal)
    // partially applied. `scope`/`debounce_ms` are infallible plain data at
    // this point, but `action` requires notation parsing that can fail.
    let resolved_action = match &args.action {
        Some(action_args) => Some(build_action(action_args)?),
        None => None,
    };

    let profile = config
        .profiles
        .iter_mut()
        .find(|p| p.name == name)
        .ok_or_else(|| ProfileCommandError::NoSuchProfile(name.to_string()))?;

    if let Some(scope) = args.scope {
        profile.focus_steal = focus_steal_for(&scope);
        profile.scope = scope;
    }
    if let Some(action) = resolved_action {
        profile.action = action;
    }
    if let Some(debounce_ms) = args.debounce_ms {
        profile.debounce_ms = debounce_ms;
    }
    Ok(())
}

pub fn remove_profile(config: &mut Config, name: &str) -> Result<(), ProfileCommandError> {
    let before = config.profiles.len();
    config.profiles.retain(|p| p.name != name);
    if config.profiles.len() == before {
        return Err(ProfileCommandError::NoSuchProfile(name.to_string()));
    }
    Ok(())
}

pub fn format_profile_list(config: &Config) -> String {
    let mut out = String::new();
    for profile in &config.profiles {
        let scope_summary = match &profile.scope {
            Scope::Desktop => "Desktop".to_string(),
            Scope::Window { process_name, .. } => format!("Window({process_name})"),
        };
        let action_summary = match &profile.action {
            Action::Repeat { interval_ms, jitter, .. } => match jitter {
                Jitter::None => format!("Repeat every {interval_ms}ms\u{00b1}no jitter"),
                Jitter::Uniform { range_ms } => format!("Repeat every {interval_ms}ms\u{00b1}{range_ms}ms"),
            },
            Action::Macro { steps, loop_ } => {
                format!("Macro ({} steps, {})", steps.len(), if *loop_ { "looping" } else { "once" })
            }
        };
        out.push_str(&format!(
            "{}\t{}\t{}\tdebounce={}ms\n",
            profile.name, scope_summary, action_summary, profile.debounce_ms
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use orchestrator_window::WindowHandle;

    fn empty_config() -> Config {
        Config { schema_version: orchestrator_core::config::CURRENT_SCHEMA_VERSION, profiles: vec![] }
    }

    fn sample_windows() -> Vec<WindowInfo> {
        vec![
            WindowInfo { handle: WindowHandle("h1".to_string()), pid: Some(1), process_name: Some("firefox".to_string()), title: "Mozilla Firefox".to_string() },
            WindowInfo { handle: WindowHandle("h2".to_string()), pid: Some(2), process_name: Some("code".to_string()), title: "orchestrator - VS Code".to_string() },
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
        assert_eq!(err, ProfileCommandError::WindowIndexOutOfRange { chosen: 5, available: 2 });
    }

    #[test]
    fn resolve_window_scope_rejects_empty_window_list() {
        let err = resolve_window_scope(&[], 0).unwrap_err();
        assert_eq!(err, ProfileCommandError::NoWindowsAvailable);
    }

    // -- add_profile: Desktop + Repeat ---------------------------------------

    #[test]
    fn add_profile_desktop_repeat_writes_placeholder_trigger_and_derived_focus_steal() {
        let mut config = empty_config();
        let args = ProfileAddArgs {
            name: "clicker".to_string(),
            scope: Scope::Desktop,
            action: ProfileActionArgs::Repeat { input: "key:A".to_string(), interval_ms: 100, jitter_ms: None },
            debounce_ms: None,
        };
        add_profile(&mut config, args).unwrap();

        assert_eq!(config.profiles.len(), 1);
        let profile = &config.profiles[0];
        assert_eq!(profile.name, "clicker");
        assert_eq!(profile.trigger, KeyCombo { modifiers: vec![], key: String::new() });
        assert!(!profile.focus_steal, "Scope::Desktop must derive focus_steal = false");
        assert_eq!(profile.debounce_ms, 400, "omitted debounce_ms must default to 400");
        assert_eq!(
            profile.action,
            Action::Repeat { input: InputEvent::KeyPress(KeyCombo { modifiers: vec![], key: "A".to_string() }), interval_ms: 100, jitter: Jitter::None }
        );
    }

    #[test]
    fn add_profile_rejects_duplicate_names() {
        let mut config = empty_config();
        let args = |name: &str| ProfileAddArgs {
            name: name.to_string(),
            scope: Scope::Desktop,
            action: ProfileActionArgs::Repeat { input: "scroll:0,1".to_string(), interval_ms: 50, jitter_ms: None },
            debounce_ms: None,
        };
        add_profile(&mut config, args("dup")).unwrap();
        let err = add_profile(&mut config, args("dup")).unwrap_err();
        assert_eq!(err, ProfileCommandError::DuplicateProfileName("dup".to_string()));
    }

    #[test]
    fn add_profile_window_scope_derives_focus_steal_true() {
        let mut config = empty_config();
        let args = ProfileAddArgs {
            name: "windowed".to_string(),
            scope: Scope::Window { process_name: "firefox".to_string(), window_title_hint: "Mozilla Firefox".to_string(), backend_hint_id: Some("h1".to_string()) },
            action: ProfileActionArgs::Repeat { input: "scroll:0,-1".to_string(), interval_ms: 100, jitter_ms: None },
            debounce_ms: Some(250),
        };
        add_profile(&mut config, args).unwrap();
        assert!(config.profiles[0].focus_steal);
        assert_eq!(config.profiles[0].debounce_ms, 250);
    }

    #[test]
    fn add_profile_macro_with_steps_and_loop() {
        let mut config = empty_config();
        let args = ProfileAddArgs {
            name: "macro-one".to_string(),
            scope: Scope::Desktop,
            action: ProfileActionArgs::Macro { steps: vec!["scroll:0,1:20".to_string(), "key:Ctrl+C:50".to_string()], loop_: true },
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

    #[test]
    fn add_profile_rejects_click_in_repeat_mode() {
        let mut config = empty_config();
        let args = ProfileAddArgs {
            name: "bad".to_string(),
            scope: Scope::Desktop,
            action: ProfileActionArgs::Repeat { input: "click:left:cursor".to_string(), interval_ms: 100, jitter_ms: None },
            debounce_ms: None,
        };
        let err = add_profile(&mut config, args).unwrap_err();
        assert!(matches!(err, ProfileCommandError::Notation(_)));
    }

    // -- edit_profile ---------------------------------------------------------

    #[test]
    fn edit_profile_overwrites_only_given_fields() {
        let mut config = empty_config();
        add_profile(&mut config, ProfileAddArgs {
            name: "target".to_string(),
            scope: Scope::Desktop,
            action: ProfileActionArgs::Repeat { input: "key:A".to_string(), interval_ms: 100, jitter_ms: None },
            debounce_ms: Some(400),
        }).unwrap();

        edit_profile(&mut config, "target", ProfileEditArgs { action: Some(ProfileActionArgs::Repeat { input: "key:B".to_string(), interval_ms: 200, jitter_ms: None }), scope: None, debounce_ms: None }).unwrap();

        let profile = &config.profiles[0];
        assert_eq!(profile.debounce_ms, 400, "debounce_ms must be unchanged when not given to edit");
        assert_eq!(
            profile.action,
            Action::Repeat { input: InputEvent::KeyPress(KeyCombo { modifiers: vec![], key: "B".to_string() }), interval_ms: 200, jitter: Jitter::None }
        );
    }

    #[test]
    fn edit_profile_errors_for_unknown_name() {
        let mut config = empty_config();
        let err = edit_profile(&mut config, "nope", ProfileEditArgs { action: None, scope: None, debounce_ms: None }).unwrap_err();
        assert_eq!(err, ProfileCommandError::NoSuchProfile("nope".to_string()));
    }

    #[test]
    fn edit_profile_is_atomic_when_action_notation_fails() {
        let mut config = empty_config();
        add_profile(&mut config, ProfileAddArgs {
            name: "target".to_string(),
            scope: Scope::Desktop,
            action: ProfileActionArgs::Repeat { input: "key:A".to_string(), interval_ms: 100, jitter_ms: None },
            debounce_ms: Some(400),
        }).unwrap();

        let original_action = config.profiles[0].action.clone();

        // Provide both a scope change AND an action whose notation is
        // rejected by parse_repeat_input (click is not allowed in repeat
        // mode). The whole edit must fail, and NEITHER field should be
        // written -- a rejected edit must be a true no-op.
        let err = edit_profile(
            &mut config,
            "target",
            ProfileEditArgs {
                scope: Some(Scope::Window {
                    process_name: "firefox".to_string(),
                    window_title_hint: "Mozilla Firefox".to_string(),
                    backend_hint_id: None,
                }),
                action: Some(ProfileActionArgs::Repeat { input: "click:left:cursor".to_string(), interval_ms: 100, jitter_ms: None }),
                debounce_ms: None,
            },
        )
        .unwrap_err();

        assert!(matches!(err, ProfileCommandError::Notation(_)));
        let profile = &config.profiles[0];
        assert_eq!(profile.scope, Scope::Desktop, "scope must be unchanged when the edit as a whole fails");
        assert!(!profile.focus_steal, "focus_steal must be unchanged when the edit as a whole fails");
        assert_eq!(profile.action, original_action, "action must be unchanged when the edit as a whole fails");
    }

    #[test]
    fn edit_profile_scope_only_change_flips_focus_steal() {
        let mut config = empty_config();
        add_profile(&mut config, ProfileAddArgs {
            name: "target".to_string(),
            scope: Scope::Desktop,
            action: ProfileActionArgs::Repeat { input: "key:A".to_string(), interval_ms: 100, jitter_ms: None },
            debounce_ms: None,
        }).unwrap();
        assert!(!config.profiles[0].focus_steal);

        edit_profile(
            &mut config,
            "target",
            ProfileEditArgs {
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
        assert!(config.profiles[0].focus_steal, "Desktop -> Window edit must flip focus_steal to true");

        edit_profile(&mut config, "target", ProfileEditArgs { scope: Some(Scope::Desktop), action: None, debounce_ms: None }).unwrap();
        assert!(!config.profiles[0].focus_steal, "Window -> Desktop edit must flip focus_steal back to false");
    }

    // -- remove_profile --------------------------------------------------------

    #[test]
    fn remove_profile_removes_by_name() {
        let mut config = empty_config();
        add_profile(&mut config, ProfileAddArgs { name: "gone".to_string(), scope: Scope::Desktop, action: ProfileActionArgs::Repeat { input: "key:A".to_string(), interval_ms: 100, jitter_ms: None }, debounce_ms: None }).unwrap();
        remove_profile(&mut config, "gone").unwrap();
        assert!(config.profiles.is_empty());
    }

    #[test]
    fn remove_profile_errors_for_unknown_name() {
        let mut config = empty_config();
        let err = remove_profile(&mut config, "nope").unwrap_err();
        assert_eq!(err, ProfileCommandError::NoSuchProfile("nope".to_string()));
    }

    // -- format_profile_list -----------------------------------------------------

    #[test]
    fn format_profile_list_repeat_with_jitter_matches_full_expected_line() {
        let mut config = empty_config();
        add_profile(&mut config, ProfileAddArgs { name: "p1".to_string(), scope: Scope::Desktop, action: ProfileActionArgs::Repeat { input: "key:A".to_string(), interval_ms: 100, jitter_ms: Some(20) }, debounce_ms: None }).unwrap();
        let listing = format_profile_list(&config);
        assert_eq!(listing, "p1\tDesktop\tRepeat every 100ms\u{00b1}20ms\tdebounce=400ms\n");
    }

    #[test]
    fn format_profile_list_repeat_without_jitter_shows_no_jitter_suffix() {
        // Design spec §3.5: the action summary is always `Repeat every
        // <interval_ms>ms±<jitter or "no jitter">` -- even with no jitter,
        // the `±` and a placeholder must appear, not be dropped entirely.
        let mut config = empty_config();
        add_profile(&mut config, ProfileAddArgs { name: "p2".to_string(), scope: Scope::Desktop, action: ProfileActionArgs::Repeat { input: "key:A".to_string(), interval_ms: 100, jitter_ms: None }, debounce_ms: None }).unwrap();
        let listing = format_profile_list(&config);
        assert_eq!(listing, "p2\tDesktop\tRepeat every 100ms\u{00b1}no jitter\tdebounce=400ms\n");
    }

    #[test]
    fn format_profile_list_window_scope_matches_full_expected_line() {
        let mut config = empty_config();
        add_profile(&mut config, ProfileAddArgs {
            name: "p3".to_string(),
            scope: Scope::Window { process_name: "firefox".to_string(), window_title_hint: "Mozilla Firefox".to_string(), backend_hint_id: Some("h1".to_string()) },
            action: ProfileActionArgs::Repeat { input: "key:A".to_string(), interval_ms: 50, jitter_ms: None },
            debounce_ms: Some(250),
        }).unwrap();
        let listing = format_profile_list(&config);
        assert_eq!(listing, "p3\tWindow(firefox)\tRepeat every 50ms\u{00b1}no jitter\tdebounce=250ms\n");
    }

    #[test]
    fn format_profile_list_macro_matches_full_expected_line() {
        let mut config = empty_config();
        add_profile(&mut config, ProfileAddArgs {
            name: "p4".to_string(),
            scope: Scope::Desktop,
            action: ProfileActionArgs::Macro { steps: vec!["scroll:0,1:20".to_string(), "key:Ctrl+C:50".to_string()], loop_: true },
            debounce_ms: None,
        }).unwrap();
        let listing = format_profile_list(&config);
        assert_eq!(listing, "p4\tDesktop\tMacro (2 steps, looping)\tdebounce=400ms\n");
    }

    #[test]
    fn format_profile_list_macro_once_matches_full_expected_line() {
        let mut config = empty_config();
        add_profile(&mut config, ProfileAddArgs {
            name: "p5".to_string(),
            scope: Scope::Desktop,
            action: ProfileActionArgs::Macro { steps: vec!["scroll:0,1:20".to_string()], loop_: false },
            debounce_ms: None,
        }).unwrap();
        let listing = format_profile_list(&config);
        assert_eq!(listing, "p5\tDesktop\tMacro (1 steps, once)\tdebounce=400ms\n");
    }
}
