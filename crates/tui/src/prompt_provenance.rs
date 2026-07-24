//! Provenance of the effective base prompt (the Constitution) — issue #3928.
//!
//! The Constitution *is* the base prompt, and it can come from three places:
//!
//! 1. the compiled-in `BASE_PROMPT` constant (the default),
//! 2. a config-directory override file (`<config>/prompts/constitution.md`),
//!    which additionally requires the `CODEWHALE_ALLOW_BASE_PROMPT_OVERRIDE`
//!    opt-in, or
//! 3. an embedder override installed through
//!    [`crate::prompts::set_base_prompt_override`].
//!
//! Before #3928 none of this was visible from inside the app: `/context`
//! printed a hardcoded in-tree source path, and the "override file present but
//! gated off" case was a `tracing::warn` nobody sees. Users could believe a
//! custom Constitution was live while running the bundled one — the worst
//! failure mode for a trust-centric feature.
//!
//! This module owns the *resolution* of that provenance. It deliberately does
//! not live in `prompts.rs`: the only hooks it needs there are two tiny
//! accessors (`base_prompt_override_active`, `effective_base_prompt_text`).
//!
//! The interesting logic is [`resolve`], which is pure over its inputs and
//! therefore directly testable without touching process-global state.

use std::path::{Path, PathBuf};

use crate::prompts::{BASE_PROMPT_OVERRIDE_OPT_IN_ENV, CONSTITUTION_OVERRIDE_FILE};

/// Where the effective base prompt (Constitution) came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BasePromptProvenance {
    /// The compiled-in constant. No override file, no embedder override.
    Bundled,
    /// A config-directory override file is in force.
    ConfigDirOverride { path: PathBuf },
    /// A config-directory override file exists but the env opt-in is unset, so
    /// the bundled Constitution is what actually reaches the model.
    ConfigDirOverrideGatedOff { path: PathBuf },
    /// The host application replaced the base prompt programmatically.
    EmbedderOverride,
}

impl BasePromptProvenance {
    /// Whether the bundled (compiled-in) text is what reaches the model.
    pub(crate) fn is_bundled_in_force(&self) -> bool {
        matches!(self, Self::Bundled | Self::ConfigDirOverrideGatedOff { .. })
    }

    /// Short label for the `/context` source column and other one-line UIs.
    ///
    /// This replaces the old hardcoded `crates/tui/src/prompts/text.rs`
    /// string, which was meaningless to anyone who ran `cargo install`.
    pub(crate) fn source_label(&self) -> String {
        match self {
            Self::Bundled => "bundled (compiled in)".to_string(),
            Self::ConfigDirOverride { path } => {
                format!("override: {}", display_path(path))
            }
            Self::ConfigDirOverrideGatedOff { path } => format!(
                "bundled (compiled in); override at {} is disabled ({} unset)",
                display_path(path),
                BASE_PROMPT_OVERRIDE_OPT_IN_ENV,
            ),
            Self::EmbedderOverride => "override: embedder (set by host application)".to_string(),
        }
    }

    /// Full-sentence provenance for the `/constitution text` pager header.
    pub(crate) fn description(&self) -> String {
        match self {
            Self::Bundled => {
                "Bundled Constitution (compiled into this binary). No override is active."
                    .to_string()
            }
            Self::ConfigDirOverride { path } => format!(
                "Custom Constitution ACTIVE, loaded from {}. The bundled Constitution is not in force.",
                display_path(path),
            ),
            Self::ConfigDirOverrideGatedOff { path } => format!(
                "Bundled Constitution in force. A custom Constitution exists at {} but is DISABLED because {} is not set; set {}=1 to opt in.",
                display_path(path),
                BASE_PROMPT_OVERRIDE_OPT_IN_ENV,
                BASE_PROMPT_OVERRIDE_OPT_IN_ENV,
            ),
            Self::EmbedderOverride => {
                "Custom Constitution ACTIVE, installed programmatically by the host application. The bundled Constitution is not in force."
                    .to_string()
            }
        }
    }
}

fn display_path(path: &Path) -> String {
    path.display().to_string()
}

