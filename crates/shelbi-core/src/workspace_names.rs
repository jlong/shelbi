//! Curated lists of workspace names the project-setup wizard offers as presets.
//!
//! Each preset is a fixed, ordered list; the wizard takes the first N names
//! from the chosen preset to populate `workspaces:` in the project YAML.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceNamePreset {
    Phonetic,
    Greek,
    ToyStory,
}

impl WorkspaceNamePreset {
    pub const ALL: [WorkspaceNamePreset; 3] = [
        WorkspaceNamePreset::Phonetic,
        WorkspaceNamePreset::Greek,
        WorkspaceNamePreset::ToyStory,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            WorkspaceNamePreset::Phonetic => "phonetic",
            WorkspaceNamePreset::Greek => "greek",
            WorkspaceNamePreset::ToyStory => "toy_story",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            WorkspaceNamePreset::Phonetic => "NATO phonetic (alpha, bravo, charlie…)",
            WorkspaceNamePreset::Greek => "Greek letters (alpha, beta, gamma…)",
            WorkspaceNamePreset::ToyStory => "Toy Story (woody, buzz, jessie…)",
        }
    }

    pub fn names(self) -> &'static [&'static str] {
        match self {
            WorkspaceNamePreset::Phonetic => PHONETIC,
            WorkspaceNamePreset::Greek => GREEK,
            WorkspaceNamePreset::ToyStory => TOY_STORY,
        }
    }
}

impl std::fmt::Display for WorkspaceNamePreset {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for WorkspaceNamePreset {
    type Err = crate::Error;
    fn from_str(s: &str) -> crate::Result<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "phonetic" | "nato" => Ok(WorkspaceNamePreset::Phonetic),
            "greek" => Ok(WorkspaceNamePreset::Greek),
            "toy_story" | "toy-story" | "toystory" => Ok(WorkspaceNamePreset::ToyStory),
            other => Err(crate::Error::Other(format!(
                "unknown workspace name preset: {other}"
            ))),
        }
    }
}

const PHONETIC: &[&str] = &[
    "alpha", "bravo", "charlie", "delta", "echo", "foxtrot", "golf", "hotel", "india", "juliet",
    "kilo", "lima", "mike", "november", "oscar", "papa", "quebec", "romeo", "sierra", "tango",
    "uniform", "victor", "whiskey", "xray", "yankee", "zulu",
];

const GREEK: &[&str] = &[
    "alpha", "beta", "gamma", "delta", "epsilon", "zeta", "eta", "theta", "iota", "kappa",
    "lambda", "mu", "nu", "xi", "omicron", "pi", "rho", "sigma", "tau", "upsilon", "phi", "chi",
    "psi", "omega",
];

const TOY_STORY: &[&str] = &[
    "woody",
    "buzz",
    "jessie",
    "rex",
    "hamm",
    "slinky",
    "bullseye",
    "mr_potato",
    "mrs_potato",
    "forky",
    "bo_peep",
    "stinky_pete",
    "wheezy",
    "trixie",
    "dolly",
    "buttercup",
    "ducky",
    "bunny",
    "gabby_gabby",
    "duke_caboom",
];

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn presets_have_ordered_non_empty_lists() {
        for p in WorkspaceNamePreset::ALL {
            let names = p.names();
            assert!(!names.is_empty(), "{p} preset is empty");
            assert!(names.len() >= 10, "{p} preset has fewer than 10 names");
        }
    }

    #[test]
    fn names_are_valid_agent_ids() {
        for p in WorkspaceNamePreset::ALL {
            for name in p.names() {
                crate::validate_agent_id(name).unwrap_or_else(|e| {
                    panic!("preset {p} has invalid name {name:?}: {e}");
                });
            }
        }
    }

    #[test]
    fn presets_open_with_documented_names() {
        assert_eq!(WorkspaceNamePreset::Phonetic.names()[0], "alpha");
        assert_eq!(WorkspaceNamePreset::Phonetic.names()[1], "bravo");
        assert_eq!(WorkspaceNamePreset::Phonetic.names()[2], "charlie");
        assert_eq!(WorkspaceNamePreset::Greek.names()[0], "alpha");
        assert_eq!(WorkspaceNamePreset::Greek.names()[1], "beta");
        assert_eq!(WorkspaceNamePreset::ToyStory.names()[0], "woody");
        assert_eq!(WorkspaceNamePreset::ToyStory.names()[1], "buzz");
    }

    #[test]
    fn preset_round_trips_through_string() {
        for p in WorkspaceNamePreset::ALL {
            assert_eq!(WorkspaceNamePreset::from_str(p.as_str()).unwrap(), p);
        }
        assert_eq!(
            WorkspaceNamePreset::from_str("toy-story").unwrap(),
            WorkspaceNamePreset::ToyStory
        );
        assert_eq!(
            WorkspaceNamePreset::from_str("NATO").unwrap(),
            WorkspaceNamePreset::Phonetic
        );
        assert!(WorkspaceNamePreset::from_str("garbage").is_err());
    }

    #[test]
    fn preset_serde_uses_snake_case() {
        let y = serde_yaml::to_string(&WorkspaceNamePreset::ToyStory).unwrap();
        assert_eq!(y.trim(), "toy_story");
        let back: WorkspaceNamePreset = serde_yaml::from_str(&y).unwrap();
        assert_eq!(back, WorkspaceNamePreset::ToyStory);
    }
}
