//! Declarative QA recipes: a thin, serde-typed composition over the existing
//! bridge vocabulary (`protocol.rs`). A recipe is a named list of [`Step`]s; a
//! runner (the `iced-agent run` CLI, or an in-process interpreter) resolves
//! each step to one or more bridge commands. No new wire concepts — `Role`,
//! `Intent` and `Cond` are the protocol types, matched here by their serde
//! shapes so a recipe reads exactly like the commands it expands to.

use serde::{Deserialize, Serialize};

use crate::protocol::{Cond, Intent, Role};

/// A named scenario. `preset` names the boot state the recipe assumes; runners
/// that support presets fail loudly when an app doesn't define it. `lane`
/// self-declares which QA lane can run it (defaults to [`Lane::Both`]).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Recipe {
    pub name: String,
    pub preset: Option<String>,
    #[serde(default)]
    pub lane: Lane,
    pub steps: Vec<Step>,
}

/// Which QA lane a recipe can run in.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Lane {
    /// Pure UI logic provable by `update()` + `view()` in-process — runs in
    /// both the in-process lane and the fleet lane.
    #[default]
    Both,
    /// Needs a living process (subscription-driven global shortcuts,
    /// multi-window, real node/CEF effects, screenshots, a11y) — fleet only.
    Fleet,
}

/// One recipe step, externally tagged (serde default) and snake_case — so the
/// wire form is `{ "click": { ... } }`, `{ "type": "hello" }`, etc.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Step {
    /// Semantic click: `find{role,name}` → first match's `@ref` → `click`.
    Click { role: Role, name: String },
    /// Type a string into the focused widget.
    Type(String),
    /// Press a named key (or single-char chord) with optional modifiers.
    Press {
        key: String,
        #[serde(default)]
        mods: Vec<String>,
    },
    /// Inject a curated intent.
    Intent(Intent),
    /// Assert a condition now (one-shot).
    Expect(Cond),
    /// Poll a condition until it holds or the timeout elapses.
    Wait {
        cond: Cond,
        timeout_ms: Option<u64>,
    },
}

impl Recipe {
    /// Parse a recipe from JSON, surfacing serde's line/column in the error.
    pub fn parse(s: &str) -> Result<Recipe, String> {
        serde_json::from_str(s).map_err(|e| format!("recipe parse error: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact example from the spec's "Recipe format" section.
    const SPEC_EXAMPLE: &str = r#"{
  "name": "nav-smoke",
  "preset": "ui-demo",
  "steps": [
    { "click":  { "role": "button", "name": "Chat" } },
    { "type":   "hello" },
    { "press":  { "key": "k", "mods": ["ctrl"] } },
    { "intent": { "section": { "name": "operator" } } },
    { "expect": { "state_path": { "path": "screen", "equals": "chat" } } },
    { "expect": { "node": { "role": "tab", "name": "User", "exists": true } } },
    { "wait":   { "cond": { "node": { "role": "button", "name": "Save", "exists": true } }, "timeout_ms": 5000 } }
  ]
}"#;

    #[test]
    fn parses_spec_example() {
        let r = Recipe::parse(SPEC_EXAMPLE).expect("spec example must parse");
        assert_eq!(r.name, "nav-smoke");
        assert_eq!(r.preset.as_deref(), Some("ui-demo"));
        assert_eq!(r.lane, Lane::Both, "absent lane defaults to Both");
        assert_eq!(r.steps.len(), 7);
        assert!(matches!(&r.steps[0], Step::Click { role: Role::Button, name } if name == "Chat"));
        assert!(matches!(&r.steps[1], Step::Type(s) if s == "hello"));
        assert!(matches!(&r.steps[2], Step::Press { key, mods } if key == "k" && mods == &["ctrl"]));
        assert!(matches!(&r.steps[3], Step::Intent(Intent::Section { name }) if name == "operator"));
        assert!(matches!(&r.steps[4], Step::Expect(Cond::StatePath { .. })));
        assert!(matches!(&r.steps[5], Step::Expect(Cond::Node { exists: true, .. })));
        assert!(matches!(&r.steps[6], Step::Wait { timeout_ms: Some(5000), .. }));
    }

    #[test]
    fn spec_example_round_trips() {
        // Value-level round-trip: parse → serialize → re-parse yields the same
        // Recipe, and re-serializing matches the first serialization.
        let r = Recipe::parse(SPEC_EXAMPLE).unwrap();
        let json = serde_json::to_string(&r).unwrap();
        let back = Recipe::parse(&json).unwrap();
        assert_eq!(r, back);
    }

    #[test]
    fn step_wire_shapes() {
        // External tagging + snake_case: each variant is a single-key object.
        let click = serde_json::to_string(&Step::Click {
            role: Role::Button,
            name: "Chat".into(),
        })
        .unwrap();
        assert_eq!(click, r#"{"click":{"role":"button","name":"Chat"}}"#);

        assert_eq!(
            serde_json::to_string(&Step::Type("hi".into())).unwrap(),
            r#"{"type":"hi"}"#
        );

        let press = serde_json::to_string(&Step::Press {
            key: "k".into(),
            mods: vec!["ctrl".into()],
        })
        .unwrap();
        assert_eq!(press, r#"{"press":{"key":"k","mods":["ctrl"]}}"#);
    }

    #[test]
    fn press_mods_default_when_absent() {
        let step: Step = serde_json::from_str(r#"{"press":{"key":"enter"}}"#).unwrap();
        assert!(matches!(step, Step::Press { mods, .. } if mods.is_empty()));
    }

    #[test]
    fn lane_explicit_fleet_and_default_both() {
        // Explicit "fleet" parses and round-trips.
        let fleet = Recipe::parse(
            r#"{"name":"popout","lane":"fleet","steps":[{"type":"hi"}]}"#,
        )
        .unwrap();
        assert_eq!(fleet.lane, Lane::Fleet);
        assert_eq!(Recipe::parse(&serde_json::to_string(&fleet).unwrap()).unwrap(), fleet);

        // Absent lane defaults to Both.
        let dflt = Recipe::parse(r#"{"name":"x","steps":[{"type":"hi"}]}"#).unwrap();
        assert_eq!(dflt.lane, Lane::Both);
    }

    #[test]
    fn missing_preset_is_none() {
        let r =
            Recipe::parse(r#"{"name":"x","steps":[{"type":"hi"}]}"#).expect("preset is optional");
        assert!(r.preset.is_none());
    }

    #[test]
    fn parse_error_is_readable() {
        let err = Recipe::parse("{ not json").unwrap_err();
        assert!(err.starts_with("recipe parse error:"), "got: {err}");
    }
}
