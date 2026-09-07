//! Web2-compatible endpoint and candidate artifact formats.

use std::{
    fmt, fs, io,
    path::{Path, PathBuf},
};

use thiserror::Error;

use crate::artifacts::atomic_write;

pub const ENDPOINT_MAGIC: &[u8; 8] = b"NTLMEND1";
pub const CANDIDATE_MAGIC: &[u8; 8] = b"NTLMCAN1";
pub const ENDPOINT_HEADER_BYTES: usize = 32;
pub const CANDIDATE_HEADER_BYTES: usize = 40;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Role {
    Des1,
    Des2,
}

impl Role {
    pub const fn key_index(self) -> usize {
        match self {
            Self::Des1 => 0,
            Self::Des2 => 1,
        }
    }

    pub const fn filename_prefix(self) -> &'static str {
        match self {
            Self::Des1 => "des1",
            Self::Des2 => "des2",
        }
    }

    pub const fn human_label(self) -> &'static str {
        match self {
            Self::Des1 => "DES1",
            Self::Des2 => "DES2",
        }
    }
}

impl fmt::Display for Role {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.filename_prefix())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EndpointFile {
    pub endpoints: Vec<u64>,
}

impl EndpointFile {
    pub fn new(endpoints: Vec<u64>) -> Self {
        Self { endpoints }
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut output = Vec::with_capacity(ENDPOINT_HEADER_BYTES + self.endpoints.len() * 8);
        output.extend_from_slice(ENDPOINT_MAGIC);
        push_u32(&mut output, 1);
        push_u32(&mut output, 8);
        push_u64(&mut output, self.endpoints.len() as u64);
        push_u32(&mut output, 0);
        push_u32(&mut output, 0);
        for endpoint in &self.endpoints {
            push_u64(&mut output, *endpoint);
        }
        output
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, FormatError> {
        if bytes.len() < ENDPOINT_HEADER_BYTES || bytes.get(..8) != Some(ENDPOINT_MAGIC) {
            return Err(FormatError::WrongMagic { kind: "endpoint" });
        }
        if read_u32(bytes, 8)? != 1
            || read_u32(bytes, 12)? != 8
            || read_u32(bytes, 24)? != 0
            || read_u32(bytes, 28)? != 0
        {
            return Err(FormatError::UnsupportedHeader { kind: "endpoint" });
        }
        let count = count_to_usize(read_u64(bytes, 16)?, "endpoint")?;
        let expected = exact_size(ENDPOINT_HEADER_BYTES, count, 8, "endpoint")?;
        if bytes.len() != expected {
            return Err(FormatError::LengthMismatch {
                kind: "endpoint",
                expected,
                actual: bytes.len(),
            });
        }
        let endpoints = bytes[ENDPOINT_HEADER_BYTES..]
            .chunks_exact(8)
            .map(|chunk| u64::from_le_bytes(chunk.try_into().expect("exact chunk")))
            .collect();
        Ok(Self { endpoints })
    }

    pub fn read(path: impl AsRef<Path>) -> Result<Self, FormatError> {
        Self::decode(&fs::read(path)?)
    }

    pub fn write_atomic(&self, path: impl AsRef<Path>) -> Result<(), FormatError> {
        atomic_write(path.as_ref(), &self.encode())?;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CandidateRecord {
    pub ordinal: u64,
    pub start: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CandidateFile {
    pub query_count: u64,
    pub records: Vec<CandidateRecord>,
}

impl CandidateFile {
    pub fn new(query_count: u64, records: Vec<CandidateRecord>) -> Result<Self, FormatError> {
        if records.iter().any(|record| record.ordinal >= query_count) {
            return Err(FormatError::OrdinalOutOfRange);
        }
        Ok(Self {
            query_count,
            records,
        })
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut output = Vec::with_capacity(CANDIDATE_HEADER_BYTES + self.records.len() * 16);
        output.extend_from_slice(CANDIDATE_MAGIC);
        push_u32(&mut output, 1);
        push_u32(&mut output, 16);
        push_u64(&mut output, self.query_count);
        push_u64(&mut output, self.records.len() as u64);
        push_u64(&mut output, 0);
        for record in &self.records {
            push_u64(&mut output, record.ordinal);
            push_u64(&mut output, record.start);
        }
        output
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, FormatError> {
        if bytes.len() < CANDIDATE_HEADER_BYTES || bytes.get(..8) != Some(CANDIDATE_MAGIC) {
            return Err(FormatError::WrongMagic { kind: "candidate" });
        }
        if read_u32(bytes, 8)? != 1 || read_u32(bytes, 12)? != 16 || read_u64(bytes, 32)? != 0 {
            return Err(FormatError::UnsupportedHeader { kind: "candidate" });
        }
        let query_count = read_u64(bytes, 16)?;
        let match_count = count_to_usize(read_u64(bytes, 24)?, "candidate")?;
        let expected = exact_size(CANDIDATE_HEADER_BYTES, match_count, 16, "candidate")?;
        if bytes.len() != expected {
            return Err(FormatError::LengthMismatch {
                kind: "candidate",
                expected,
                actual: bytes.len(),
            });
        }
        let mut records = Vec::with_capacity(match_count);
        for chunk in bytes[CANDIDATE_HEADER_BYTES..].chunks_exact(16) {
            let record = CandidateRecord {
                ordinal: u64::from_le_bytes(chunk[..8].try_into().expect("exact chunk")),
                start: u64::from_le_bytes(chunk[8..].try_into().expect("exact chunk")),
            };
            if record.ordinal >= query_count {
                return Err(FormatError::OrdinalOutOfRange);
            }
            records.push(record);
        }
        Ok(Self {
            query_count,
            records,
        })
    }

    pub fn read(path: impl AsRef<Path>) -> Result<Self, FormatError> {
        Self::decode(&fs::read(path)?)
    }

    pub fn write_atomic(&self, path: impl AsRef<Path>) -> Result<(), FormatError> {
        atomic_write(path.as_ref(), &self.encode())?;
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum FormatError {
    #[error("not a NetNTLMv1 {kind} file")]
    WrongMagic { kind: &'static str },
    #[error("unsupported or corrupt native {kind} file header")]
    UnsupportedHeader { kind: &'static str },
    #[error("{kind} file length is {actual} bytes; expected {expected}")]
    LengthMismatch {
        kind: &'static str,
        expected: usize,
        actual: usize,
    },
    #[error("{kind} record count does not fit this platform")]
    CountOverflow { kind: &'static str },
    #[error("candidate ordinal is outside the endpoint query")]
    OrdinalOutOfRange,
    #[error("filename has conflicting artifact role tokens: {0}")]
    ConflictingRole(String),
    #[error("two files must use the des1 and des2 filename roles")]
    MissingPairRoles,
    #[error("both files are named for {0}")]
    DuplicateRole(Role),
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// Infer the first or second ciphertext role from strict filename tokens.
///
/// Tokens are delimited by non-ASCII-alphanumeric characters, mirroring the
/// browser behavior. A filename containing tokens from both roles is rejected.
pub fn infer_role(path: impl AsRef<Path>) -> Result<Option<Role>, FormatError> {
    let filename = path
        .as_ref()
        .file_name()
        .unwrap_or_else(|| path.as_ref().as_os_str())
        .to_string_lossy();
    let mut found = None;
    for token in filename
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|token| !token.is_empty())
    {
        let token = token.to_ascii_lowercase();
        let role = match token.as_str() {
            "des1" => Some(Role::Des1),
            "des2" => Some(Role::Des2),
            _ => None,
        };
        if let Some(role) = role {
            if found.is_some_and(|existing| existing != role) {
                return Err(FormatError::ConflictingRole(filename.into_owned()));
            }
            found = Some(role);
        }
    }
    Ok(found)
}

/// Validate and order a one- or two-file stage input by key role.
pub fn order_paths_by_role(paths: &[PathBuf]) -> Result<Vec<(PathBuf, Option<Role>)>, FormatError> {
    let mut assigned = paths
        .iter()
        .map(|path| Ok((path.clone(), infer_role(path)?)))
        .collect::<Result<Vec<_>, FormatError>>()?;
    if assigned.len() == 2 {
        let Some(left) = assigned[0].1 else {
            return Err(FormatError::MissingPairRoles);
        };
        let Some(right) = assigned[1].1 else {
            return Err(FormatError::MissingPairRoles);
        };
        if left == right {
            return Err(FormatError::DuplicateRole(left));
        }
        assigned.sort_by_key(|(_, role)| role.expect("two roles").key_index());
    }
    Ok(assigned)
}

/// Extract the shared 12-hex run ID from an endpoint or candidate filename.
pub fn run_id_from_filename(path: impl AsRef<Path>) -> Option<String> {
    let name = path.as_ref().file_name()?.to_str()?;
    let lowercase = name.to_ascii_lowercase();
    let stem = lowercase
        .strip_suffix(".endpoints")
        .or_else(|| lowercase.strip_suffix(".candidates"))?;
    let suffix = stem.rsplit('-').next()?;
    (suffix.len() == 12 && suffix.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then(|| suffix.to_owned())
}

fn push_u32(output: &mut Vec<u8>, value: u32) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn push_u64(output: &mut Vec<u8>, value: u64) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, FormatError> {
    bytes
        .get(offset..offset + 4)
        .map(|slice| u32::from_le_bytes(slice.try_into().expect("four bytes")))
        .ok_or(FormatError::UnsupportedHeader { kind: "native" })
}

fn read_u64(bytes: &[u8], offset: usize) -> Result<u64, FormatError> {
    bytes
        .get(offset..offset + 8)
        .map(|slice| u64::from_le_bytes(slice.try_into().expect("eight bytes")))
        .ok_or(FormatError::UnsupportedHeader { kind: "native" })
}

fn count_to_usize(value: u64, kind: &'static str) -> Result<usize, FormatError> {
    usize::try_from(value).map_err(|_| FormatError::CountOverflow { kind })
}

fn exact_size(
    header: usize,
    count: usize,
    record: usize,
    kind: &'static str,
) -> Result<usize, FormatError> {
    count
        .checked_mul(record)
        .and_then(|payload| header.checked_add(payload))
        .ok_or(FormatError::CountOverflow { kind })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_wire_format_is_exact_and_round_trips() {
        let file = EndpointFile::new(vec![0x0123_4567_89ab_cdef, 7]);
        let bytes = file.encode();
        assert_eq!(&bytes[..8], b"NTLMEND1");
        assert_eq!(bytes.len(), 48);
        assert_eq!(&bytes[16..24], &2u64.to_le_bytes());
        assert_eq!(&bytes[32..40], &0x0123_4567_89ab_cdefu64.to_le_bytes());
        assert_eq!(EndpointFile::decode(&bytes).unwrap(), file);
    }

    #[test]
    fn candidate_wire_format_preserves_duplicates() {
        let records = vec![
            CandidateRecord {
                ordinal: 2,
                start: 9,
            },
            CandidateRecord {
                ordinal: 2,
                start: 10,
            },
        ];
        let file = CandidateFile::new(3, records.clone()).unwrap();
        let bytes = file.encode();
        assert_eq!(&bytes[..8], b"NTLMCAN1");
        assert_eq!(&bytes[16..24], &3u64.to_le_bytes());
        assert_eq!(&bytes[24..32], &2u64.to_le_bytes());
        assert_eq!(CandidateFile::decode(&bytes).unwrap().records, records);
    }

    #[test]
    fn rejects_corrupt_headers_lengths_and_ordinals() {
        let mut endpoint = EndpointFile::new(vec![1]).encode();
        endpoint[24] = 1;
        assert!(matches!(
            EndpointFile::decode(&endpoint),
            Err(FormatError::UnsupportedHeader { .. })
        ));
        let truncated = &EndpointFile::new(vec![1]).encode()[..39];
        assert!(matches!(
            EndpointFile::decode(truncated),
            Err(FormatError::LengthMismatch { .. })
        ));

        let mut candidate = CandidateFile {
            query_count: 1,
            records: vec![CandidateRecord {
                ordinal: 0,
                start: 4,
            }],
        }
        .encode();
        candidate[40..48].copy_from_slice(&1u64.to_le_bytes());
        assert!(matches!(
            CandidateFile::decode(&candidate),
            Err(FormatError::OrdinalOutOfRange)
        ));
        assert!(
            CandidateFile::new(
                1,
                vec![CandidateRecord {
                    ordinal: 1,
                    start: 0
                }]
            )
            .is_err()
        );
    }

    #[test]
    fn role_inference_uses_delimited_tokens_and_detects_conflicts() {
        assert_eq!(
            infer_role("des1-aabbccddeeff.endpoints").unwrap(),
            Some(Role::Des1)
        );
        assert_eq!(
            infer_role("DES2-aabbccddeeff.candidates").unwrap(),
            Some(Role::Des2)
        );
        assert_eq!(infer_role("backup-copy.endpoints").unwrap(), None);
        assert!(matches!(
            infer_role("des1-des2-aabbccddeeff.endpoints"),
            Err(FormatError::ConflictingRole(_))
        ));
    }

    #[test]
    fn pairs_are_strict_and_ordered() {
        let input = vec![
            PathBuf::from("des2-b.candidates"),
            PathBuf::from("des1-a.candidates"),
        ];
        let ordered = order_paths_by_role(&input).unwrap();
        assert_eq!(ordered[0].1, Some(Role::Des1));
        assert!(matches!(
            order_paths_by_role(&[PathBuf::from("a"), PathBuf::from("des2-b")]),
            Err(FormatError::MissingPairRoles)
        ));
        assert!(matches!(
            order_paths_by_role(&[PathBuf::from("des1-a"), PathBuf::from("des1-b")]),
            Err(FormatError::DuplicateRole(Role::Des1))
        ));
    }

    #[test]
    fn extracts_only_valid_shared_ids() {
        assert_eq!(
            run_id_from_filename("des1-A1B2C3D4E5F6.endpoints").as_deref(),
            Some("a1b2c3d4e5f6")
        );
        assert_eq!(run_id_from_filename("des1-short.endpoints"), None);
        assert_eq!(run_id_from_filename("a1b2c3d4e5f6.txt"), None);
    }

    #[test]
    fn role_has_distinct_human_and_filename_labels() {
        assert_eq!(Role::Des1.human_label(), "DES1");
        assert_eq!(Role::Des1.filename_prefix(), "des1");
        assert_eq!(Role::Des2.human_label(), "DES2");
        assert_eq!(Role::Des2.filename_prefix(), "des2");
    }
}
