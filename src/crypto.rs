// // src/crypto.rs
// //
// // All encryption/decryption for Vitruvius.
// // Uses ChaCha20-Poly1305 — authenticated encryption.
// // If a single byte is tampered with, decryption returns an error.
// //
// // Wire format for encrypted files:
// //   [ 12 bytes nonce ][ ciphertext + 16 byte auth tag ]
// //
// // The nonce is randomly generated per encryption — so encrypting
// // the same file twice produces different ciphertext. This is correct
// // and expected.

// use anyhow::{anyhow, Result};
// use chacha20poly1305::{
//     aead::{Aead, KeyInit},
//     ChaCha20Poly1305, Nonce,
// };
// use rand::TryRngCore;
// use std::fs;
// use std::path::Path;

// // ─── Generate a new key file ──────────────────────────────────────────────────
// // Writes 32 random bytes to the given path.
// // Call once per group of peers, then copy the file to every machine.
// pub fn generate_key(path: &Path) -> Result<()> {
//     let mut key = [0u8; 32];
//     rand::rng().fill_bytes(&mut key).map_err(|e| anyhow!("RNG error: {e}"))?;
//     fs::write(path, &key)?;
//     println!("Key written to: {}", path.display());
//     println!("Copy this file to every peer. Keep it secret.");
//     Ok(())
// }

// // ─── Load an existing key file ────────────────────────────────────────────────
// // Reads exactly 32 bytes from the given path.
// // Returns an error if the file is the wrong size or unreadable.
// pub fn load_key(path: &Path) -> Result<[u8; 32]> {
//     let bytes = fs::read(path)?;
//     if bytes.len() != 32 {
//         return Err(anyhow!(
//             "Key file must be exactly 32 bytes, got {}. \
//              Did you generate it with --generate-key?",
//             bytes.len()
//         ));
//     }
//     let mut key = [0u8; 32];
//     key.copy_from_slice(&bytes);
//     Ok(key)
// }

// // ─── Encrypt ──────────────────────────────────────────────────────────────────
// // Returns: [ 12-byte random nonce || ciphertext+tag ]
// // The nonce is prepended so decrypt() can extract it without any extra state.
// pub fn encrypt(key: &[u8; 32], plaintext: &[u8]) -> Result<Vec<u8>> {
//     let cipher = ChaCha20Poly1305::new(key.into());

//     // Fresh random nonce every call — never reuse a nonce with the same key
//     let mut nonce_bytes = [0u8; 12];
//     let nonce = Nonce::from(nonce_bytes);
//     rand::rng().fill_bytes(&mut nonce_bytes).map_err(|e| anyhow!("RNG error: {e}"))?;
    

//     let ciphertext = cipher
//         .encrypt(&nonce, plaintext)
//         .map_err(|e| anyhow!("Encryption failed: {e}"))?;

//     // Prepend nonce so the receiver can decrypt without out-of-band state
//     let mut output = Vec::with_capacity(12 + ciphertext.len());
//     output.extend_from_slice(&nonce_bytes);
//     output.extend_from_slice(&ciphertext);
//     Ok(output)
// }

// // ─── Decrypt ──────────────────────────────────────────────────────────────────
// // Expects the format produced by encrypt(): [ 12-byte nonce || ciphertext+tag ]
// // Returns an error if the data was tampered with or the key is wrong.
// pub fn decrypt(key: &[u8; 32], data: &[u8]) -> Result<Vec<u8>> {
//     if data.len() < 12 {
//         return Err(anyhow!("Data too short to be a valid encrypted file"));
//     }

//     let (nonce_bytes, ciphertext) = data.split_at(12);
//     let nonce = Nonce::from_slice(nonce_bytes);
//     let cipher = ChaCha20Poly1305::new(key.into());

//     cipher
//         .decrypt(nonce, ciphertext)
//         .map_err(|_| anyhow!(
//             "Decryption failed — wrong key, or file was corrupted/tampered with"
//         ))
// }
