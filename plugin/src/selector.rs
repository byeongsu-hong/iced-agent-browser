//! Semantic-role selectors for `iced_test`: Rust tests address widgets by their
//! `sem()` role + name — the same handle the agent uses over the wire — instead
//! of brittle visible-text matching.
//!
//! The [`Sem`](crate::sem::Sem) wrapper brackets its content with a
//! `SemProbe::Enter` custom operation carrying the node's role/name.
//! `iced_selector`'s `Find`/`FindAll` forward that as
//! [`Candidate::Custom`](iced_selector::Candidate::Custom) with the probe as its
//! `&dyn Any` state. We downcast to [`SemProbe`], match `Enter` (ignoring the
//! balanced `Exit`, which never downcasts to an `Enter`), and return a
//! [`Target::Custom`] carrying the node's bounds — all `Simulator::click` needs,
//! since `Target: Bounded`.

use iced_selector::{Candidate, Selector, Target};

use crate::protocol::Role;
use crate::sem::SemProbe;

/// Matches a `sem`-tagged node by role, optionally by (case-insensitive, exact)
/// name. `name: None` matches the first node of that role.
struct ByRole {
    role: Role,
    name: Option<String>,
}

impl Selector for ByRole {
    type Output = Target;

    fn select(&mut self, candidate: Candidate<'_>) -> Option<Target> {
        let Candidate::Custom {
            bounds,
            visible_bounds,
            state,
            ..
        } = candidate
        else {
            return None;
        };
        // Non-`SemProbe` customs and the `Exit` marker both fail this and are skipped.
        let SemProbe::Enter { role, name, .. } = state.downcast_ref::<SemProbe>()? else {
            return None;
        };
        if *role != self.role {
            return None;
        }
        if let Some(want) = &self.name
            && !name.eq_ignore_ascii_case(want)
        {
            return None;
        }
        Some(Target::Custom {
            id: None,
            bounds,
            visible_bounds,
        })
    }

    fn description(&self) -> String {
        match &self.name {
            Some(name) => format!("sem {:?} named {name:?}", self.role),
            None => format!("first sem {:?}", self.role),
        }
    }
}

/// Selectors addressing `sem()`-tagged widgets by semantic role.
pub mod by {
    use super::{ByRole, Role, Selector, Target};

    /// Selects the `sem`-tagged node of `role` whose name equals `name`
    /// case-insensitively (exact, not substring).
    pub fn role(role: Role, name: impl Into<String>) -> impl Selector<Output = Target> {
        ByRole {
            role,
            name: Some(name.into()),
        }
    }

    /// Selects the first `sem`-tagged node of `role`, regardless of name.
    pub fn any(role: Role) -> impl Selector<Output = Target> {
        ByRole { role, name: None }
    }
}

#[cfg(test)]
mod tests {
    use super::by;
    use crate::protocol::Role;
    use crate::sem::sem;
    use iced::widget::{Column, button};

    #[derive(Debug, Clone, PartialEq)]
    enum Msg {
        Went,
        Stopped,
    }

    fn view() -> iced::Element<'static, Msg> {
        Column::new()
            .push(sem(Role::Button, "Go", button("Go").on_press(Msg::Went)))
            .push(sem(Role::Button, "Stop", button("Stop").on_press(Msg::Stopped)))
            .into()
    }

    #[test]
    fn click_by_role_and_name_hits_that_button() {
        let mut ui = iced_test::simulator(view());
        // Lowercase on purpose: name match is case-insensitive.
        ui.click(by::role(Role::Button, "go")).expect("Go button");
        let msgs: Vec<Msg> = ui.into_messages().collect();
        assert_eq!(msgs, vec![Msg::Went]);
    }

    #[test]
    fn missing_name_errors() {
        let mut ui = iced_test::simulator(view());
        assert!(ui.click(by::role(Role::Button, "Missing")).is_err());
    }

    #[test]
    fn any_matches_first_of_role() {
        let mut ui = iced_test::simulator(view());
        ui.click(by::any(Role::Button)).expect("first button");
        let msgs: Vec<Msg> = ui.into_messages().collect();
        assert_eq!(msgs, vec![Msg::Went]);
    }
}
