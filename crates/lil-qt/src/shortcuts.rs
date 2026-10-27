//! The suite's one shortcut map (APP-011), scoped by the focused panel (APP-143).
//!
//! Plan 4a has only global keys: one per panel of the Default layout, Ctrl+Shift+P to pause or
//! resume GPU work, and F6 / Shift+F6 to move between panels. The browser and Review scope
//! (plan 4b) and the canvas scope (plan 5) are declared empty. The conflict check covers each
//! scope together with the global keys: a scope may reuse another scope's key, never a global
//! one.

use serde::Serialize;

/// Where a binding applies: everywhere, or while a panel of that kind has the focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    Global,
    BrowserReview,
    Canvas,
}

/// One key sequence (Qt's portable text, such as `Ctrl+Shift+P`) and the action it runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Binding {
    pub keys: String,
    pub action: String,
    pub scope: Scope,
}

impl Binding {
    pub fn new(keys: &str, action: &str, scope: Scope) -> Binding {
        Binding {
            keys: keys.to_string(),
            action: action.to_string(),
            scope,
        }
    }
}

/// The map: every binding of every scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShortcutMap {
    bindings: Vec<Binding>,
}

/// Plan 4a's global keys, in the order the shell lists them.
const PLAN_4A: [(&str, &str); 10] = [
    ("Ctrl+1", "panel.home"),
    ("Ctrl+2", "panel.jobs"),
    ("Ctrl+3", "panel.confirmations"),
    ("Ctrl+4", "panel.hardware"),
    ("Ctrl+5", "panel.usage"),
    ("Ctrl+6", "panel.diagnostics"),
    ("Ctrl+,", "panel.settings"),
    ("Ctrl+Shift+P", "gpu.toggle_pause"),
    ("F6", "panel.next"),
    ("Shift+F6", "panel.previous"),
];

impl ShortcutMap {
    /// Plan 4a's map: the global keys alone.
    pub fn plan_4a() -> ShortcutMap {
        ShortcutMap {
            bindings: PLAN_4A
                .iter()
                .map(|(keys, action)| Binding::new(keys, action, Scope::Global))
                .collect(),
        }
    }

    /// Every binding, in order.
    pub fn bindings(&self) -> &[Binding] {
        &self.bindings
    }

    /// Adds a binding (a later plan's scope keys).
    pub fn add(&mut self, binding: Binding) {
        self.bindings.push(binding);
    }

    /// Each pair of bindings whose keys are the same sequence (modifiers in any order, any
    /// case) in the same scope, or with either of them global.
    pub fn conflicts(&self) -> Vec<(Binding, Binding)> {
        let keys: Vec<String> = self.bindings.iter().map(|b| normalized(&b.keys)).collect();
        let mut out = Vec::new();
        for (i, a) in self.bindings.iter().enumerate() {
            for (j, b) in self.bindings.iter().enumerate().skip(i + 1) {
                let shared =
                    a.scope == b.scope || a.scope == Scope::Global || b.scope == Scope::Global;
                if shared && keys[i] == keys[j] {
                    out.push((a.clone(), b.clone()));
                }
            }
        }
        out
    }
}

/// `keys` with its modifiers sorted and everything upper-cased: `shift+ctrl+p` and
/// `Ctrl+Shift+P` are one sequence. The last `+`-separated part is the key (`Ctrl++` is Ctrl
/// and `+`).
fn normalized(keys: &str) -> String {
    let keys = keys.trim().to_ascii_uppercase();
    let (mods, key) = match keys.strip_suffix("++") {
        Some(mods) => (mods, "+"),
        None => match keys.rsplit_once('+') {
            Some((mods, key)) => (mods, key),
            None => ("", keys.as_str()),
        },
    };
    let mut mods: Vec<&str> = mods.split('+').filter(|m| !m.is_empty()).collect();
    mods.sort_unstable();
    mods.dedup();
    mods.push(key);
    mods.join("+")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test] // spec: APP-011, APP-143
    fn the_plan_4a_map_has_no_conflicts() {
        let map = ShortcutMap::plan_4a();
        assert_eq!(map.conflicts(), []);
        let keys: Vec<&str> = map.bindings().iter().map(|b| b.keys.as_str()).collect();
        assert_eq!(
            keys,
            [
                "Ctrl+1",
                "Ctrl+2",
                "Ctrl+3",
                "Ctrl+4",
                "Ctrl+5",
                "Ctrl+6",
                "Ctrl+,",
                "Ctrl+Shift+P",
                "F6",
                "Shift+F6"
            ]
        );
        assert!(map.bindings().iter().all(|b| b.scope == Scope::Global));
        let settings = map.bindings().iter().find(|b| b.keys == "Ctrl+,").unwrap();
        assert_eq!(settings.action, "panel.settings");
    }

    #[test] // spec: APP-143
    fn a_scope_key_equal_to_a_global_key_is_a_conflict() {
        let mut map = ShortcutMap::plan_4a();
        // Two panel scopes may share a key.
        map.add(Binding::new("P", "review.pick", Scope::BrowserReview));
        map.add(Binding::new("p", "canvas.pen", Scope::Canvas));
        assert_eq!(map.conflicts(), []);

        // A scope key that is a global key, spelt another way, is a conflict.
        let clash = Binding::new("shift+ctrl+p", "review.flag_pick", Scope::BrowserReview);
        map.add(clash.clone());
        let global = Binding::new("Ctrl+Shift+P", "gpu.toggle_pause", Scope::Global);
        assert_eq!(map.conflicts(), [(global, clash)]);

        // So is a key used twice in one scope.
        let mut map = ShortcutMap::plan_4a();
        let a = Binding::new("G", "review.grid", Scope::BrowserReview);
        let b = Binding::new("g", "review.loupe", Scope::BrowserReview);
        map.add(a.clone());
        map.add(b.clone());
        assert_eq!(map.conflicts(), [(a, b)]);
    }

    #[test] // spec: APP-011
    fn shortcut_keys_normalize_modifiers_and_case() {
        assert_eq!(normalized("Ctrl+,"), "CTRL+,");
        assert_eq!(normalized("shift+ctrl+P"), normalized("Ctrl+Shift+p"));
        assert_eq!(normalized("Ctrl++"), "CTRL++");
        assert_eq!(normalized("F6"), "F6");
        assert_ne!(normalized("Shift+F6"), normalized("F6"));
    }
}
