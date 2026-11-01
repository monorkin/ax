//! Directory → account mappings for `ax run` auto-resolution.
//!
//! Maps a canonical absolute directory path, per tool, to a stored account identity
//! (email + organization UUID). Identity is stored as that composite rather
//! than the account number, since numbers are reused when accounts are
//! removed and re-added. Persisted to `<data_dir>/mappings.json`.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::fsutil;
use crate::paths;
use crate::store::Provider;

#[derive(Serialize, Deserialize, Clone)]
pub struct Mapping {
    pub email: String,
    #[serde(default, rename = "organizationUuid")]
    pub organization_uuid: String,
    pub added: String,
}

#[derive(Serialize, Deserialize, Default)]
struct MappingsFile {
    #[serde(rename = "schemaVersion")]
    schema_version: u32,
    /// Claude Code's, under the name they had before there were other tools.
    mappings: BTreeMap<String, Mapping>,
    #[serde(default, rename = "codexMappings", skip_serializing_if = "BTreeMap::is_empty")]
    codex_mappings: BTreeMap<String, Mapping>,
}

impl MappingsFile {
    fn of(&self, provider: Provider) -> &BTreeMap<String, Mapping> {
        match provider {
            Provider::Claude => &self.mappings,
            Provider::Codex => &self.codex_mappings,
        }
    }

    fn of_mut(&mut self, provider: Provider) -> &mut BTreeMap<String, Mapping> {
        match provider {
            Provider::Claude => &mut self.mappings,
            Provider::Codex => &mut self.codex_mappings,
        }
    }
}

pub fn all(provider: Provider) -> Result<BTreeMap<String, Mapping>> {
    Ok(load()?.of(provider).clone())
}

pub fn set(directory: &Path, provider: Provider, email: &str, organization_uuid: &str) -> Result<String> {
    let key = normalize(directory)?;
    let mut file = load()?;
    file.of_mut(provider).insert(
        key.clone(),
        Mapping {
            email: email.to_string(),
            organization_uuid: organization_uuid.to_string(),
            added: crate::clock::timestamp(),
        },
    );
    save(&file)?;
    Ok(key)
}

pub fn remove(directory: &Path, provider: Provider) -> Result<bool> {
    let key = normalize(directory)?;
    let mut file = load()?;
    if file.of_mut(provider).remove(&key).is_none() {
        Ok(false)
    } else {
        save(&file)?;
        Ok(true)
    }
}

/// Each tool goes by its own deepest mapping, so a subdirectory can change
/// one tool's account and inherit the other's.
pub fn resolve(directory: &Path, provider: Provider) -> Result<Option<Mapping>> {
    let target = normalize(directory)?;
    let deepest_mapped_ancestor = all(provider)?
        .into_iter()
        .filter(|(mapped, _)| covers(mapped, &target))
        .max_by_key(|(mapped, _)| mapped.len())
        .map(|(_, mapping)| mapping);
    Ok(deepest_mapped_ancestor)
}

fn covers(mapped: &str, target: &str) -> bool {
    if let Some(rest) = target.strip_prefix(mapped) {
        rest.is_empty() || rest.starts_with('/')
    } else {
        false
    }
}

fn normalize(directory: &Path) -> Result<String> {
    let canonical = directory
        .canonicalize()
        .with_context(|| format!("no such directory: {}", directory.display()))?;
    Ok(canonical.to_string_lossy().into_owned())
}

fn load() -> Result<MappingsFile> {
    let path = mappings_path();
    if path.exists() {
        let contents = fs::read_to_string(&path)?;
        Ok(serde_json::from_str(&contents)
            .with_context(|| format!("could not parse {}", path.display()))?)
    } else {
        Ok(MappingsFile {
            schema_version: 1,
            mappings: BTreeMap::new(),
            codex_mappings: BTreeMap::new(),
        })
    }
}

fn save(file: &MappingsFile) -> Result<()> {
    fsutil::write_json_atomically(&mappings_path(), file)
}

fn mappings_path() -> PathBuf {
    paths::data_dir().join("mappings.json")
}
