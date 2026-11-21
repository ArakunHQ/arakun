//! Scenes that ship with Arakun as Rhai, so every project has examples to
//! start from, and Change has a script to copy into the project.

use std::path::Path;

use super::script::ScriptScene;

pub struct Starter {
    /// The file name it gets when copied into a project's `iso/`.
    pub file: &'static str,
    pub source: &'static str,
}

pub const ALL: &[Starter] = &[
    Starter {
        file: "billing.rhai",
        source: include_str!("../../iso/billing.rhai"),
    },
    Starter {
        file: "cpu.rhai",
        source: include_str!("../../iso/cpu.rhai"),
    },
    Starter {
        file: "ec2.rhai",
        source: include_str!("../../iso/ec2.rhai"),
    },
    Starter {
        file: "raccoon.rhai",
        source: include_str!("../../iso/raccoon.rhai"),
    },
];

/// Compiles a starter. Its path is the bare file name: it is not on disk.
pub fn load(s: &Starter) -> Result<ScriptScene, String> {
    ScriptScene::from_source(Path::new(s.file), s.source)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starters_load_with_their_names() {
        let names: Vec<String> = ALL.iter().map(|s| load(s).unwrap().meta.name).collect();
        assert_eq!(names, ["Usage billing", "CPU", "EC2 instances", "Raccoon"]);
    }

    #[test]
    fn starters_run_whole_loop() {
        assert!(!ALL.is_empty());
        for s in ALL {
            let scene = load(s).unwrap();
            for i in 0..60 {
                let t = i as f32 / 60.;
                assert!(!scene.frame(t).unwrap().is_empty(), "{} at {t}", s.file);
            }
        }
    }

    #[test]
    fn no_rust_scene_shares_a_starter_name() {
        let names: Vec<String> = ALL.iter().map(|s| load(s).unwrap().meta.name).collect();
        assert!(!names.is_empty());
        for s in super::super::scenes::ALL {
            assert!(!names.iter().any(|n| n == s.name), "{}", s.name);
        }
    }
}
