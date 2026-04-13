// src/crypto.rs
//
// All encryption/decryption for Vitruvius.
// Uses ChaCha20-Poly1305 — authenticated encryption.
// If a single byte is tampered with, decryption returns an error.
//
// Wire format for encrypted chunks:
//   [ 12 bytes nonce ][ ciphertext + 16 byte auth tag ]
//
// The nonce is randomly generated per chunk — so encrypting
// the same chunk twice produces different ciphertext. This is correct
// and expected. Never reuse a nonce with the same key.
//
// Key distribution model:
//   Generate once with --generate-key, copy the 32-byte file to every peer.
//   All peers must share the exact same key file.
// Commit by Riad

use anyhow::{anyhow, Result};
use chacha20poly1305::{
    aead::{Aead, KeyInit},
    ChaCha20Poly1305, Nonce,
};
use rand::TryRng;
use std::fs;
use std::path::Path;

// ─── Generate a new key file ──────────────────────────────────────────────────
// Writes 32 random bytes to the given path.
// Call once per sync group, then copy the file to every peer machine.
pub fn generate_key(path: &Path) -> Result<()> {
    let mut key = [0u8; 32];
    rand::rng()
        .try_fill_bytes(&mut key)
        .map_err(|e| anyhow!("RNG error: {e}"))?;
    fs::write(path, &key)?;
    println!("Key written to: {}", path.display());
    println!("Copy this file to every peer. Keep it secret.");
    Ok(())
}

// ─── Load an existing key file ────────────────────────────────────────────────
// Reads exactly 32 bytes from the given path.
// Returns an error if the file is the wrong size or unreadable.
pub fn load_key(path: &Path) -> Result<[u8; 32]> {
    let bytes = fs::read(path)?;
    if bytes.len() != 32 {
        return Err(anyhow!(
            "Key file must be exactly 32 bytes, got {}. \
             Did you generate it with --generate-key?",
            bytes.len()
        ));
    }
    let mut key = [0u8; 32];
    key.copy_from_slice(&bytes);
    Ok(key)
}

// ─── Encrypt ──────────────────────────────────────────────────────────────────
// Returns: [ 12-byte random nonce || ciphertext+tag ]
// The nonce is prepended so decrypt() can extract it without any extra state.
pub fn encrypt(key: &[u8; 32], plaintext: &[u8]) -> Result<Vec<u8>> {
    // Generate random nonce FIRST, then build the Nonce value from it.
    // IMPORTANT: nonce must be randomised before use, not after.
    let mut nonce_bytes = [0u8; 12];
    rand::rng()
        .try_fill_bytes(&mut nonce_bytes)
        .map_err(|e| anyhow!("RNG error: {e}"))?;

    let cipher = ChaCha20Poly1305::new(key.into());
    let nonce = Nonce::from(nonce_bytes);

    let ciphertext = cipher
        .encrypt(&nonce, plaintext)
        .map_err(|e| anyhow!("Encryption failed: {e}"))?;

    // Prepend nonce so the receiver can decrypt without out-of-band state.
    // Layout: [ 12 nonce bytes ][ ciphertext ][ 16 byte Poly1305 tag ]
    let mut output = Vec::with_capacity(12 + ciphertext.len());
    output.extend_from_slice(&nonce_bytes);
    output.extend_from_slice(&ciphertext);
    Ok(output)
}

// ─── Decrypt ──────────────────────────────────────────────────────────────────
// Expects the format produced by encrypt(): [ 12-byte nonce || ciphertext+tag ]
// Returns an error if the data was tampered with or the key is wrong.
// The Poly1305 authentication tag ensures integrity — no separate hash needed.
pub fn decrypt(key: &[u8; 32], data: &[u8]) -> Result<Vec<u8>> {
    if data.len() < 12 + 16 {
        // 12 nonce + 16 tag minimum — anything smaller cannot be valid
        return Err(anyhow!(
            "Data too short ({} bytes) to be a valid encrypted chunk",
            data.len()
        ));
    }

    let (nonce_bytes, ciphertext) = data.split_at(12);
    let nonce = Nonce::from_slice(nonce_bytes);
    let cipher = ChaCha20Poly1305::new(key.into());

    cipher
        .decrypt(nonce, ciphertext)
        .map_err(|_| anyhow!("Decryption failed — wrong key, or data was corrupted/tampered with"))
}

// ─── Tests ────────────────────────────────────────────────────────────────────
#[cfg(test)]
mod tests {
    use super::*;

    fn test_key() -> [u8; 32] {
        // deterministic key for tests only — never use in production
        [0xAB; 32]
    }

    #[test]
    fn round_trip_small() {
        let key = test_key();
        let plaintext = b"hello vitruvius";
        let encrypted = encrypt(&key, plaintext).unwrap();
        let decrypted = decrypt(&key, &encrypted).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn round_trip_empty() {
        let key = test_key();
        let encrypted = encrypt(&key, b"").unwrap();
        let decrypted = decrypt(&key, &encrypted).unwrap();
        assert_eq!(decrypted, b"");
    }

    #[test]
    fn nonces_are_unique() {
        // Encrypting the same plaintext twice must produce different ciphertext
        // (because nonces are random). If the old nonce bug were present, these
        // would be identical.
        let key = test_key();
        let a = encrypt(&key, b"same input").unwrap();
        let b = encrypt(&key, b"same input").unwrap();
        assert_ne!(a, b, "nonce reuse detected — nonces must be random");
    }

    #[test]
    fn tamper_detection() {
        let key = test_key();
        let mut encrypted = encrypt(&key, b"secret data").unwrap();
        // flip one byte in the ciphertext area
        let last = encrypted.len() - 1;
        encrypted[last] ^= 0xFF;
        assert!(
            decrypt(&key, &encrypted).is_err(),
            "tampered data should fail"
        );
    }

    #[test]
    fn wrong_key_fails() {
        let key_a = [0xAAu8; 32];
        let key_b = [0xBBu8; 32];
        let encrypted = encrypt(&key_a, b"secret").unwrap();
        assert!(decrypt(&key_b, &encrypted).is_err());
    }

    #[test]
    fn short_data_fails() {
        let key = test_key();
        assert!(decrypt(&key, &[0u8; 10]).is_err());
    }
}
