//! Limit of the memory allocator's arenas at start-up (CRD ch.45 §45.6A, Reloaded-original).
//!
//! On Linux with glibc every thread that allocates can get an allocation area of its own, an arena. Under heavy
//! order updating the memory the process holds then grows although the data it holds does not. Limiting the number of
//! arenas, once, before the first worker thread starts, removes that growth.
//!
//! The decision (`plan_arena_limit`) is a pure function of the environment variable and the configuration; the one
//! allocator call lives in `apply_at_startup`, which is a no-op everywhere but Linux with glibc (R45.6A.1).

use common::log::log_crate::{Level, Record};
use common::log::{chunk2log, format_record, LogLevel};
use serde_json::Value as Json;

/// The limit used when neither the environment nor the configuration says otherwise (R45.6A.3 item 3).
const DEFAULT_ARENA_LIMIT: i32 = 2;
/// The environment variable the allocator itself reads; when it is set the operator has chosen (R45.6A.3 item 1).
const ARENA_ENV: &str = "MALLOC_ARENA_MAX";
/// The configuration key (R45.6A.3 item 2).
const ARENA_KEY: &str = "malloc_arena_max";

/// What the daemon asks of the allocator.
#[derive(Debug, PartialEq)]
pub(crate) enum ArenaDecision {
    /// Ask the allocator for at most this many arenas.
    Call(i32),
    /// No call: the environment variable is set and the allocator reads it itself.
    LeftToEnvironment,
    /// No call: the configuration key is `0`, the allocator keeps its own default.
    NoLimit,
}

/// The decision together with the lines to log about it.
#[derive(Debug, PartialEq)]
pub(crate) struct ArenaPlan {
    pub(crate) decision: ArenaDecision,
    /// Set when the configuration key is invalid (R45.6A.4).
    pub(crate) warning: Option<String>,
    /// The one information line of R45.6A.5.
    pub(crate) info: String,
}

/// Decides what to do from the value of `MALLOC_ARENA_MAX` (if set) and the configuration (R45.6A.3, R45.6A.4).
pub(crate) fn plan_arena_limit(env_value: Option<&str>, conf: &Json) -> ArenaPlan {
    if let Some(value) = env_value.filter(|v| !v.is_empty()) {
        return ArenaPlan {
            decision: ArenaDecision::LeftToEnvironment,
            warning: None,
            info: format!("Memory allocator arena limit is left to the environment: {ARENA_ENV}={value}."),
        };
    }

    let default_plan = |warning: Option<String>, reason: &str| ArenaPlan {
        decision: ArenaDecision::Call(DEFAULT_ARENA_LIMIT),
        warning,
        info: format!("Memory allocator arena limit is {DEFAULT_ARENA_LIMIT}, set from the default{reason}."),
    };

    match &conf[ARENA_KEY] {
        Json::Null => default_plan(None, ""),
        value => match value.as_u64().filter(|n| *n <= i32::MAX as u64) {
            Some(0) => ArenaPlan {
                decision: ArenaDecision::NoLimit,
                warning: None,
                info: format!("Memory allocator arena limit is not set, because {ARENA_KEY} is 0."),
            },
            Some(n) => ArenaPlan {
                decision: ArenaDecision::Call(n as i32),
                warning: None,
                info: format!("Memory allocator arena limit is {n}, set from the configuration ({ARENA_KEY})."),
            },
            None => default_plan(
                Some(format!(
                    "Configuration key {ARENA_KEY} must be a whole number from 0 to {}, got {value}. \
                     Using the default arena limit {DEFAULT_ARENA_LIMIT}.",
                    i32::MAX
                )),
                &format!(" because {ARENA_KEY} is invalid"),
            ),
        },
    }
}

/// Applies the limit of R45.6A once, for the native daemon. Call it before any worker thread starts.
///
/// The application logger is not built yet at this point (it is built together with the context inside `lp_main`),
/// so the lines go through `chunk2log`, which is the sink that logger itself writes to (stdout and the `MM_LOG`
/// file) and the one the version banner uses. They respect the same level filter.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
pub(crate) fn apply_at_startup(conf: &Json, filter: LogLevel) {
    let env_value = std::env::var_os(ARENA_ENV).map(|v| v.to_string_lossy().into_owned());
    let plan = plan_arena_limit(env_value.as_deref(), conf);

    if let Some(warning) = &plan.warning {
        emit(filter, Level::Warn, warning);
    }
    emit(filter, Level::Info, &plan.info);
    if let ArenaDecision::Call(limit) = plan.decision {
        if !set_arena_max(limit) {
            let warning = format!(
                "The memory allocator refused the arena limit {limit}. Going on with the allocator's own default."
            );
            emit(filter, Level::Warn, &warning);
        }
    }
}