/// Pure provenance resolution.
///
/// * `override_file` — the config-dir override file, when it exists and is
///   non-empty after trimming (an empty file is a no-op, matching
///   `prompts::read_prompt_override_file`).
/// * `opt_in` — whether `CODEWHALE_ALLOW_BASE_PROMPT_OVERRIDE` is set.
/// * `base_prompt_overridden` — whether the base-prompt override cell holds a
///   value.
/// * `override_matches_file` — whether the installed override text is the
///   override file's text. This distinguishes "the config-dir file won" from
///   "an embedder got there first" (the cell is first-call-wins, so a host
///   application can beat the config-dir loader).
pub(crate) fn resolve(
    override_file: Option<&Path>,
    opt_in: bool,
    base_prompt_overridden: bool,
    override_matches_file: bool,
) -> BasePromptProvenance {
    // An installed override whose text is not the file's text can only have
    // come from the embedder hook, and it is what actually reaches the model —
    // so it outranks anything the config directory has to say.
    if base_prompt_overridden && !override_matches_file {
        return BasePromptProvenance::EmbedderOverride;
    }
    match override_file {
        Some(path) if opt_in && base_prompt_overridden => BasePromptProvenance::ConfigDirOverride {
            path: path.to_path_buf(),
        },
        Some(path) => BasePromptProvenance::ConfigDirOverrideGatedOff {
            path: path.to_path_buf(),
        },
        None if base_prompt_overridden => BasePromptProvenance::EmbedderOverride,
        None => BasePromptProvenance::Bundled,
    }
}

/// The config-dir Constitution override file, when it exists and is non-empty.
fn override_file_in(config_dir: &Path) -> Option<PathBuf> {
    let path = config_dir.join(CONSTITUTION_OVERRIDE_FILE);
    let raw = std::fs::read_to_string(&path).ok()?;
    if raw.trim().is_empty() {
        return None;
    }
    Some(path)
}

/// Provenance as observed against a specific config directory. Split out from
/// [`current`] so tests can drive it with a tempdir.
pub(crate) fn provenance_for_config_dir(config_dir: &Path) -> BasePromptProvenance {
    let file = override_file_in(config_dir);
    let matches_file = match (file.as_ref(), crate::prompts::base_prompt_override_active()) {
        (Some(path), true) => std::fs::read_to_string(path)
            .map(|raw| raw == crate::prompts::effective_base_prompt_text())
            .unwrap_or(false),
        _ => false,
    };
    resolve(
        file.as_deref(),
        crate::prompts::base_prompt_override_opt_in(),
        crate::prompts::base_prompt_override_active(),
        matches_file,
    )
}

