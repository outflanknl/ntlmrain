//! Parsing and validation for DES and NetNTLMv1 recovery targets.

use std::fmt;

use thiserror::Error;

use crate::FIXED_CHALLENGE_HEX;

const FIXED_CHALLENGE: [u8; 8] = [0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88];

/// A target accepted by the recovery pipeline.
#[derive(Clone, PartialEq, Eq)]
pub enum Target {
    /// One independently recoverable DES ciphertext.
    Des([u8; 8]),
    /// The DES1 and DES2 portions of a NetNTLMv1 response.
    TwoDes([u8; 16]),
    /// A complete 24-byte NetNTLMv1 response, including DES3.
    FullResponse([u8; 24]),
}

impl fmt::Debug for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Des(bytes) => f.debug_tuple("Des").field(&hex::encode(bytes)).finish(),
            Self::TwoDes(bytes) => f.debug_tuple("TwoDes").field(&hex::encode(bytes)).finish(),
            Self::FullResponse(bytes) => f
                .debug_tuple("FullResponse")
                .field(&hex::encode(bytes))
                .finish(),
        }
    }
}

impl Target {
    /// The ciphertexts covered by the rainbow table (DES1 and optionally DES2).
    pub fn des_targets(&self) -> Vec<[u8; 8]> {
        let bytes = self.as_bytes();
        let mut targets = vec![bytes[..8].try_into().expect("eight-byte slice")];
        if bytes.len() >= 16 {
            targets.push(bytes[8..16].try_into().expect("eight-byte slice"));
        }
        targets
    }

    pub fn as_bytes(&self) -> &[u8] {
        match self {
            Self::Des(bytes) => bytes,
            Self::TwoDes(bytes) => bytes,
            Self::FullResponse(bytes) => bytes,
        }
    }

    pub fn to_hex(&self) -> String {
        hex::encode_upper(self.as_bytes())
    }

    pub fn is_full_response(&self) -> bool {
        matches!(self, Self::FullResponse(_))
    }

    pub fn k3_ciphertext(&self) -> Option<[u8; 8]> {
        match self {
            Self::FullResponse(bytes) => Some(bytes[16..24].try_into().expect("eight-byte slice")),
            _ => None,
        }
    }
}