/// Other platforms and C libraries: no limit and no line (R45.6A.1).
#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
pub(crate) fn apply_at_startup(_conf: &Json, _filter: LogLevel) {}

/// Returns `false` if the allocator refused the limit.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn set_arena_max(limit: i32) -> bool {
    // SAFETY: `mallopt` takes two plain integers and has no pointer arguments. It is made to be callable at any time;
    // that it is made before the worker threads start is what makes the limit effective, not what makes the call safe.
    unsafe { libc::mallopt(libc::M_ARENA_MAX, limit) == 1 }
}

fn emit(filter: LogLevel, level: Level, message: &str) {
    if filter < LogLevel::from(level) {
        return;
    }
    let args = format_args!("{message}");
    let record = Record::builder()
        .args(args)
        .level(level)
        .module_path(Some(module_path!()))
        .file(Some(file!()))
        .line(Some(line!()))
        .build();
    chunk2log(format_record(&record), LogLevel::from(level));
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn plan_for(env_value: Option<&str>, conf: Json) -> ArenaPlan { plan_arena_limit(env_value, &conf) }

    #[test]
    fn nothing_set_uses_the_default() {
        for conf in [json!({}), json!({ "malloc_arena_max": null })] {
            let plan = plan_for(None, conf);
            assert_eq!(plan.decision, ArenaDecision::Call(2));
            assert_eq!(plan.warning, None);
            assert!(
                plan.info.contains("2") && plan.info.contains("from the default"),
                "{}",
                plan.info
            );
        }
    }

    #[test]
    fn the_key_selects_the_limit() {
        let plan = plan_for(None, json!({ "malloc_arena_max": 4 }));
        assert_eq!(plan.decision, ArenaDecision::Call(4));
        assert_eq!(plan.warning, None);
        assert!(
            plan.info.contains("4") && plan.info.contains("from the configuration"),
            "{}",
            plan.info
        );

        let plan = plan_for(None, json!({ "malloc_arena_max": 2147483647u32 }));
        assert_eq!(plan.decision, ArenaDecision::Call(i32::MAX));
    }

    #[test]
    fn key_zero_makes_no_call() {
        let plan = plan_for(None, json!({ "malloc_arena_max": 0 }));
        assert_eq!(plan.decision, ArenaDecision::NoLimit);
        assert_eq!(plan.warning, None);
        assert!(
            plan.info.contains("not set") && plan.info.contains("is 0"),
            "{}",
            plan.info
        );
    }

    #[test]
    fn the_environment_wins_over_the_key() {
        let plan = plan_for(Some("3"), json!({ "malloc_arena_max": 4 }));
        assert_eq!(plan.decision, ArenaDecision::LeftToEnvironment);
        assert_eq!(plan.warning, None);
        assert!(plan.info.contains("left to the environment"), "{}", plan.info);
        assert!(plan.info.contains("MALLOC_ARENA_MAX=3"), "{}", plan.info);

        // The value is reported as given, even one the allocator will not accept, and an invalid key is ignored.
        let plan = plan_for(Some("abc"), json!({ "malloc_arena_max": "x" }));
        assert_eq!(plan.decision, ArenaDecision::LeftToEnvironment);
        assert_eq!(plan.warning, None);
        assert!(plan.info.contains("MALLOC_ARENA_MAX=abc"), "{}", plan.info);
    }

    #[test]
    fn an_empty_environment_value_counts_as_unset() {
        let plan = plan_for(Some(""), json!({ "malloc_arena_max": 4 }));
        assert_eq!(plan.decision, ArenaDecision::Call(4));
        assert!(plan.info.contains("from the configuration"), "{}", plan.info);
    }

    #[test]
    fn an_invalid_key_warns_and_uses_the_default() {
        let invalid = [
            json!("2"),
            json!(-1),
            json!(1.5),
            json!(2.0),
            json!(true),
            json!(2147483648u64),
            json!([]),
            json!({}),
        ];
        for value in invalid {
            let plan = plan_for(None, json!({ "malloc_arena_max": value }));
            assert_eq!(plan.decision, ArenaDecision::Call(2), "{value}");
            let warning = plan.warning.unwrap_or_else(|| panic!("no warning for {value}"));
            assert!(warning.contains("malloc_arena_max"), "{warning}");
            assert!(warning.contains("default arena limit 2"), "{warning}");
            assert!(
                plan.info.contains("2") && plan.info.contains("default"),
                "{}",
                plan.info
            );
        }
    }

    /// T45.11: the call is made with a limit that does not lower the arena limit of the test process.
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    #[test]
    fn the_allocator_accepts_a_high_limit() {
        // The limit is process-wide and there is no way to read it back, so use a value far above the number of
        // arenas any test process creates, and never a lower one than the allocator's own default.
        assert!(set_arena_max(1024));
    }
}
