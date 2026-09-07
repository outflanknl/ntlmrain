//! Run-directory creation, manifests, and atomic artifact writes.

use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
};

use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::formats::Role;

pub const DEFAULT_ARTIFACT_ROOT: &str = "artifacts";
pub const MANIFEST_FILENAME: &str = "manifest.json";

#[derive(Clone, Debug)]
pub struct RunArtifacts {
    pub root: PathBuf,
    pub directory: PathBuf,
    pub run_id: String,
    pub created_at: DateTime<Utc>,
}

impl RunArtifacts {
    /// Create a uniquely named run directory under `root`.
    pub fn create(root: impl AsRef<Path>) -> Result<Self, ArtifactError> {
        Self::create_at(root.as_ref(), Utc::now())
    }

    pub fn create_default() -> Result<Self, ArtifactError> {
        Self::create(DEFAULT_ARTIFACT_ROOT)
    }

    fn create_at(root: &Path, created_at: DateTime<Utc>) -> Result<Self, ArtifactError> {
        fs::create_dir_all(root)?;
        for _ in 0..32 {
            let run_id = new_run_id();
            let timestamp = created_at.format("%Y%m%dT%H%M%SZ");
            let directory = root.join(format!("ntlmrain-{timestamp}-{run_id}"));
            match fs::create_dir(&directory) {
                Ok(()) => {
                    return Ok(Self {
                        root: root.to_path_buf(),
                        directory,
                        run_id,
                        created_at,
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error.into()),
            }
        }
        Err(ArtifactError::RunIdExhausted)
    }

    pub fn endpoint_path(&self, role: Role) -> PathBuf {
        self.directory.join(format!(
            "{}-{}.endpoints",
            role.filename_prefix(),
            self.run_id
        ))
    }

    pub fn candidate_path(&self, role: Role) -> PathBuf {
        self.directory.join(format!(
            "{}-{}.candidates",
            role.filename_prefix(),
            self.run_id
        ))
    }

    pub fn manifest_path(&self) -> PathBuf {
        self.directory.join(MANIFEST_FILENAME)
    }

    pub fn new_manifest(&self, command: impl Into<String>) -> RunManifest {
        RunManifest {
            schema_version: 1,
            tool_version: env!("CARGO_PKG_VERSION").to_owned(),
            run_id: self.run_id.clone(),
            created_at: self.created_at.to_rfc3339_opts(SecondsFormat::Secs, true),
            command: command.into(),
            input: None,
            compute: None,
            selected_device: None,
            tuning: None,
            outputs: BTreeMap::new(),
            result: None,
        }
    }

    /// Write the final manifest atomically. An existing manifest is never
    /// overwritten, preventing concurrent or resumed runs from being confused.
    pub fn write_manifest(&self, manifest: &RunManifest) -> Result<(), ArtifactError> {
        if manifest.run_id != self.run_id {
            return Err(ArtifactError::ManifestRunIdMismatch {
                expected: self.run_id.clone(),
                actual: manifest.run_id.clone(),
            });
        }
        let mut bytes = serde_json::to_vec_pretty(manifest)?;
        bytes.push(b'\n');
        atomic_write(&self.manifest_path(), &bytes)?;
        Ok(())
    }
}

/// Stable top-level run record. Device/tuning/result payloads intentionally use
/// JSON values so their subsystem schemas can evolve without changing artifact
/// creation code.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct RunManifest {
    pub schema_version: u32,
    pub tool_version: String,
    pub run_id: String,
    pub created_at: String,
    pub command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compute: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected_device: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tuning: Option<Value>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub outputs: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
}

#[derive(Debug, Error)]
pub enum ArtifactError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("could not allocate a unique run ID")]
    RunIdExhausted,
    #[error("manifest run ID is {actual}, expected {expected}")]
    ManifestRunIdMismatch { expected: String, actual: String },
}

/// Atomically publish a newly created artifact in its destination directory.
///
/// Bytes are fully written and synced to a unique sibling temporary file before
/// the rename. Existing targets are rejected, which keeps this operation atomic
/// on Windows as well as Unix and prevents accidental artifact replacement.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    if path.exists() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("artifact already exists: {}", path.display()),
        ));
    }

    let filename = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("artifact");
    let mut last_collision = None;
    for _ in 0..32 {
        let suffix: u64 = rand::random();
        let temporary = parent.join(format!(".{filename}.{suffix:016x}.tmp"));
        let mut file = match OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
        {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                last_collision = Some(error);
                continue;
            }
            Err(error) => return Err(error),
        };

        let result = (|| {
            file.write_all(bytes)?;
            file.sync_all()?;
            drop(file);
            if path.exists() {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!("artifact already exists: {}", path.display()),
                ));
            }
            fs::rename(&temporary, path)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        return result;
    }
    Err(last_collision.unwrap_or_else(|| {
        io::Error::new(io::ErrorKind::AlreadyExists, "temporary filename collision")
    }))
}

fn new_run_id() -> String {
    hex::encode(rand::random::<[u8; 6]>())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formats::{CandidateFile, CandidateRecord, EndpointFile};

    #[test]
    fn creates_named_run_and_shared_artifact_ids() {
        let temp = tempfile::tempdir().unwrap();
        let run = RunArtifacts::create(temp.path()).unwrap();
        assert_eq!(run.run_id.len(), 12);
        assert!(run.run_id.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert!(run.directory.is_dir());

        let endpoint = run.endpoint_path(Role::Des1);
        let candidate = run.candidate_path(Role::Des2);
        assert!(endpoint.ends_with(format!("des1-{}.endpoints", run.run_id)));
        assert!(candidate.ends_with(format!("des2-{}.candidates", run.run_id)));

        EndpointFile::new(vec![1]).write_atomic(&endpoint).unwrap();
        CandidateFile::new(
            1,
            vec![CandidateRecord {
                ordinal: 0,
                start: 2,
            }],
        )
        .unwrap()
        .write_atomic(&candidate)
        .unwrap();
        assert!(endpoint.is_file());
        assert!(candidate.is_file());
    }

    #[test]
    fn atomic_write_never_overwrites() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("result.bin");
        atomic_write(&path, b"first").unwrap();
        let error = atomic_write(&path, b"second").unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(path).unwrap(), b"first");
        assert!(fs::read_dir(temp.path()).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp")
        }));
    }

    #[test]
    fn manifest_is_stable_and_validated() {
        let temp = tempfile::tempdir().unwrap();
        let run = RunArtifacts::create(temp.path()).unwrap();
        let mut manifest = run.new_manifest("crack");
        manifest.outputs.insert(
            "des1_endpoints".into(),
            run.endpoint_path(Role::Des1).to_string_lossy().into_owned(),
        );
        run.write_manifest(&manifest).unwrap();
        let decoded: RunManifest =
            serde_json::from_slice(&fs::read(run.manifest_path()).unwrap()).unwrap();
        assert_eq!(decoded, manifest);
        assert_eq!(decoded.schema_version, 1);

        let duplicate = run.write_manifest(&manifest).unwrap_err();
        assert!(
            matches!(duplicate, ArtifactError::Io(ref error) if error.kind() == io::ErrorKind::AlreadyExists)
        );
    }

    #[test]
    fn rejects_manifest_for_another_run() {
        let temp = tempfile::tempdir().unwrap();
        let run = RunArtifacts::create(temp.path()).unwrap();
        let mut manifest = run.new_manifest("lookup");
        manifest.run_id = "000000000000".into();
        assert!(matches!(
            run.write_manifest(&manifest),
            Err(ArtifactError::ManifestRunIdMismatch { .. })
        ));
        assert!(!run.manifest_path().exists());
    }
}
