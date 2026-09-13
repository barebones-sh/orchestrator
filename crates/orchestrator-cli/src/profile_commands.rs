//! Profile CRUD logic operating on an in-memory `Config`. No I/O beyond what
//! `Config::load`/`save` already do, and no live D-Bus/window-listing here —
//! see design spec §5 for why this layer stays synchronous and pure.
//! docs/superpowers/specs/2026-09-12-cli-ux-design.md

use orchestrator_core::action::{Action, Jitter};
use orchestrator_core::profile::Scope;
use orchestrator_core::profile_ops::{self, ProfileOpError};
use orchestrator_core::Config;
use orchestrator_window::WindowInfo;

use crate::notation::{parse_macro_step, parse_repeat_input, NotationError};

#[derive(Debug, PartialEq, Eq)]
pub enum ProfileCommandError {
    Notation(String),
    Op(ProfileOpError),
    MissingRequiredField(&'static str),
    RepeatAndMacroBothOrNeitherSpecified,
}

impl std::fmt::Display for ProfileCommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Notation(msg) => write!(f, "{msg}"),
            Self::Op(e) => write!(f, "{e}"),
            Self::MissingRequiredField(field) => write!(f, "--{field} is required"),
            Self::RepeatAndMacroBothOrNeitherSpecified => write!(
                f,
                "--action repeat's fields (--input/--interval-ms) and --action macro's \
                 fields (--step/--loop) cannot both be given at once -- only supply the \
                 fields for the --action you chose"
            ),
        }
    }
}

impl std::error::Error for ProfileCommandError {}

impl From<NotationError> for ProfileCommandError {
    fn from(e: NotationError) -> Self {
        Self::Notation(e.to_string())
    }
}

impl From<ProfileOpError> for ProfileCommandError {
    fn from(e: ProfileOpError) -> Self {
        Self::Op(e)
    }
}

#[derive(Debug, Clone)]
pub enum ProfileActionArgs {
    Repeat {
        input: String,
        interval_ms: u64,
        jitter_ms: Option<u64>,
    },
    Macro {
        steps: Vec<String>,
        loop_: bool,
    },
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

fn build_action(args: &ProfileActionArgs) -> Result<Action, ProfileCommandError> {
    match args {
        ProfileActionArgs::Repeat {
            input,
            interval_ms,
            jitter_ms,
        } => {
            let event = parse_repeat_input(input)?;
            let jitter = match jitter_ms {
                Some(range_ms) => Jitter::Uniform {
                    range_ms: *range_ms,
                },
                None => Jitter::None,
            };
            Ok(Action::Repeat {
                input: event,
                interval_ms: *interval_ms,
                jitter,
            })
        }
        ProfileActionArgs::Macro { steps, loop_ } => {
            let parsed_steps = steps
                .iter()
                .map(|s| parse_macro_step(s))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Action::Macro {
                steps: parsed_steps,
                loop_: *loop_,
            })
        }
    }
}

// Only currently called from `main.rs`'s Linux-only window picker
// (`linux_window_picker::pick_window`), even though this function's logic
// itself is portable/OS-independent by design (Task 2 kept it free of any
// live D-Bus dependency specifically so it stays unit-testable on any
// platform -- see the module doc comment / design spec §5). Gating the
// function itself behind `#[cfg(target_os = "linux")]` would misrepresent
// that and stop these unit tests from running/meaning anything on a
// non-Linux target, so this is `allow(dead_code)` on non-Linux instead of a
// `cfg`.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn resolve_window_scope(
    windows: &[WindowInfo],
    chosen_index: usize,
) -> Result<Scope, ProfileCommandError> {
    profile_ops::resolve_window_scope(windows, chosen_index).map_err(Into::into)
}

pub fn add_profile(config: &mut Config, args: ProfileAddArgs) -> Result<(), ProfileCommandError> {
    let action = build_action(&args.action)?;
    profile_ops::add_profile(
        config,
        profile_ops::AddProfileArgs {
            name: args.name,
            scope: args.scope,
            action,
            debounce_ms: args.debounce_ms,
        },
    )
    .map_err(Into::into)
}