/// Identity fields retained when the input was a Responder/hashcat-style capture.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaptureMetadata {
    pub username: String,
    pub domain: String,
    pub lm_response: Option<[u8; 24]>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedTarget {
    pub target: Target,
    pub capture: Option<CaptureMetadata>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum InputError {
    #[error("target must be 16, 32, or 48 hexadecimal characters")]
    InvalidRawLength,
    #[error("target contains non-hexadecimal characters")]
    InvalidHex,
    #[error("invalid NetNTLMv1 capture; expected USER::DOMAIN:LM_RESPONSE:NT_RESPONSE:CHALLENGE")]
    InvalidCapture,
    #[error("NetNTLMv1 capture contains an invalid {field}")]
    InvalidCaptureField { field: &'static str },
    #[error("NTLMv1-ESS captures are not supported by this table")]
    NtlmV1Ess,
    #[error("capture challenge {actual} is unsupported; this table requires {FIXED_CHALLENGE_HEX}")]
    UnsupportedChallenge { actual: String },
}

/// Parse raw 16/32/48-character hexadecimal input or a conventional capture.
///
/// Supported capture layouts are `USER::DOMAIN:LM:NT:CHALLENGE` and the
/// LM-omitted `USER::DOMAIN:NT:CHALLENGE` form. Captures are accepted only for
/// the table's fixed server challenge, and the characteristic NTLMv1-ESS LM
/// response is rejected explicitly.
pub fn parse_target(input: &str) -> Result<ParsedTarget, InputError> {
    let trimmed = input.trim();
    if trimmed.contains("::") {
        parse_capture(trimmed)
    } else {
        parse_raw(trimmed)
    }
}

pub fn parse_raw(input: &str) -> Result<ParsedTarget, InputError> {
    let normalized: String = input
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect();
    if !matches!(normalized.len(), 16 | 32 | 48) {
        return Err(InputError::InvalidRawLength);
    }
    let decoded = decode_hex(&normalized).map_err(|_| InputError::InvalidHex)?;
    let target = match decoded.len() {
        8 => Target::Des(decoded.try_into().expect("validated length")),
        16 => Target::TwoDes(decoded.try_into().expect("validated length")),
        24 => Target::FullResponse(decoded.try_into().expect("validated length")),
        _ => unreachable!("validated length"),
    };
    Ok(ParsedTarget {
        target,
        capture: None,
    })
}

fn parse_capture(input: &str) -> Result<ParsedTarget, InputError> {
    let fields: Vec<&str> = input.split(':').collect();
    let (username, domain, lm_text, nt_text, challenge_text) = match fields.as_slice() {
        [username, "", domain, lm, nt, challenge] => {
            (*username, *domain, Some(*lm), *nt, *challenge)
        }
        [username, "", domain, nt, challenge] => (*username, *domain, None, *nt, *challenge),
        _ => return Err(InputError::InvalidCapture),
    };

    if username.is_empty() {
        return Err(InputError::InvalidCaptureField { field: "username" });
    }
    let nt_response: [u8; 24] = decode_exact(nt_text, "NT response")?;
    let challenge: [u8; 8] = decode_exact(challenge_text, "challenge")?;
    let lm_response = match lm_text {
        Some(text) if !text.is_empty() => Some(decode_exact(text, "LM response")?),
        _ => None,
    };

    // NTLMv1-ESS encodes an 8-byte client challenge followed by sixteen zero
    // bytes in the LM response. The NT response cannot be used with this table.
    if lm_response
        .as_ref()
        .is_some_and(|response| response[8..].iter().all(|byte| *byte == 0))
    {
        return Err(InputError::NtlmV1Ess);
    }
    if challenge != FIXED_CHALLENGE {
        return Err(InputError::UnsupportedChallenge {
            actual: hex::encode_upper(challenge),
        });
    }

    Ok(ParsedTarget {
        target: Target::FullResponse(nt_response),
        capture: Some(CaptureMetadata {
            username: username.to_owned(),
            domain: domain.to_owned(),
            lm_response,
        }),
    })
}

fn decode_exact<const N: usize>(input: &str, field: &'static str) -> Result<[u8; N], InputError> {
    if input.len() != N * 2 {
        return Err(InputError::InvalidCaptureField { field });
    }
    decode_hex(input)
        .and_then(|bytes| bytes.try_into().map_err(|_| ()))
        .map_err(|_| InputError::InvalidCaptureField { field })
}

fn decode_hex(input: &str) -> Result<Vec<u8>, ()> {
    if !input.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(());
    }
    hex::decode(input).map_err(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    const RESPONSE: &str = "727B4E35F947129EA52B9CDEDAE86934BB23EF89F50FC595";

    #[test]
    fn parses_all_raw_lengths_case_insensitively() {
        assert!(matches!(
            parse_target("727b4e35f947129e").unwrap().target,
            Target::Des(_)
        ));
        assert!(matches!(
            parse_target("727B4E35F947129EA52B9CDEDAE86934")
                .unwrap()
                .target,
            Target::TwoDes(_)
        ));
        let parsed = parse_target(RESPONSE).unwrap();
        assert!(matches!(parsed.target, Target::FullResponse(_)));
        assert_eq!(parsed.target.to_hex(), RESPONSE);
        assert_eq!(parsed.target.des_targets().len(), 2);
        assert_eq!(
            hex::encode_upper(parsed.target.k3_ciphertext().unwrap()),
            "BB23EF89F50FC595"
        );
    }

    #[test]
    fn raw_input_allows_surrounding_and_embedded_whitespace() {
        let parsed = parse_target("  727B4E35 F947129E\n").unwrap();
        assert_eq!(parsed.target.to_hex(), "727B4E35F947129E");
    }

    #[test]
    fn parses_responder_capture() {
        let lm = "AABBCCDDEEFF00112233445566778899AABBCCDDEEFF0011";
        let capture = format!("alice::DOMAIN:{lm}:{RESPONSE}:1122334455667788");
        let parsed = parse_target(&capture).unwrap();
        assert_eq!(parsed.target.to_hex(), RESPONSE);
        let metadata = parsed.capture.unwrap();
        assert_eq!(metadata.username, "alice");
        assert_eq!(metadata.domain, "DOMAIN");
        assert!(metadata.lm_response.is_some());
    }

    #[test]
    fn parses_capture_without_lm_response() {
        let capture = format!("alice::DOMAIN:{RESPONSE}:1122334455667788");
        let parsed = parse_target(&capture).unwrap();
        assert_eq!(parsed.capture.unwrap().lm_response, None);
    }

    #[test]
    fn rejects_arbitrary_challenge() {
        let capture = format!("alice::DOMAIN::{RESPONSE}:0102030405060708");
        assert_eq!(
            parse_target(&capture).unwrap_err(),
            InputError::UnsupportedChallenge {
                actual: "0102030405060708".into()
            }
        );
    }

    #[test]
    fn rejects_ntlmv1_ess_before_challenge_validation() {
        let lm = "010203040506070800000000000000000000000000000000";
        let capture = format!("alice::DOMAIN:{lm}:{RESPONSE}:0102030405060708");
        assert_eq!(parse_target(&capture).unwrap_err(), InputError::NtlmV1Ess);
    }

    #[test]
    fn rejects_invalid_shapes_and_hex() {
        assert_eq!(
            parse_target("abcd").unwrap_err(),
            InputError::InvalidRawLength
        );
        assert_eq!(
            parse_target("ZZZZZZZZZZZZZZZZ").unwrap_err(),
            InputError::InvalidHex
        );
        assert_eq!(
            parse_target("alice:DOMAIN:bad").unwrap_err(),
            InputError::InvalidHex
        );
        assert_eq!(
            parse_target("::DOMAIN::0011:1122").unwrap_err(),
            InputError::InvalidCaptureField { field: "username" }
        );
    }
}