/// Provenance of the base prompt this process is actually using.
///
/// Falls back to inspecting only the override cell when the config home cannot
/// be resolved, which mirrors `prompts::load_prompt_overrides_from_config_home`.
pub(crate) fn current() -> BasePromptProvenance {
    match codewhale_config::codewhale_home() {
        Ok(home) => provenance_for_config_dir(&home),
        Err(_) => resolve(
            None,
            crate::prompts::base_prompt_override_opt_in(),
            crate::prompts::base_prompt_override_active(),
            false,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_bundled_when_nothing_overrides() {
        assert_eq!(
            resolve(None, false, false, false),
            BasePromptProvenance::Bundled
        );
        // Opting in without an override file is still bundled.
        assert_eq!(
            resolve(None, true, false, false),
            BasePromptProvenance::Bundled
        );
    }

    #[test]
    fn resolves_gated_off_when_file_present_without_opt_in() {
        let path = Path::new("/cfg/prompts/constitution.md");
        let provenance = resolve(Some(path), false, false, false);
        assert_eq!(
            provenance,
            BasePromptProvenance::ConfigDirOverrideGatedOff {
                path: path.to_path_buf()
            }
        );
        assert!(provenance.is_bundled_in_force());
        assert!(provenance.source_label().contains("bundled (compiled in)"));
        assert!(
            provenance
                .source_label()
                .contains(BASE_PROMPT_OVERRIDE_OPT_IN_ENV)
        );
        assert!(provenance.description().contains("DISABLED"));
    }

    #[test]
    fn resolves_gated_off_when_opt_in_set_but_override_never_installed() {
        // Opt-in alone is not enough: the loader must have actually installed
        // the text (e.g. it may have run before the file was written).
        let path = Path::new("/cfg/prompts/constitution.md");
        assert_eq!(
            resolve(Some(path), true, false, false),
            BasePromptProvenance::ConfigDirOverrideGatedOff {
                path: path.to_path_buf()
            }
        );
    }

    #[test]
    fn resolves_config_dir_override_when_opt_in_and_installed() {
        let path = Path::new("/cfg/prompts/constitution.md");
        let provenance = resolve(Some(path), true, true, true);
        assert_eq!(
            provenance,
            BasePromptProvenance::ConfigDirOverride {
                path: path.to_path_buf()
            }
        );
        assert!(!provenance.is_bundled_in_force());
        assert!(provenance.source_label().starts_with("override: "));
        assert!(provenance.source_label().contains("constitution.md"));
        assert!(provenance.description().contains("ACTIVE"));
    }

    #[test]
    fn embedder_override_outranks_config_dir_file() {
        // The override cell is first-call-wins: if an embedder installed text
        // that is not the file's text, the embedder's text is what ships.
        let path = Path::new("/cfg/prompts/constitution.md");
        assert_eq!(
            resolve(Some(path), true, true, false),
            BasePromptProvenance::EmbedderOverride
        );
        assert_eq!(
            resolve(None, false, true, false),
            BasePromptProvenance::EmbedderOverride
        );
        assert!(!BasePromptProvenance::EmbedderOverride.is_bundled_in_force());
        assert!(
            BasePromptProvenance::EmbedderOverride
                .source_label()
                .contains("embedder")
        );
    }

    #[test]
    fn bundled_source_label_is_not_a_repo_path() {
        let label = BasePromptProvenance::Bundled.source_label();
        assert_eq!(label, "bundled (compiled in)");
        assert!(!label.contains("crates/"));
    }

    #[test]
    fn empty_override_file_is_ignored() {
        let dir = tempfile::tempdir().expect("tempdir");
        let prompts_dir = dir.path().join("prompts");
        std::fs::create_dir_all(&prompts_dir).expect("prompts dir");
        std::fs::write(prompts_dir.join("constitution.md"), "   \n").expect("write");
        assert!(override_file_in(dir.path()).is_none());
    }

    #[test]
    fn non_empty_override_file_is_detected() {
        let dir = tempfile::tempdir().expect("tempdir");
        let prompts_dir = dir.path().join("prompts");
        std::fs::create_dir_all(&prompts_dir).expect("prompts dir");
        std::fs::write(prompts_dir.join("constitution.md"), "custom law\n").expect("write");
        assert_eq!(
            override_file_in(dir.path()),
            Some(prompts_dir.join("constitution.md"))
        );
    }

    #[test]
    fn gated_off_file_reports_gated_off_against_a_real_config_dir() {
        let _lock = crate::test_support::lock_test_env();
        let _env = crate::test_support::EnvVarGuard::remove(BASE_PROMPT_OVERRIDE_OPT_IN_ENV);
        let dir = tempfile::tempdir().expect("tempdir");
        let prompts_dir = dir.path().join("prompts");
        std::fs::create_dir_all(&prompts_dir).expect("prompts dir");
        std::fs::write(prompts_dir.join("constitution.md"), "custom law\n").expect("write");

        // NOTE: `base_prompt_override_active()` is process-global and may have
        // been set by another test in this binary; the gated-off classification
        // below only holds while it is unset, which is the shipped default.
        if !crate::prompts::base_prompt_override_active() {
            assert_eq!(
                provenance_for_config_dir(dir.path()),
                BasePromptProvenance::ConfigDirOverrideGatedOff {
                    path: prompts_dir.join("constitution.md")
                }
            );
        }
    }

    #[test]
    fn missing_config_dir_reports_bundled_when_no_override_installed() {
        let dir = tempfile::tempdir().expect("tempdir");
        if !crate::prompts::base_prompt_override_active() {
            assert_eq!(
                provenance_for_config_dir(dir.path()),
                BasePromptProvenance::Bundled
            );
        }
    }
}