pub fn edit_profile(
    config: &mut Config,
    name: &str,
    args: ProfileEditArgs,
) -> Result<(), ProfileCommandError> {
    // Resolve the fallible notation-parsing step BEFORE calling into
    // profile_ops -- profile_ops::edit_profile itself has no fallible step
    // left (see its doc comment), so THIS is where the original atomicity
    // guarantee ("a rejected edit is a true no-op") now lives.
    let resolved_action = match &args.action {
        Some(action_args) => Some(build_action(action_args)?),
        None => None,
    };
    profile_ops::edit_profile(
        config,
        name,
        profile_ops::EditProfileArgs {
            scope: args.scope,
            action: resolved_action,
            debounce_ms: args.debounce_ms,
        },
    )
    .map_err(Into::into)
}

pub fn remove_profile(config: &mut Config, name: &str) -> Result<(), ProfileCommandError> {
    profile_ops::remove_profile(config, name).map_err(Into::into)
}

pub fn format_profile_list(config: &Config) -> String {
    let mut out = String::new();
    for profile in &config.profiles {
        let scope_summary = match &profile.scope {
            Scope::Desktop => "Desktop".to_string(),
            Scope::Window { process_name, .. } => format!("Window({process_name})"),
        };
        let action_summary = match &profile.action {
            Action::Repeat {
                interval_ms,
                jitter,
                ..
            } => match jitter {
                Jitter::None => format!("Repeat every {interval_ms}ms\u{00b1}no jitter"),
                Jitter::Uniform { range_ms } => {
                    format!("Repeat every {interval_ms}ms\u{00b1}{range_ms}ms")
                }
            },
            Action::Macro { steps, loop_ } => {
                format!(
                    "Macro ({} steps, {})",
                    steps.len(),
                    if *loop_ { "looping" } else { "once" }
                )
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

    fn empty_config() -> Config {
        Config {
            schema_version: orchestrator_core::config::CURRENT_SCHEMA_VERSION,
            profiles: vec![],
        }
    }

    #[test]
    fn add_profile_rejects_click_in_repeat_mode() {
        let mut config = empty_config();
        let args = ProfileAddArgs {
            name: "bad".to_string(),
            scope: Scope::Desktop,
            action: ProfileActionArgs::Repeat {
                input: "click:left:cursor".to_string(),
                interval_ms: 100,
                jitter_ms: None,
            },
            debounce_ms: None,
        };
        let err = add_profile(&mut config, args).unwrap_err();
        assert!(matches!(err, ProfileCommandError::Notation(_)));
    }

    // -- wrapper error-conversion: `impl From<ProfileOpError> for
    // ProfileCommandError` and the `.map_err(Into::into)` call sites are
    // only exercised by calling the CLI's own public wrapper functions
    // (not `profile_ops` directly) -- the underlying `ProfileOpError`
    // logic itself is already fully covered by `profile_ops.rs`'s own
    // test module, so these are deliberately thin.

    #[test]
    fn add_profile_wraps_duplicate_name_as_op_error() {
        let mut config = empty_config();
        let args = |name: &str| ProfileAddArgs {
            name: name.to_string(),
            scope: Scope::Desktop,
            action: ProfileActionArgs::Repeat {
                input: "key:A".to_string(),
                interval_ms: 100,
                jitter_ms: None,
            },
            debounce_ms: None,
        };
        add_profile(&mut config, args("dup")).unwrap();
        let err = add_profile(&mut config, args("dup")).unwrap_err();
        assert_eq!(
            err,
            ProfileCommandError::Op(ProfileOpError::DuplicateProfileName("dup".to_string()))
        );
    }

    #[test]
    fn edit_profile_wraps_unknown_name_as_op_error() {
        let mut config = empty_config();
        let err = edit_profile(
            &mut config,
            "nope",
            ProfileEditArgs {
                scope: None,
                action: None,
                debounce_ms: None,
            },
        )
        .unwrap_err();
        assert_eq!(
            err,
            ProfileCommandError::Op(ProfileOpError::NoSuchProfile("nope".to_string()))
        );
    }

    #[test]
    fn remove_profile_wraps_unknown_name_as_op_error() {
        let mut config = empty_config();
        let err = remove_profile(&mut config, "nope").unwrap_err();
        assert_eq!(
            err,
            ProfileCommandError::Op(ProfileOpError::NoSuchProfile("nope".to_string()))
        );
    }

    #[test]
    fn resolve_window_scope_wraps_empty_window_list_as_op_error() {
        let err = resolve_window_scope(&[], 0).unwrap_err();
        assert_eq!(
            err,
            ProfileCommandError::Op(ProfileOpError::NoWindowsAvailable)
        );
    }

    // -- final-review Fix 1(a): add_profile/edit_profile don't validate ----
    // ------------------------------------------------------------------------
    // `add_profile`/`edit_profile` are deliberately pure and don't duplicate
    // `Config::validate`'s checks (see design spec §5 and this module's doc
    // comment) -- that's the `main.rs` glue layer's job, calling
    // `Config::validate()` after a successful `add_profile`/`edit_profile`
    // and before `config.save`. These tests demonstrate why that glue-layer
    // call is load-bearing: without it, invalid data sails straight through.

    #[test]
    fn add_profile_alone_does_not_reject_zero_interval() {
        let mut config = empty_config();
        let args = ProfileAddArgs {
            name: "zero-interval".to_string(),
            scope: Scope::Desktop,
            action: ProfileActionArgs::Repeat {
                input: "key:A".to_string(),
                interval_ms: 0,
                jitter_ms: None,
            },
            debounce_ms: None,
        };
        // add_profile succeeds even though interval_ms: 0 is invalid --
        // catching this is Config::validate's job, which a caller MUST run
        // afterward (see main.rs's Add/Edit handlers).
        assert!(add_profile(&mut config, args).is_ok());
        assert_eq!(config.profiles[0].debounce_ms, 400);

        let err = config.validate().unwrap_err();
        assert!(matches!(
            err,
            orchestrator_core::error::ConfigError::InvalidInterval { .. }
        ));
    }

    #[test]
    fn add_profile_alone_does_not_reject_jitter_exceeding_interval() {
        let mut config = empty_config();
        let args = ProfileAddArgs {
            name: "bad-jitter".to_string(),
            scope: Scope::Desktop,
            action: ProfileActionArgs::Repeat {
                input: "key:A".to_string(),
                interval_ms: 100,
                jitter_ms: Some(999),
            },
            debounce_ms: None,
        };
        assert!(add_profile(&mut config, args).is_ok());

        let err = config.validate().unwrap_err();
        assert!(matches!(
            err,
            orchestrator_core::error::ConfigError::JitterRangeExceedsInterval { .. }
        ));
    }

    #[test]
    fn edit_profile_alone_does_not_reject_zero_interval() {
        let mut config = empty_config();
        add_profile(
            &mut config,
            ProfileAddArgs {
                name: "target".to_string(),
                scope: Scope::Desktop,
                action: ProfileActionArgs::Repeat {
                    input: "key:A".to_string(),
                    interval_ms: 100,
                    jitter_ms: None,
                },
                debounce_ms: None,
            },
        )
        .unwrap();
        assert!(config.validate().is_ok());

        let result = edit_profile(
            &mut config,
            "target",
            ProfileEditArgs {
                scope: None,
                action: Some(ProfileActionArgs::Repeat {
                    input: "key:A".to_string(),
                    interval_ms: 0,
                    jitter_ms: None,
                }),
                debounce_ms: None,
            },
        );
        // edit_profile itself accepts this -- Config::validate is what must
        // catch it, and a caller (main.rs) must call it before saving.
        assert!(result.is_ok());
        let err = config.validate().unwrap_err();
        assert!(matches!(
            err,
            orchestrator_core::error::ConfigError::InvalidInterval { .. }
        ));
    }

    // -- edit_profile ---------------------------------------------------------

    #[test]
    fn edit_profile_is_atomic_when_action_notation_fails() {
        let mut config = empty_config();
        add_profile(
            &mut config,
            ProfileAddArgs {
                name: "target".to_string(),
                scope: Scope::Desktop,
                action: ProfileActionArgs::Repeat {
                    input: "key:A".to_string(),
                    interval_ms: 100,
                    jitter_ms: None,
                },
                debounce_ms: Some(400),
            },
        )
        .unwrap();

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
                action: Some(ProfileActionArgs::Repeat {
                    input: "click:left:cursor".to_string(),
                    interval_ms: 100,
                    jitter_ms: None,
                }),
                debounce_ms: None,
            },
        )
        .unwrap_err();

        assert!(matches!(err, ProfileCommandError::Notation(_)));
        let profile = &config.profiles[0];
        assert_eq!(
            profile.scope,
            Scope::Desktop,
            "scope must be unchanged when the edit as a whole fails"
        );
        assert!(
            !profile.focus_steal,
            "focus_steal must be unchanged when the edit as a whole fails"
        );
        assert_eq!(
            profile.action, original_action,
            "action must be unchanged when the edit as a whole fails"
        );
    }

    // -- format_profile_list -----------------------------------------------------

    #[test]
    fn format_profile_list_repeat_with_jitter_matches_full_expected_line() {
        let mut config = empty_config();
        add_profile(
            &mut config,
            ProfileAddArgs {
                name: "p1".to_string(),
                scope: Scope::Desktop,
                action: ProfileActionArgs::Repeat {
                    input: "key:A".to_string(),
                    interval_ms: 100,
                    jitter_ms: Some(20),
                },
                debounce_ms: None,
            },
        )
        .unwrap();
        let listing = format_profile_list(&config);
        assert_eq!(
            listing,
            "p1\tDesktop\tRepeat every 100ms\u{00b1}20ms\tdebounce=400ms\n"
        );
    }

    #[test]
    fn format_profile_list_repeat_without_jitter_shows_no_jitter_suffix() {
        // Design spec §3.5: the action summary is always `Repeat every
        // <interval_ms>ms±<jitter or "no jitter">` -- even with no jitter,
        // the `±` and a placeholder must appear, not be dropped entirely.
        let mut config = empty_config();
        add_profile(
            &mut config,
            ProfileAddArgs {
                name: "p2".to_string(),
                scope: Scope::Desktop,
                action: ProfileActionArgs::Repeat {
                    input: "key:A".to_string(),
                    interval_ms: 100,
                    jitter_ms: None,
                },
                debounce_ms: None,
            },
        )
        .unwrap();
        let listing = format_profile_list(&config);
        assert_eq!(
            listing,
            "p2\tDesktop\tRepeat every 100ms\u{00b1}no jitter\tdebounce=400ms\n"
        );
    }

    #[test]
    fn format_profile_list_window_scope_matches_full_expected_line() {
        let mut config = empty_config();
        add_profile(
            &mut config,
            ProfileAddArgs {
                name: "p3".to_string(),
                scope: Scope::Window {
                    process_name: "firefox".to_string(),
                    window_title_hint: "Mozilla Firefox".to_string(),
                    backend_hint_id: Some("h1".to_string()),
                },
                action: ProfileActionArgs::Repeat {
                    input: "key:A".to_string(),
                    interval_ms: 50,
                    jitter_ms: None,
                },
                debounce_ms: Some(250),
            },
        )
        .unwrap();
        let listing = format_profile_list(&config);
        assert_eq!(
            listing,
            "p3\tWindow(firefox)\tRepeat every 50ms\u{00b1}no jitter\tdebounce=250ms\n"
        );
    }

    #[test]
    fn format_profile_list_macro_matches_full_expected_line() {
        let mut config = empty_config();
        add_profile(
            &mut config,
            ProfileAddArgs {
                name: "p4".to_string(),
                scope: Scope::Desktop,
                action: ProfileActionArgs::Macro {
                    steps: vec!["scroll:0,1:20".to_string(), "key:Ctrl+C:50".to_string()],
                    loop_: true,
                },
                debounce_ms: None,
            },
        )
        .unwrap();
        let listing = format_profile_list(&config);
        assert_eq!(
            listing,
            "p4\tDesktop\tMacro (2 steps, looping)\tdebounce=400ms\n"
        );
    }

    #[test]
    fn format_profile_list_macro_once_matches_full_expected_line() {
        let mut config = empty_config();
        add_profile(
            &mut config,
            ProfileAddArgs {
                name: "p5".to_string(),
                scope: Scope::Desktop,
                action: ProfileActionArgs::Macro {
                    steps: vec!["scroll:0,1:20".to_string()],
                    loop_: false,
                },
                debounce_ms: None,
            },
        )
        .unwrap();
        let listing = format_profile_list(&config);
        assert_eq!(
            listing,
            "p5\tDesktop\tMacro (1 steps, once)\tdebounce=400ms\n"
        );
    }
}
