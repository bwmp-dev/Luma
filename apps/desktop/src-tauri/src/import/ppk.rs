//! PuTTY private key (`.ppk`) decoding.
//!
//! PuTTY stores keys in its own container rather than an OpenSSH one, and
//! `russh` cannot read it. Every `.ppk` Luma accepts is therefore converted to
//! OpenSSH format here, at the import boundary, and only the converted key is
//! stored. Nothing downstream — the keystore, the sync wire format, the temp
//! file `ssh::identity_material` materialises for `russh` — ever sees a PPK.
//!
//! Versions 2 and 3 are supported. Version 1 predates the MAC and is rejected.
//!
//! The private half of a `.ppk` is authenticated but not self-validating: a
//! file can carry a perfectly good MAC and still pair a private scalar with an
//! unrelated public key. `verify_self_consistent` closes that gap so a bad key
//! fails loudly at import instead of silently at connect time.

use argon2::{
    Algorithm as Argon2Flavour, Argon2, Params as Argon2Params, Version as Argon2Version,
};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use cbc::cipher::{BlockDecryptMut, KeyIvInit};
use hmac::{Hmac, Mac};
use rsa::traits::{PrivateKeyParts as _, PublicKeyParts as _};
use rsa::BigUint;
use serde::Serialize;
use sha1::Sha1;
use sha2::Sha256;
use ssh_key::{HashAlg, LineEnding, PrivateKey, PublicKey};
use zeroize::Zeroizing;

use crate::errors::{LumaError, Result};

/// Comfortably above any real key; a `.ppk` holding a 16 kbit RSA key is ~12 KiB.
pub(crate) const MAX_PPK_FILE_BYTES: usize = 1024 * 1024;
const MAX_BASE64_LINES: usize = 10_000;
const MAX_BASE64_LINE_LENGTH: usize = 128;
const MAX_BLOB_BYTES: usize = 256 * 1024;
const MAGIC: &[u8] = b"PuTTY-User-Key-File-";

/// Argon2 parameters come from the file, so they are attacker-controlled: a
/// hostile `.ppk` could otherwise ask us to allocate an arbitrary amount of
/// memory. These bounds are far above anything PuTTYgen emits (its default is
/// 8 MiB / 1 lane) and far below anything that would hurt.
const MAX_ARGON2_MEMORY_KIB: u32 = 1024 * 1024;
const MAX_ARGON2_PASSES: u32 = 100;
const MAX_ARGON2_PARALLELISM: u32 = 64;
const MIN_SALT_BYTES: usize = 8;
const MAX_SALT_BYTES: usize = 64;

const AES_BLOCK_BYTES: usize = 16;
/// Arbitrary; OpenSSH only requires the two copies to agree.
const CHECKINT: u32 = 0x4c55_4d41;

type Aes256CbcDec = cbc::Decryptor<aes::Aes256>;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

fn not_a_ppk() -> LumaError {
    LumaError::InvalidInput("this file is not a PuTTY private key (.ppk)".into())
}

fn malformed() -> LumaError {
    LumaError::InvalidInput("the PuTTY .ppk file is malformed and could not be read".into())
}

fn version_one() -> LumaError {
    LumaError::InvalidInput(
        "PuTTY .ppk version 1 keys are not supported; open the key in PuTTYgen and save it again to upgrade it"
            .into(),
    )
}

fn unsupported_version() -> LumaError {
    LumaError::InvalidInput(
        "unsupported PuTTY .ppk file version; this build supports versions 2 and 3".into(),
    )
}

fn needs_passphrase() -> LumaError {
    LumaError::InvalidInput("this PuTTY key is encrypted and requires a passphrase".into())
}

/// One message for encrypted keys whether the passphrase was wrong or the
/// ciphertext was tampered with — distinguishing them would hand an attacker a
/// passphrase oracle, and PuTTY itself does not distinguish either.
fn bad_passphrase() -> LumaError {
    LumaError::InvalidInput("could not decrypt the PuTTY key; the passphrase is incorrect".into())
}

fn integrity_failed() -> LumaError {
    LumaError::InvalidInput("the PuTTY key failed its integrity check; the file is corrupt".into())
}

fn unsupported_encryption() -> LumaError {
    LumaError::InvalidInput("unsupported PuTTY key encryption; only aes256-cbc is supported".into())
}

fn unsupported_kdf() -> LumaError {
    LumaError::InvalidInput("unsupported or unreasonable PuTTY key derivation parameters".into())
}

fn inconsistent_key() -> LumaError {
    LumaError::InvalidInput(
        "the .ppk private key does not match the public key stored in the same file; the file is corrupt or was written by an incompatible tool"
            .into(),
    )
}

fn inconsistent_signature() -> LumaError {
    LumaError::InvalidInput(
        "the .ppk private key failed a self-consistency check; it does not sign for its own public key"
            .into(),
    )
}

// ---------------------------------------------------------------------------
// SSH wire format helpers
// ---------------------------------------------------------------------------

struct Reader<'a> {
    data: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, offset: 0 }
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8]> {
        let end = self.offset.checked_add(length).ok_or_else(malformed)?;
        let slice = self.data.get(self.offset..end).ok_or_else(malformed)?;
        self.offset = end;
        Ok(slice)
    }

    fn u32(&mut self) -> Result<u32> {
        let bytes = self.take(4)?;
        Ok(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    /// A length-prefixed SSH string. `mpint` uses the same framing, so this
    /// serves both; the caller interprets the payload.
    fn string(&mut self) -> Result<&'a [u8]> {
        let length = self.u32()? as usize;
        self.take(length)
    }

    fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.offset)
    }
}

fn put_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_be_bytes());
}

fn put_string(out: &mut Vec<u8>, value: &[u8]) {
    put_u32(out, value.len() as u32);
    out.extend_from_slice(value);
}

fn trim_leading_zeros(value: &[u8]) -> &[u8] {
    let first = value.iter().position(|byte| *byte != 0);
    match first {
        Some(index) => &value[index..],
        None => &[],
    }
}

