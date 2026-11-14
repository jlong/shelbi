//! Close All Other Projects: the one confirmation both clients ask before
//! closing every open project but one (the palette's command, a desktop
//! tab's context menu).
//!
//! The copy lives here so the desktop's sheet and the TUI's dialog say the
//! same thing: how many projects close, which ones, and that their agents
//! stop.

/// What the confirmation says, and its confirm button's label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Confirmation {
    /// "Close 2 other projects?"
    pub title: String,
    /// Names the projects and says their agents stop.
    pub detail: String,
    /// "Close Projects" (or "Close Project" for one).
    pub confirm: String,
}

/// The confirmation for closing `closing` (their labels, in tab order)
/// while `keep` (its label) stays open.
pub fn confirmation(keep: &str, closing: &[String]) -> Confirmation {
    let n = closing.len();
    let (title, confirm) = if n == 1 {
        ("Close 1 other project?".to_string(), "Close Project".to_string())
    } else {
        (format!("Close {n} other projects?"), "Close Projects".to_string())
    };
    let (their, close) = if n == 1 { ("its", "closes") } else { ("their", "close") };
    Confirmation {
        title,
        detail: format!(
            "{} {close} and {their} agents stop. {keep} keeps running.",
            join_names(closing)
        ),
        confirm,
    }
}

/// `a`, `a and b`, `a, b and c`.
fn join_names(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// The projects in `open` (in order) that Close All Other Projects closes:
/// all but `keep`.
pub fn others<'a>(open: &'a [String], keep: &str) -> Vec<&'a str> {
    open.iter().map(String::as_str).filter(|p| *p != keep).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(n: &[&str]) -> Vec<String> {
        n.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn it_names_how_many_and_which_and_that_their_agents_stop() {
        let c = confirmation("Shelbi", &names(&["ChartMe", "Website"]));
        assert_eq!(c.title, "Close 2 other projects?");
        assert_eq!(c.detail, "ChartMe and Website close and their agents stop. Shelbi keeps running.");
        assert_eq!(c.confirm, "Close Projects");

        let c = confirmation("Shelbi", &names(&["ChartMe"]));
        assert_eq!(c.title, "Close 1 other project?");
        assert_eq!(c.detail, "ChartMe closes and its agents stop. Shelbi keeps running.");
        assert_eq!(c.confirm, "Close Project");

        let c = confirmation("Shelbi", &names(&["A", "B", "C"]));
        assert!(c.detail.starts_with("A, B and C close"), "{}", c.detail);
    }

    #[test]
    fn others_keeps_order_and_drops_the_kept_one() {
        let open = names(&["a", "b", "c"]);
        assert_eq!(others(&open, "b"), ["a", "c"]);
        assert!(others(&names(&["a"]), "a").is_empty());
    }
}
