//! Directory → account mappings for `ax run` auto-resolution.
//!
//! Maps a canonical absolute directory path to a stored account identity
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
    mappings: BTreeMap<String, Mapping>,
}

pub fn all() -> Result<BTreeMap<String, Mapping>> {
    Ok(load()?.mappings)
}

pub fn set(directory: &Path, email: &str, organization_uuid: &str) -> Result<String> {
    let key = normalize(directory)?;
    let mut file = load()?;
    file.mappings.insert(
        key.clone(),
        Mapping {
            email: email.to_string(),
            organization_uuid: organization_uuid.to_string(),
            added: crate::timestamp(),
        },
    );
    save(&file)?;
    Ok(key)
}

pub fn remove(directory: &Path) -> Result<bool> {
    let key = normalize(directory)?;
    let mut file = load()?;
    if file.mappings.remove(&key).is_none() {
        Ok(false)
    } else {
        save(&file)?;
        Ok(true)
    }
}

pub fn resolve(directory: &Path) -> Result<Option<Mapping>> {
    let target = normalize(directory)?;
    let deepest_mapped_ancestor = all()?
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
        })
    }
}

fn save(file: &MappingsFile) -> Result<()> {
    fsutil::write_json_atomically(&mappings_path(), file)
}

fn mappings_path() -> PathBuf {
    paths::data_dir().join("mappings.json")
}