/// Encode a big-endian magnitude as an SSH `mpint`: minimal length, with a
/// leading zero byte when the top bit would otherwise read as a sign bit.
fn put_mpint(out: &mut Vec<u8>, magnitude: &[u8]) {
    let trimmed = trim_leading_zeros(magnitude);
    if trimmed.is_empty() {
        put_u32(out, 0);
        return;
    }
    let needs_pad = trimmed[0] & 0x80 != 0;
    put_u32(out, trimmed.len() as u32 + u32::from(needs_pad));
    if needs_pad {
        out.push(0);
    }
    out.extend_from_slice(trimmed);
}

fn decode_hex(value: &[u8]) -> Result<Vec<u8>> {
    if !value.len().is_multiple_of(2) || value.len() > 512 {
        return Err(malformed());
    }
    let digit = |byte: u8| -> Result<u8> {
        match byte {
            b'0'..=b'9' => Ok(byte - b'0'),
            b'a'..=b'f' => Ok(byte - b'a' + 10),
            b'A'..=b'F' => Ok(byte - b'A' + 10),
            _ => Err(malformed()),
        }
    };
    value
        .chunks_exact(2)
        .map(|pair| Ok(digit(pair[0])? << 4 | digit(pair[1])?))
        .collect()
}

fn ascii_string(value: &[u8]) -> Result<String> {
    let text = std::str::from_utf8(value).map_err(|_| malformed())?;
    if text.chars().any(|character| character.is_control()) {
        return Err(malformed());
    }
    Ok(text.to_string())
}

// ---------------------------------------------------------------------------
// Algorithms
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyAlgorithm {
    Rsa,
    Ed25519,
    Ecdsa { scalar_bytes: usize },
}

impl KeyAlgorithm {
    fn parse(name: &str) -> Result<Self> {
        Ok(match name {
            "ssh-rsa" => Self::Rsa,
            "ssh-ed25519" => Self::Ed25519,
            "ecdsa-sha2-nistp256" => Self::Ecdsa { scalar_bytes: 32 },
            "ecdsa-sha2-nistp384" => Self::Ecdsa { scalar_bytes: 48 },
            "ecdsa-sha2-nistp521" => Self::Ecdsa { scalar_bytes: 66 },
            "ssh-dss" => {
                return Err(LumaError::InvalidInput(
                    "DSA (ssh-dss) keys are not supported; generate a modern key instead".into(),
                ))
            }
            other => {
                return Err(LumaError::InvalidInput(format!(
                    "unsupported PuTTY key algorithm: {other}"
                )))
            }
        })
    }
}

// ---------------------------------------------------------------------------
// File parsing
// ---------------------------------------------------------------------------

struct Argon2Settings {
    flavour: Argon2Flavour,
    memory_kib: u32,
    passes: u32,
    parallelism: u32,
    salt: Vec<u8>,
}

struct PpkFile {
    version: u8,
    algorithm: String,
    encryption: String,
    comment: Vec<u8>,
    public_blob: Vec<u8>,
    private_blob: Zeroizing<Vec<u8>>,
    mac: Vec<u8>,
    argon2: Option<Argon2Settings>,
}

impl PpkFile {
    fn encrypted(&self) -> Result<bool> {
        match self.encryption.as_str() {
            "none" => Ok(false),
            "aes256-cbc" => Ok(true),
            _ => Err(unsupported_encryption()),
        }
    }
}

/// Split `Key: value`. PuTTY requires exactly one space after the colon, so we
/// do too — matching its reader matters because the comment we MAC has to be
/// byte-identical to the comment PuTTY MACed.
fn split_header(line: &[u8]) -> Result<(String, &[u8])> {
    let colon = line
        .iter()
        .position(|byte| *byte == b':')
        .ok_or_else(malformed)?;
    let key = ascii_string(&line[..colon])?;
    let value = line.get(colon + 1..).ok_or_else(malformed)?;
    let value = value.strip_prefix(b" ").ok_or_else(malformed)?;
    Ok((key, value))
}

fn read_blob(lines: &[&[u8]], index: &mut usize, count_value: &[u8]) -> Result<Vec<u8>> {
    let count: usize = ascii_string(count_value)?
        .parse()
        .map_err(|_| malformed())?;
    if count > MAX_BASE64_LINES {
        return Err(malformed());
    }
    let mut encoded = Vec::with_capacity(count * 64);
    for _ in 0..count {
        let line = lines.get(*index).ok_or_else(malformed)?;
        *index += 1;
        if line.len() > MAX_BASE64_LINE_LENGTH {
            return Err(malformed());
        }
        encoded.extend_from_slice(line);
    }
    let blob = BASE64.decode(&encoded).map_err(|_| malformed())?;
    if blob.len() > MAX_BLOB_BYTES {
        return Err(malformed());
    }
    Ok(blob)
}

