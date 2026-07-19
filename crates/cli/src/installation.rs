use std::{
    env,
    path::{Path, PathBuf},
};

use serde::Serialize;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DoctorSurface {
    DirectCli,
    PluginMcp,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LegacySkillStatus {
    pub path: PathBuf,
    pub present: bool,
    pub plugin_surface_active: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error(
    "legacy standalone Skill exists at {path}; back up or remove it manually, install matching binary and plugin versions, then restart Codex or open a new task"
)]
pub struct LegacySkillError {
    path: PathBuf,
}

impl LegacySkillError {
    pub fn code(&self) -> &'static str {
        "LEGACY_SKILL_CONFLICT"
    }
}

pub fn inspect_legacy_skill(
    codex_home: &Path,
    surface: DoctorSurface,
) -> Result<LegacySkillStatus, LegacySkillError> {
    let path = codex_home.join("skills/worktree-merge-consensus");
    let present = path.exists();
    let plugin_surface_active = surface == DoctorSurface::PluginMcp;
    if present && !plugin_surface_active {
        return Err(LegacySkillError { path });
    }
    Ok(LegacySkillStatus {
        path,
        present,
        plugin_surface_active,
    })
}

pub fn inspect_effective_legacy_skill(
    surface: DoctorSurface,
) -> Result<Option<LegacySkillStatus>, LegacySkillError> {
    effective_codex_home()
        .map(|home| inspect_legacy_skill(&home, surface))
        .transpose()
}

fn effective_codex_home() -> Option<PathBuf> {
    nonempty_env("CODEX_HOME").map(PathBuf::from).or_else(|| {
        nonempty_env("HOME")
            .map(PathBuf::from)
            .map(|home| home.join(".codex"))
    })
}

fn nonempty_env(name: &str) -> Option<std::ffi::OsString> {
    env::var_os(name).filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn existing_legacy_skill_conflicts_outside_plugin_surface_without_mutation() {
        let home = tempfile::tempdir().unwrap();
        let skill = home.path().join("skills/worktree-merge-consensus/SKILL.md");
        fs::create_dir_all(skill.parent().unwrap()).unwrap();
        fs::write(&skill, "legacy workflow\n").unwrap();

        let error = inspect_legacy_skill(home.path(), DoctorSurface::DirectCli).unwrap_err();

        assert_eq!(error.code(), "LEGACY_SKILL_CONFLICT");
        assert!(error.to_string().contains("back up or remove"));
        assert_eq!(fs::read_to_string(&skill).unwrap(), "legacy workflow\n");
    }

    #[test]
    fn active_plugin_surface_reports_legacy_path_without_blocking_itself() {
        let home = tempfile::tempdir().unwrap();
        let skill = home.path().join("skills/worktree-merge-consensus/SKILL.md");
        fs::create_dir_all(skill.parent().unwrap()).unwrap();
        fs::write(&skill, "legacy workflow\n").unwrap();

        let status = inspect_legacy_skill(home.path(), DoctorSurface::PluginMcp).unwrap();

        assert!(status.present);
        assert!(status.plugin_surface_active);
        assert_eq!(status.path, skill.parent().unwrap());
    }

    #[test]
    fn absent_legacy_skill_is_a_clean_diagnostic() {
        let home = tempfile::tempdir().unwrap();

        let status = inspect_legacy_skill(home.path(), DoctorSurface::DirectCli).unwrap();

        assert!(!status.present);
        assert!(!status.plugin_surface_active);
    }
}
