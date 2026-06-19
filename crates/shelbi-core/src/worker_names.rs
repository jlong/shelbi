//! Curated lists of worker names the project-setup wizard offers as presets.
//!
//! Each preset is a fixed, ordered list; the wizard takes the first N names
//! from the chosen preset to populate `workers:` in the project YAML.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerNamePreset {
    Phonetic,
    Greek,
    ToyStory,
}

impl WorkerNamePreset {
    pub const ALL: [WorkerNamePreset; 3] = [
        WorkerNamePreset::Phonetic,
        WorkerNamePreset::Greek,
        WorkerNamePreset::ToyStory,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            WorkerNamePreset::Phonetic => "phonetic",
            WorkerNamePreset::Greek => "greek",
            WorkerNamePreset::ToyStory => "toy_story",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            WorkerNamePreset::Phonetic => "NATO phonetic (alpha, bravo, charlie…)",
            WorkerNamePreset::Greek => "Greek letters (alpha, beta, gamma…)",
            WorkerNamePreset::ToyStory => "Toy Story (woody, buzz, jessie…)",
        }
    }

    pub fn names(self) -> &'static [&'static str] {
        match self {
            WorkerNamePreset::Phonetic => PHONETIC,
            WorkerNamePreset::Greek => GREEK,
            WorkerNamePreset::ToyStory => TOY_STORY,
        }
    }
}

impl std::fmt::Display for WorkerNamePreset {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for WorkerNamePreset {
    type Err = crate::Error;
    fn from_str(s: &str) -> crate::Result<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "phonetic" | "nato" => Ok(WorkerNamePreset::Phonetic),
            "greek" => Ok(WorkerNamePreset::Greek),
            "toy_story" | "toy-story" | "toystory" => Ok(WorkerNamePreset::ToyStory),
            other => Err(crate::Error::Other(format!(
                "unknown worker name preset: {other}"
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
        for p in WorkerNamePreset::ALL {
            let names = p.names();
            assert!(!names.is_empty(), "{p} preset is empty");
            assert!(names.len() >= 10, "{p} preset has fewer than 10 names");
        }
    }

    #[test]
    fn names_are_valid_agent_ids() {
        for p in WorkerNamePreset::ALL {
            for name in p.names() {
                crate::validate_agent_id(name).unwrap_or_else(|e| {
                    panic!("preset {p} has invalid name {name:?}: {e}");
                });
            }
        }
    }

    #[test]
    fn presets_open_with_documented_names() {
        assert_eq!(WorkerNamePreset::Phonetic.names()[0], "alpha");
        assert_eq!(WorkerNamePreset::Phonetic.names()[1], "bravo");
        assert_eq!(WorkerNamePreset::Phonetic.names()[2], "charlie");
        assert_eq!(WorkerNamePreset::Greek.names()[0], "alpha");
        assert_eq!(WorkerNamePreset::Greek.names()[1], "beta");
        assert_eq!(WorkerNamePreset::ToyStory.names()[0], "woody");
        assert_eq!(WorkerNamePreset::ToyStory.names()[1], "buzz");
    }

    #[test]
    fn preset_round_trips_through_string() {
        for p in WorkerNamePreset::ALL {
            assert_eq!(WorkerNamePreset::from_str(p.as_str()).unwrap(), p);
        }
        assert_eq!(
            WorkerNamePreset::from_str("toy-story").unwrap(),
            WorkerNamePreset::ToyStory
        );
        assert_eq!(
            WorkerNamePreset::from_str("NATO").unwrap(),
            WorkerNamePreset::Phonetic
        );
        assert!(WorkerNamePreset::from_str("garbage").is_err());
    }

    #[test]
    fn preset_serde_uses_snake_case() {
        let y = serde_yaml::to_string(&WorkerNamePreset::ToyStory).unwrap();
        assert_eq!(y.trim(), "toy_story");
        let back: WorkerNamePreset = serde_yaml::from_str(&y).unwrap();
        assert_eq!(back, WorkerNamePreset::ToyStory);
    }
}