fn parse_file(contents: &[u8]) -> Result<PpkFile> {
    if contents.len() > MAX_PPK_FILE_BYTES {
        return Err(malformed());
    }
    let lines: Vec<&[u8]> = contents
        .split(|byte| *byte == b'\n')
        .map(|line| line.strip_suffix(b"\r").unwrap_or(line))
        .collect();

    let first = lines.first().copied().ok_or_else(not_a_ppk)?;
    let rest = first.strip_prefix(MAGIC).ok_or_else(not_a_ppk)?;
    let colon = rest
        .iter()
        .position(|byte| *byte == b':')
        .ok_or_else(malformed)?;
    let version: u8 = ascii_string(&rest[..colon])?
        .parse()
        .map_err(|_| unsupported_version())?;
    match version {
        2 | 3 => {}
        1 => return Err(version_one()),
        _ => return Err(unsupported_version()),
    }
    let algorithm = ascii_string(
        rest.get(colon + 1..)
            .ok_or_else(malformed)?
            .strip_prefix(b" ")
            .ok_or_else(malformed)?,
    )?;

    let mut encryption = None;
    let mut comment = None;
    let mut public_blob = None;
    let mut private_blob = None;
    let mut mac = None;
    let mut flavour = None;
    let mut memory_kib = None;
    let mut passes = None;
    let mut parallelism = None;
    let mut salt = None;

    let mut index = 1usize;
    while let Some(line) = lines.get(index).copied() {
        index += 1;
        if line.is_empty() {
            continue;
        }
        let (key, value) = split_header(line)?;
        match key.as_str() {
            "Encryption" => encryption = Some(ascii_string(value)?),
            "Comment" => comment = Some(value.to_vec()),
            "Public-Lines" => public_blob = Some(read_blob(&lines, &mut index, value)?),
            "Private-Lines" => {
                private_blob = Some(Zeroizing::new(read_blob(&lines, &mut index, value)?))
            }
            "Private-MAC" => mac = Some(decode_hex(value)?),
            // Only PPK v1 has this, and the version check above already
            // rejected v1; keep the branch so a hand-mangled file still gets
            // the actionable message rather than "malformed".
            "Private-Hash" => return Err(version_one()),
            "Key-Derivation" => flavour = Some(ascii_string(value)?),
            "Argon2-Memory" => memory_kib = Some(parse_u32(value)?),
            "Argon2-Passes" => passes = Some(parse_u32(value)?),
            "Argon2-Parallelism" => parallelism = Some(parse_u32(value)?),
            "Argon2-Salt" => salt = Some(decode_hex(value)?),
            _ => {}
        }
    }

    let argon2 = match flavour {
        Some(flavour) => {
            let flavour = match flavour.as_str() {
                "Argon2id" => Argon2Flavour::Argon2id,
                "Argon2i" => Argon2Flavour::Argon2i,
                "Argon2d" => Argon2Flavour::Argon2d,
                _ => return Err(unsupported_kdf()),
            };
            let memory_kib = memory_kib.ok_or_else(unsupported_kdf)?;
            let passes = passes.ok_or_else(unsupported_kdf)?;
            let parallelism = parallelism.ok_or_else(unsupported_kdf)?;
            let salt = salt.ok_or_else(unsupported_kdf)?;
            if memory_kib == 0
                || memory_kib > MAX_ARGON2_MEMORY_KIB
                || passes == 0
                || passes > MAX_ARGON2_PASSES
                || parallelism == 0
                || parallelism > MAX_ARGON2_PARALLELISM
                || salt.len() < MIN_SALT_BYTES
                || salt.len() > MAX_SALT_BYTES
            {
                return Err(unsupported_kdf());
            }
            Some(Argon2Settings {
                flavour,
                memory_kib,
                passes,
                parallelism,
                salt,
            })
        }
        None => None,
    };

    Ok(PpkFile {
        version,
        algorithm,
        encryption: encryption.ok_or_else(malformed)?,
        comment: comment.unwrap_or_default(),
        public_blob: public_blob.ok_or_else(malformed)?,
        private_blob: private_blob.ok_or_else(malformed)?,
        mac: mac.ok_or_else(malformed)?,
        argon2,
    })
}

fn parse_u32(value: &[u8]) -> Result<u32> {
    ascii_string(value)?.parse().map_err(|_| unsupported_kdf())
}

// ---------------------------------------------------------------------------
// Key derivation
// ---------------------------------------------------------------------------

struct DerivedKeys {
    cipher_key: Zeroizing<[u8; 32]>,
    iv: [u8; AES_BLOCK_BYTES],
    mac_key: Zeroizing<Vec<u8>>,
}

fn derive_keys(file: &PpkFile, encrypted: bool, passphrase: &str) -> Result<DerivedKeys> {
    match file.version {
        2 => {
            // key = SHA1(u32be(0) || passphrase) || SHA1(u32be(1) || passphrase),
            // truncated to 32 bytes for AES-256. The IV is zero.
            use sha1::Digest as _;
            let mut cipher_key = Zeroizing::new([0u8; 32]);
            let mut block = Zeroizing::new(Vec::with_capacity(4 + passphrase.len()));
            for (counter, chunk) in [0u32, 1].into_iter().enumerate() {
                block.clear();
                block.extend_from_slice(&chunk.to_be_bytes());
                block.extend_from_slice(passphrase.as_bytes());
                let digest = Sha1::digest(&block[..]);
                let start = counter * 20;
                let end = (start + 20).min(32);
                cipher_key[start..end].copy_from_slice(&digest[..end - start]);
            }
            // The MAC key is derived even for unencrypted v2 keys, where the
            // passphrase contributes nothing.
            let mut mac_input = Zeroizing::new(Vec::with_capacity(30 + passphrase.len()));
            mac_input.extend_from_slice(b"putty-private-key-file-mac-key");
            mac_input.extend_from_slice(passphrase.as_bytes());
            let mac_key = Zeroizing::new(Sha1::digest(&mac_input[..]).to_vec());
            Ok(DerivedKeys {
                cipher_key,
                iv: [0u8; AES_BLOCK_BYTES],
                mac_key,
            })
        }
        3 => {
            if !encrypted {
                // v3 derives everything from Argon2, which is not run at all
                // for an unencrypted key, so the MAC key is empty.
                return Ok(DerivedKeys {
                    cipher_key: Zeroizing::new([0u8; 32]),
                    iv: [0u8; AES_BLOCK_BYTES],
                    mac_key: Zeroizing::new(Vec::new()),
                });
            }
            let settings = file.argon2.as_ref().ok_or_else(unsupported_kdf)?;
            let params = Argon2Params::new(
                settings.memory_kib,
                settings.passes,
                settings.parallelism,
                Some(80),
            )
            .map_err(|_| unsupported_kdf())?;
            let argon2 = Argon2::new(settings.flavour, Argon2Version::V0x13, params);
            let mut output = Zeroizing::new([0u8; 80]);
            argon2
                .hash_password_into(passphrase.as_bytes(), &settings.salt, &mut output[..])
                .map_err(|_| unsupported_kdf())?;
            let mut cipher_key = Zeroizing::new([0u8; 32]);
            cipher_key.copy_from_slice(&output[..32]);
            let mut iv = [0u8; AES_BLOCK_BYTES];
            iv.copy_from_slice(&output[32..48]);
            Ok(DerivedKeys {
                cipher_key,
                iv,
                mac_key: Zeroizing::new(output[48..80].to_vec()),
            })
        }
        _ => Err(unsupported_version()),
    }
}

/// The MAC covers five length-prefixed strings, the last of which is the
/// *decrypted* private blob including its padding.
fn mac_input(file: &PpkFile, private_blob: &[u8]) -> Zeroizing<Vec<u8>> {
    let mut data = Zeroizing::new(Vec::with_capacity(
        file.algorithm.len()
            + file.comment.len()
            + file.public_blob.len()
            + private_blob.len()
            + 32,
    ));
    put_string(&mut data, file.algorithm.as_bytes());
    put_string(&mut data, file.encryption.as_bytes());
    put_string(&mut data, &file.comment);
    put_string(&mut data, &file.public_blob);
    put_string(&mut data, private_blob);
    data
}

fn verify_mac(file: &PpkFile, mac_key: &[u8], private_blob: &[u8], encrypted: bool) -> Result<()> {
    let data = mac_input(file, private_blob);
    let matches = match file.version {
        2 => Hmac::<Sha1>::new_from_slice(mac_key)
            .map_err(|_| malformed())
            .map(|mut mac| {
                mac.update(&data);
                mac.verify_slice(&file.mac).is_ok()
            })?,
        _ => Hmac::<Sha256>::new_from_slice(mac_key)
            .map_err(|_| malformed())
            .map(|mut mac| {
                mac.update(&data);
                mac.verify_slice(&file.mac).is_ok()
            })?,
    };
    if matches {
        return Ok(());
    }
    // For an encrypted key a MAC failure almost always means the passphrase was
    // wrong; for an unencrypted one it can only mean corruption.
    Err(if encrypted {
        bad_passphrase()
    } else {
        integrity_failed()
    })
}

fn decrypt_in_place(cipher_key: &[u8; 32], iv: &[u8; AES_BLOCK_BYTES], buffer: &mut [u8]) {
    let mut decryptor = Aes256CbcDec::new(cipher_key.into(), iv.into());
    for chunk in buffer.chunks_exact_mut(AES_BLOCK_BYTES) {
        decryptor.decrypt_block_mut(chunk.into());
    }
}

// ---------------------------------------------------------------------------
// Conversion to OpenSSH
// ---------------------------------------------------------------------------

/// Assemble an `openssh-key-v1` container from the PPK's own public blob plus
/// the decoded private fields, then let `ssh-key` decode and validate it. Going
/// through the wire format keeps one code path for every algorithm instead of
/// three sets of `ssh-key` internal types.
fn build_openssh_container(
    algorithm: KeyAlgorithm,
    public_blob: &[u8],
    private_blob: &[u8],
    comment: &str,
) -> Result<Zeroizing<Vec<u8>>> {
    let mut fields = Zeroizing::new(Vec::<u8>::with_capacity(
        public_blob.len() + private_blob.len() + 64,
    ));
    let mut public = Reader::new(public_blob);
    let mut private = Reader::new(private_blob);
    let algorithm_name = public.string()?;

    match algorithm {
        KeyAlgorithm::Rsa => {
            // Public blob orders the modulus pair (e, n); the OpenSSH private
            // section orders it (n, e). Do not copy one into the other.
            let e = public.string()?;
            let n = public.string()?;
            let d = private.string()?;
            let p = private.string()?;
            let q = private.string()?;
            // PuTTY's own iqmp is read past and discarded: we recompute it from
            // p and q so it is guaranteed consistent with the p/q ordering we
            // emit, whichever way PuTTY happened to label the two primes.
            let _iqmp = private.string()?;

            let key = rsa::RsaPrivateKey::from_components(
                BigUint::from_bytes_be(n),
                BigUint::from_bytes_be(e),
                BigUint::from_bytes_be(d),
                vec![BigUint::from_bytes_be(p), BigUint::from_bytes_be(q)],
            )
            // from_components always validates: it proves the primes multiply
            // to n and that d is the true inverse of e. That is the whole
            // cryptographic self-check for RSA.
            .map_err(|_| inconsistent_key())?;
            let iqmp = key.crt_coefficient().ok_or_else(inconsistent_key)?;
            let primes = key.primes();
            let (p, q) = (
                primes.first().ok_or_else(inconsistent_key)?,
                primes.get(1).ok_or_else(inconsistent_key)?,
            );

            put_string(&mut fields, b"ssh-rsa");
            put_mpint(&mut fields, &key.n().to_bytes_be());
            put_mpint(&mut fields, &key.e().to_bytes_be());
            put_mpint(&mut fields, &key.d().to_bytes_be());
            put_mpint(&mut fields, &iqmp.to_bytes_be());
            put_mpint(&mut fields, &p.to_bytes_be());
            put_mpint(&mut fields, &q.to_bytes_be());
        }
        KeyAlgorithm::Ed25519 => {
            let point = public.string()?;
            if point.len() != 32 {
                return Err(malformed());
            }
            // PuTTY reads the seed as a little-endian integer, so a seed whose
            // most significant (last) byte is zero comes back short. Pad on the
            // right to restore the 32-byte seed.
            let seed_bytes = private.string()?;
            if seed_bytes.len() > 32 {
                return Err(malformed());
            }
            let mut seed = Zeroizing::new([0u8; 32]);
            seed[..seed_bytes.len()].copy_from_slice(seed_bytes);

            let mut combined = Zeroizing::new(Vec::<u8>::with_capacity(64));
            combined.extend_from_slice(&seed[..]);
            combined.extend_from_slice(point);

            put_string(&mut fields, b"ssh-ed25519");
            put_string(&mut fields, point);
            put_string(&mut fields, &combined);
        }
        KeyAlgorithm::Ecdsa { scalar_bytes } => {
            let curve = public.string()?;
            let point = public.string()?;
            let scalar = private.string()?;
            let trimmed = trim_leading_zeros(scalar);
            if trimmed.len() > scalar_bytes {
                return Err(malformed());
            }
            // ssh-key decodes the ECDSA scalar into a fixed-size array and
            // rejects anything shorter, so a trimmed mpint must be left-padded
            // back to the full field width.
            let mut padded = Zeroizing::new(vec![0u8; scalar_bytes]);
            padded[scalar_bytes - trimmed.len()..].copy_from_slice(trimmed);

            put_string(&mut fields, algorithm_name);
            put_string(&mut fields, curve);
            put_string(&mut fields, point);
            if padded[0] & 0x80 != 0 {
                put_u32(&mut fields, scalar_bytes as u32 + 1);
                fields.push(0);
            } else {
                put_u32(&mut fields, scalar_bytes as u32);
            }
            fields.extend_from_slice(&padded);
        }
    }

    // Trailing bytes are the random padding PuTTY adds before encrypting; it is
    // covered by the MAC, so anything left over is expected but bounded.
    if private.remaining() >= AES_BLOCK_BYTES {
        return Err(malformed());
    }

    let mut private_section =
        Zeroizing::new(Vec::<u8>::with_capacity(fields.len() + comment.len() + 24));
    put_u32(&mut private_section, CHECKINT);
    put_u32(&mut private_section, CHECKINT);
    private_section.extend_from_slice(&fields);
    put_string(&mut private_section, comment.as_bytes());
    let mut pad: u8 = 1;
    while !private_section.len().is_multiple_of(8) {
        private_section.push(pad);
        pad += 1;
    }

    let mut container = Zeroizing::new(Vec::<u8>::with_capacity(
        private_section.len() + public_blob.len() + 64,
    ));
    container.extend_from_slice(b"openssh-key-v1\0");
    put_string(&mut container, b"none");
    put_string(&mut container, b"none");
    put_string(&mut container, b"");
    put_u32(&mut container, 1);
    put_string(&mut container, public_blob);
    put_string(&mut container, &private_section);
    Ok(container)
}

/// Prove the private half really belongs to the public half.
///
/// A `.ppk` with a valid MAC can still pair a private scalar with an unrelated
/// public key — the MAC only says the file was not tampered with after PuTTY
/// wrote it, not that its contents are coherent. Each algorithm gets a genuine
/// cryptographic tie:
///
/// * RSA — `RsaPrivateKey::from_components` in `build_openssh_container`
///   already proved `n == p * q` and `d * e == 1 (mod lambda(n))`, and `n` came
///   from the public blob. That is the tie, so there is nothing more to do
///   here. It also cannot be done here: `ssh-key` 0.6.7 builds an
///   `rsa::RsaPrivateKey` from an `RsaKeypair` by passing `p` twice instead of
///   `p, q` (`private/rsa.rs`), so `sign` fails on every valid RSA key.
/// * Ed25519 — `PrivateKey::from_bytes` derives the public point from the seed
///   and rejects a mismatch, and the round trip below re-confirms it.
/// * ECDSA — nothing validates the scalar against the point, so the sign and
///   verify round trip below is the only thing standing between a subtly
///   mis-decoded key and a confusing failure at connect time.
fn verify_self_consistent(key: &PrivateKey, algorithm: KeyAlgorithm) -> Result<()> {
    if algorithm == KeyAlgorithm::Rsa {
        return Ok(());
    }
    const NAMESPACE: &str = "luma-ppk-import";
    const MESSAGE: &[u8] = b"luma ppk self check";
    let signature = key
        .sign(NAMESPACE, HashAlg::Sha256, MESSAGE)
        .map_err(|_| inconsistent_signature())?;
    key.public_key()
        .verify(NAMESPACE, MESSAGE, &signature)
        .map_err(|_| inconsistent_signature())
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Header metadata, readable without a passphrase.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PpkInfo {
    pub version: u8,
    pub algorithm: String,
    pub comment: String,
    pub encrypted: bool,
    pub public_key: String,
    pub fingerprint: String,
}

pub struct ConvertedPpk {
    pub key: PrivateKey,
    pub algorithm: String,
    pub comment: String,
    pub public_key: String,
    pub fingerprint: String,
}

/// Hand-written so a decrypted key can never reach a log or a panic message,
/// mirroring `SshConnectionConfig` in `ssh::mod`.
impl std::fmt::Debug for ConvertedPpk {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ConvertedPpk")
            .field("algorithm", &self.algorithm)
            .field("comment", &self.comment)
            .field("fingerprint", &self.fingerprint)
            .finish_non_exhaustive()
    }
}

/// Cheap sniff used to tell a `.ppk` from an OpenSSH key without parsing.
pub fn is_ppk(contents: &[u8]) -> bool {
    contents.starts_with(MAGIC)
}

fn describe_public(public_blob: &[u8]) -> Result<(String, String)> {
    let mut public = PublicKey::from_bytes(public_blob).map_err(|_| malformed())?;
    public.set_comment("");
    let fingerprint = public.fingerprint(HashAlg::Sha256).to_string();
    let encoded = public.to_openssh().map_err(|_| malformed())?;
    Ok((encoded, fingerprint))
}

/// Read a `.ppk`'s headers without decrypting it, so the UI can show what the
/// key is and ask for a passphrase only when one is actually needed.
pub fn inspect(contents: &[u8]) -> Result<PpkInfo> {
    let file = parse_file(contents)?;
    KeyAlgorithm::parse(&file.algorithm)?;
    let encrypted = file.encrypted()?;
    let (public_key, fingerprint) = describe_public(&file.public_blob)?;
    Ok(PpkInfo {
        version: file.version,
        algorithm: file.algorithm,
        comment: String::from_utf8_lossy(&file.comment).into_owned(),
        encrypted,
        public_key,
        fingerprint,
    })
}

/// Decrypt, authenticate, and convert a `.ppk` into an `ssh-key` private key.
///
/// This runs Argon2 for encrypted v3 keys, so callers should invoke it from
/// `spawn_blocking` rather than on an async worker.
pub fn convert(contents: &[u8], passphrase: Option<&str>) -> Result<ConvertedPpk> {
    let file = parse_file(contents)?;
    let algorithm = KeyAlgorithm::parse(&file.algorithm)?;
    let encrypted = file.encrypted()?;
    let passphrase = passphrase.unwrap_or_default();
    if encrypted && passphrase.is_empty() {
        return Err(needs_passphrase());
    }

    let keys = derive_keys(&file, encrypted, passphrase)?;
    let mut private_blob = file.private_blob.clone();
    if encrypted {
        if private_blob.is_empty() || !private_blob.len().is_multiple_of(AES_BLOCK_BYTES) {
            return Err(malformed());
        }
        decrypt_in_place(&keys.cipher_key, &keys.iv, &mut private_blob);
    }
    // Authenticate before letting the field parser touch these bytes.
    verify_mac(&file, &keys.mac_key, &private_blob, encrypted)?;

    let comment = String::from_utf8_lossy(&file.comment).into_owned();
    let container = build_openssh_container(algorithm, &file.public_blob, &private_blob, &comment)?;
    let key = PrivateKey::from_bytes(&container).map_err(|_| inconsistent_key())?;
    verify_self_consistent(&key, algorithm)?;

    let (public_key, fingerprint) = describe_public(&file.public_blob)?;
    Ok(ConvertedPpk {
        key,
        algorithm: file.algorithm,
        comment,
        public_key,
        fingerprint,
    })
}

/// Encode a converted key as OpenSSH text, optionally re-applying a passphrase
/// so the key stays encrypted in the keystore and in the temp file the SSH
/// layer materialises for `russh`.
pub fn to_openssh(key: &PrivateKey, encrypt_with: Option<&str>) -> Result<Zeroizing<String>> {
    let encoded = match encrypt_with.filter(|passphrase| !passphrase.is_empty()) {
        Some(passphrase) => {
            let mut rng = rand::rngs::OsRng;
            key.encrypt(&mut rng, passphrase.as_bytes())
                .map_err(|_| {
                    LumaError::InvalidInput("could not encrypt the converted PuTTY key".into())
                })?
                .to_openssh(LineEnding::LF)
        }
        None => key.to_openssh(LineEnding::LF),
    };
    encoded.map_err(|_| {
        LumaError::InvalidInput("could not encode the converted PuTTY key as OpenSSH".into())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fixtures are throwaway keys generated by real `puttygen`; see
    /// `tests/fixtures/ppk/generate.sh` for the exact commands. The oracle is
    /// deliberately not our own code: OpenSSH's `ssh-keygen` made the key,
    /// PuTTY converted it, and these tests assert that converting it back
    /// yields the key `ssh-keygen` started with.
    const PASSPHRASE: &str = "luma test passphrase";

    macro_rules! fixture {
        ($name:literal) => {
            (
                include_bytes!(concat!("../../tests/fixtures/ppk/", $name, ".ppk")) as &[u8],
                include_str!(concat!("../../tests/fixtures/ppk/", $name, ".openssh")),
                include_str!(concat!("../../tests/fixtures/ppk/", $name, ".pub")),
            )
        };
    }

    struct Fixture {
        name: &'static str,
        ppk: &'static [u8],
        openssh: &'static str,
        public: &'static str,
        passphrase: Option<&'static str>,
    }

    fn fixtures() -> Vec<Fixture> {
        let mut all = Vec::new();
        let mut push = |name, parts: (&'static [u8], &'static str, &'static str), passphrase| {
            all.push(Fixture {
                name,
                ppk: parts.0,
                openssh: parts.1,
                public: parts.2,
                passphrase,
            });
        };
        push("ed25519_v2_plain", fixture!("ed25519_v2_plain"), None);
        push(
            "ed25519_v2_encrypted",
            fixture!("ed25519_v2_encrypted"),
            Some(PASSPHRASE),
        );
        push("ed25519_v3_plain", fixture!("ed25519_v3_plain"), None);
        push(
            "ed25519_v3_encrypted",
            fixture!("ed25519_v3_encrypted"),
            Some(PASSPHRASE),
        );
        push(
            "rsa2048_v2_encrypted",
            fixture!("rsa2048_v2_encrypted"),
            Some(PASSPHRASE),
        );
        push("rsa2048_v3_plain", fixture!("rsa2048_v3_plain"), None);
        push(
            "ecdsa_p256_v3_encrypted",
            fixture!("ecdsa_p256_v3_encrypted"),
            Some(PASSPHRASE),
        );
        push("ecdsa_p521_v2_plain", fixture!("ecdsa_p521_v2_plain"), None);
        all
    }

    /// `ssh-keygen` writes `<alg> <base64> <comment>`; we render without a comment.
    fn public_without_comment(line: &str) -> String {
        line.split_whitespace()
            .take(2)
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn to_hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    fn wrap_base64(blob: &[u8]) -> (usize, String) {
        let encoded = BASE64.encode(blob);
        let lines: Vec<String> = encoded
            .as_bytes()
            .chunks(64)
            .map(|chunk| String::from_utf8_lossy(chunk).into_owned())
            .collect();
        (lines.len(), lines.join("\n"))
    }

    /// Re-render an *unencrypted* PPK from its parts with a freshly computed,
    /// valid MAC. Negative tests use this to build files that are perfectly
    /// well-formed and correctly authenticated but internally inconsistent —
    /// the only way to prove `verify_self_consistent` actually fires.
    fn reencode_plain(
        version: u8,
        algorithm: &str,
        comment: &str,
        public_blob: &[u8],
        private_blob: &[u8],
    ) -> Vec<u8> {
        let file = PpkFile {
            version,
            algorithm: algorithm.to_string(),
            encryption: "none".to_string(),
            comment: comment.as_bytes().to_vec(),
            public_blob: public_blob.to_vec(),
            private_blob: Zeroizing::new(private_blob.to_vec()),
            mac: Vec::new(),
            argon2: None,
        };
        let keys = derive_keys(&file, false, "").expect("derive keys");
        let data = mac_input(&file, private_blob);
        let mac = match version {
            2 => {
                let mut mac = Hmac::<Sha1>::new_from_slice(&keys.mac_key).unwrap();
                mac.update(&data);
                to_hex(&mac.finalize().into_bytes())
            }
            _ => {
                let mut mac = Hmac::<Sha256>::new_from_slice(&keys.mac_key).unwrap();
                mac.update(&data);
                to_hex(&mac.finalize().into_bytes())
            }
        };
        let (public_lines, public_text) = wrap_base64(public_blob);
        let (private_lines, private_text) = wrap_base64(private_blob);
        format!(
            "PuTTY-User-Key-File-{version}: {algorithm}\n\
             Encryption: none\n\
             Comment: {comment}\n\
             Public-Lines: {public_lines}\n{public_text}\n\
             Private-Lines: {private_lines}\n{private_text}\n\
             Private-MAC: {mac}\n"
        )
        .into_bytes()
    }

    /// Decrypted parts of a fixture, for tests that recombine them.
    fn parts_of(ppk: &[u8], passphrase: Option<&str>) -> (String, String, Vec<u8>, Vec<u8>) {
        let file = parse_file(ppk).expect("parse");
        let encrypted = file.encrypted().expect("encryption");
        let keys = derive_keys(&file, encrypted, passphrase.unwrap_or_default()).expect("keys");
        let mut private_blob = file.private_blob.clone();
        if encrypted {
            decrypt_in_place(&keys.cipher_key, &keys.iv, &mut private_blob);
        }
        (
            file.algorithm.clone(),
            String::from_utf8_lossy(&file.comment).into_owned(),
            file.public_blob.clone(),
            private_blob.to_vec(),
        )
    }

    #[test]
    fn converts_puttygen_fixtures_back_to_the_original_openssh_key() {
        for fixture in fixtures() {
            let converted = convert(fixture.ppk, fixture.passphrase)
                .unwrap_or_else(|error| panic!("{}: {error}", fixture.name));
            let expected = PrivateKey::from_openssh(fixture.openssh)
                .unwrap_or_else(|error| panic!("{}: {error}", fixture.name));

            assert_eq!(
                converted.key.key_data(),
                expected.key_data(),
                "{}: private key material differs from the ssh-keygen original",
                fixture.name
            );
            assert_eq!(
                converted.public_key,
                public_without_comment(fixture.public),
                "{}: public key differs",
                fixture.name
            );
            assert_eq!(
                converted.fingerprint,
                expected
                    .public_key()
                    .fingerprint(HashAlg::Sha256)
                    .to_string(),
                "{}: fingerprint differs",
                fixture.name
            );
        }
    }

    #[test]
    fn converted_keys_round_trip_through_openssh_encoding() {
        for fixture in fixtures() {
            let converted = convert(fixture.ppk, fixture.passphrase).expect(fixture.name);

            let plain = to_openssh(&converted.key, None).expect(fixture.name);
            let reparsed = PrivateKey::from_openssh(plain.as_str()).expect(fixture.name);
            assert_eq!(
                reparsed.key_data(),
                converted.key.key_data(),
                "{}",
                fixture.name
            );

            let sealed = to_openssh(&converted.key, Some("re-encrypted")).expect(fixture.name);
            let sealed_key = PrivateKey::from_openssh(sealed.as_str()).expect(fixture.name);
            assert!(sealed_key.is_encrypted(), "{}", fixture.name);
            let opened = sealed_key.decrypt("re-encrypted").expect(fixture.name);
            assert_eq!(
                opened.key_data(),
                converted.key.key_data(),
                "{}",
                fixture.name
            );
        }
    }

    #[test]
    fn inspect_reads_metadata_without_a_passphrase() {
        let info = inspect(include_bytes!(
            "../../tests/fixtures/ppk/ecdsa_p256_v3_encrypted.ppk"
        ))
        .expect("inspect");
        assert_eq!(info.version, 3);
        assert_eq!(info.algorithm, "ecdsa-sha2-nistp256");
        assert!(info.encrypted);
        assert_eq!(info.comment, "luma-fixture-ecdsa_p256_v3_encrypted");
        assert_eq!(
            info.public_key,
            public_without_comment(include_str!(
                "../../tests/fixtures/ppk/ecdsa_p256_v3_encrypted.pub"
            ))
        );

        let plain = inspect(include_bytes!(
            "../../tests/fixtures/ppk/ed25519_v2_plain.ppk"
        ))
        .expect("inspect");
        assert_eq!(plain.version, 2);
        assert!(!plain.encrypted);
    }

    #[test]
    fn reports_missing_and_incorrect_passphrases() {
        let ppk = include_bytes!("../../tests/fixtures/ppk/ed25519_v3_encrypted.ppk");

        let missing = convert(ppk, None).unwrap_err();
        assert_eq!(
            missing.to_string(),
            "invalid input: this PuTTY key is encrypted and requires a passphrase"
        );

        let wrong = convert(ppk, Some("not the passphrase")).unwrap_err();
        assert_eq!(
            wrong.to_string(),
            "invalid input: could not decrypt the PuTTY key; the passphrase is incorrect"
        );

        // A v2 key takes the SHA-1 KDF path, so cover it too.
        let wrong_v2 = convert(
            include_bytes!("../../tests/fixtures/ppk/ed25519_v2_encrypted.ppk"),
            Some("not the passphrase"),
        )
        .unwrap_err();
        assert_eq!(
            wrong_v2.to_string(),
            "invalid input: could not decrypt the PuTTY key; the passphrase is incorrect"
        );
    }

    #[test]
    fn rejects_a_ppk_whose_public_blob_belongs_to_a_different_key() {
        let (algorithm, comment, public_blob, _) = parts_of(
            include_bytes!("../../tests/fixtures/ppk/ed25519_v3_plain.ppk"),
            None,
        );
        let (_, _, _, other_private) = parts_of(
            include_bytes!("../../tests/fixtures/ppk/ed25519_v2_plain.ppk"),
            None,
        );

        // Correctly MACed, well-formed, and completely incoherent.
        let forged = reencode_plain(3, &algorithm, &comment, &public_blob, &other_private);
        let error = convert(&forged, None).unwrap_err();
        assert_eq!(
            error.to_string(),
            "invalid input: the .ppk private key does not match the public key stored in the same file; the file is corrupt or was written by an incompatible tool"
        );
    }

    #[test]
    fn rejects_a_ppk_with_a_corrupted_private_scalar() {
        // ECDSA is the case with no structural protection at all: nothing but
        // the signature round trip ties the scalar to the point.
        let (algorithm, comment, public_blob, mut private_blob) = parts_of(
            include_bytes!("../../tests/fixtures/ppk/ecdsa_p521_v2_plain.ppk"),
            None,
        );
        let last = private_blob.len() - 1;
        private_blob[last] ^= 0x01;
        let corrupted = reencode_plain(2, &algorithm, &comment, &public_blob, &private_blob);
        let error = convert(&corrupted, None).unwrap_err();
        assert_eq!(
            error.to_string(),
            "invalid input: the .ppk private key failed a self-consistency check; it does not sign for its own public key"
        );

        // RSA is caught earlier, by the component validation.
        let (algorithm, comment, public_blob, mut private_blob) = parts_of(
            include_bytes!("../../tests/fixtures/ppk/rsa2048_v3_plain.ppk"),
            None,
        );
        private_blob[8] ^= 0x01;
        let corrupted = reencode_plain(3, &algorithm, &comment, &public_blob, &private_blob);
        let error = convert(&corrupted, None).unwrap_err();
        assert_eq!(
            error.to_string(),
            "invalid input: the .ppk private key does not match the public key stored in the same file; the file is corrupt or was written by an incompatible tool"
        );
    }

    #[test]
    fn ed25519_seed_with_a_trailing_zero_byte_is_accepted() {
        use ed25519_dalek::SigningKey;
        use rand::RngCore as _;

        // PuTTY stores the ed25519 seed as a little-endian integer, so a seed
        // whose most significant byte is zero is written one byte short. Roughly
        // one key in 256 looks like this.
        let mut seed = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut seed);
        seed[31] = 0;
        let point = SigningKey::from_bytes(&seed).verifying_key().to_bytes();

        let mut public_blob = Vec::new();
        put_string(&mut public_blob, b"ssh-ed25519");
        put_string(&mut public_blob, &point);
        let mut private_blob = Vec::new();
        put_string(&mut private_blob, &seed[..31]);

        let ppk = reencode_plain(3, "ssh-ed25519", "trimmed", &public_blob, &private_blob);
        let converted = convert(&ppk, None).expect("trimmed seed should be accepted");
        let (expected_public, _) = describe_public(&public_blob).unwrap();
        assert_eq!(converted.public_key, expected_public);
    }

    #[test]
    fn rejects_unsupported_versions_and_algorithms() {
        let plain = include_str!("../../tests/fixtures/ppk/ed25519_v2_plain.ppk");

        let v1 = plain.replace("PuTTY-User-Key-File-2", "PuTTY-User-Key-File-1");
        assert_eq!(
            convert(v1.as_bytes(), None).unwrap_err().to_string(),
            "invalid input: PuTTY .ppk version 1 keys are not supported; open the key in PuTTYgen and save it again to upgrade it"
        );

        let v9 = plain.replace("PuTTY-User-Key-File-2", "PuTTY-User-Key-File-9");
        assert_eq!(
            convert(v9.as_bytes(), None).unwrap_err().to_string(),
            "invalid input: unsupported PuTTY .ppk file version; this build supports versions 2 and 3"
        );

        let dss = plain.replace("ssh-ed25519", "ssh-dss");
        assert_eq!(
            convert(dss.as_bytes(), None).unwrap_err().to_string(),
            "invalid input: DSA (ssh-dss) keys are not supported; generate a modern key instead"
        );

        let unknown = plain.replace(
            "PuTTY-User-Key-File-2: ssh-ed25519",
            "PuTTY-User-Key-File-2: ssh-frobnicate",
        );
        assert_eq!(
            convert(unknown.as_bytes(), None).unwrap_err().to_string(),
            "invalid input: unsupported PuTTY key algorithm: ssh-frobnicate"
        );

        assert_eq!(
            convert(b"not a key at all", None).unwrap_err().to_string(),
            "invalid input: this file is not a PuTTY private key (.ppk)"
        );
        assert!(!is_ppk(b"-----BEGIN OPENSSH PRIVATE KEY-----"));
        assert!(is_ppk(plain.as_bytes()));
    }

    #[test]
    fn rejects_unreasonable_argon2_parameters() {
        let encrypted = include_str!("../../tests/fixtures/ppk/ed25519_v3_encrypted.ppk");
        // 8 GiB: refused while parsing headers, long before anything allocates.
        let greedy = encrypted.replace("Argon2-Memory: 8192", "Argon2-Memory: 8388608");
        assert_eq!(
            convert(greedy.as_bytes(), Some(PASSPHRASE))
                .unwrap_err()
                .to_string(),
            "invalid input: unsupported or unreasonable PuTTY key derivation parameters"
        );

        let unknown_kdf = encrypted.replace("Key-Derivation: Argon2id", "Key-Derivation: scrypt");
        assert_eq!(
            convert(unknown_kdf.as_bytes(), Some(PASSPHRASE))
                .unwrap_err()
                .to_string(),
            "invalid input: unsupported or unreasonable PuTTY key derivation parameters"
        );
    }

    /// `.gitattributes` pins the fixtures to LF, but a stray checkout setting or
    /// an editor can still hand a test CRLF. Every test that rewrites fixture
    /// text starts from a known baseline rather than trusting what is on disk.
    fn lf(text: &str) -> String {
        text.replace("\r\n", "\n")
    }

    #[test]
    fn rejects_unsupported_encryption_and_corrupt_files() {
        let plain = lf(include_str!(
            "../../tests/fixtures/ppk/ed25519_v2_plain.ppk"
        ));

        let blowfish = plain.replace("Encryption: none", "Encryption: blowfish-cbc");
        assert_eq!(
            convert(blowfish.as_bytes(), None).unwrap_err().to_string(),
            "invalid input: unsupported PuTTY key encryption; only aes256-cbc is supported"
        );

        // Flip a MAC digit on an unencrypted key: that can only be corruption.
        let mac_line = plain
            .lines()
            .find(|line| line.starts_with("Private-MAC: "))
            .unwrap();
        let digits = &mac_line["Private-MAC: ".len()..];
        // Swap the final digit for a different one; appending a fixed digit
        // would silently do nothing whenever the MAC already ended in it.
        let last = digits.chars().next_back().unwrap();
        let replacement = if last == '0' { '1' } else { '0' };
        let broken_mac = format!("Private-MAC: {}{replacement}", &digits[..digits.len() - 1]);
        let corrupt = plain.replace(mac_line, &broken_mac);
        assert_ne!(corrupt, plain, "the MAC must actually have changed");
        assert_eq!(
            convert(corrupt.as_bytes(), None).unwrap_err().to_string(),
            "invalid input: the PuTTY key failed its integrity check; the file is corrupt"
        );

        let truncated = plain.replace("Private-Lines: 1\n", "Private-Lines: 9\n");
        assert_ne!(
            truncated, plain,
            "the line count must actually have changed"
        );
        assert_eq!(
            convert(truncated.as_bytes(), None).unwrap_err().to_string(),
            "invalid input: the PuTTY .ppk file is malformed and could not be read"
        );
    }

    #[test]
    fn accepts_crlf_line_endings() {
        // PuTTY on Windows writes CRLF; the parser has to see through that or
        // every key saved on Windows fails its MAC.
        let plain = lf(include_str!(
            "../../tests/fixtures/ppk/ed25519_v3_plain.ppk"
        ));
        let crlf = plain.replace('\n', "\r\n");
        assert!(!crlf.contains("\r\r"), "the fixture must start out as LF");
        let converted = convert(crlf.as_bytes(), None).expect("CRLF should parse");
        let expected = PrivateKey::from_openssh(include_str!(
            "../../tests/fixtures/ppk/ed25519_v3_plain.openssh"
        ))
        .unwrap();
        assert_eq!(converted.key.key_data(), expected.key_data());
    }
}
