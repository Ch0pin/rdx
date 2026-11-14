//! APK Signature Scheme v2/v3 block writer.

use ecdsa::signature::Signer as EcSignerTrait;
use p256::ecdsa::SigningKey as EcSigningKey;
use p256::pkcs8::DecodePrivateKey;
use rsa::pkcs1v15::SigningKey as RsaSigningKey;
use rsa::signature::{SignatureEncoding, Signer as RsaSignerTrait};
use sha2::{Digest, Sha256};

use super::{central_dir_offset, find_eocd, KeystoreMaterial, SignError, SignResult};

const APK_SIG_MAGIC: &[u8; 16] = b"APK Sig Block 42";
const APK_SIG_KEY_V2: u32 = 0x7109_871a;
const APK_SIG_KEY_V3: u32 = 0xf053_68c0;
const CHUNK_SIZE: usize = 1024 * 1024;
const SIG_RSA_SHA256: u32 = 0x0103;
const SIG_ECDSA_SHA256: u32 = 0x0201;
/// APK Signature Scheme v3 requires API 24+ algorithm coverage; signer minSdk
/// must not be below the chosen signature algorithm's minSdk (else apksigner
/// reports "No supported signatures").
const V3_MIN_SDK: u32 = 24;
const V3_MAX_SDK: u32 = 0x7fff_ffff;

/// Sign with APK Signature Scheme v2 and/or v3 in a single signing block.
pub fn sign_v2_v3(apk: &[u8], keystore: &KeystoreMaterial, v2: bool, v3: bool) -> SignResult<Vec<u8>> {
    if !v2 && !v3 {
        return Ok(apk.to_vec());
    }
    let cd_offset = central_dir_offset(apk)?;
    let eocd_offset =
        find_eocd(apk).ok_or_else(|| SignError::InvalidApk("EOCD not found".into()))?;
    if eocd_offset < cd_offset {
        return Err(SignError::InvalidApk("EOCD before central directory".into()));
    }

    let before_cd = &apk[..cd_offset];
    let central_dir = &apk[cd_offset..eocd_offset];
    let mut eocd = apk[eocd_offset..].to_vec();
    // EOCD central-directory offset must point at the future signing-block offset
    // (i.e. the current CD offset) while hashing.
    if eocd.len() >= 20 {
        eocd[16..20].copy_from_slice(&(cd_offset as u32).to_le_bytes());
    }

    let content_digest = compute_content_digest(&[before_cd, central_dir, &eocd]);
    let sig_alg = if keystore.is_ec {
        SIG_ECDSA_SHA256
    } else {
        SIG_RSA_SHA256
    };

    let mut pairs = Vec::new();
    if v2 {
        let signed_data = build_v2_signed_data(sig_alg, &content_digest, &keystore.certificate_der)?;
        let signature = sign_signed_data(&signed_data, keystore)?;
        let signer = build_v2_signer(&signed_data, sig_alg, &signature, &keystore.certificate_der)?;
        pairs.extend_from_slice(&build_pair(
            APK_SIG_KEY_V2,
            &length_prefixed(&length_prefixed_sequence(&[signer])),
        ));
    }
    if v3 {
        let signed_data = build_v3_signed_data(
            sig_alg,
            &content_digest,
            &keystore.certificate_der,
            V3_MIN_SDK,
            V3_MAX_SDK,
        )?;
        let signature = sign_signed_data(&signed_data, keystore)?;
        let signer = build_v3_signer(
            &signed_data,
            V3_MIN_SDK,
            V3_MAX_SDK,
            sig_alg,
            &signature,
            &keystore.certificate_der,
        )?;
        pairs.extend_from_slice(&build_pair(
            APK_SIG_KEY_V3,
            &length_prefixed(&length_prefixed_sequence(&[signer])),
        ));
    }

    let signing_block = build_apk_signing_block(&pairs);
    insert_signing_block(apk, cd_offset, &signing_block)
}

/// Backward-compatible wrapper: one scheme only.
pub fn sign_v2(apk: &[u8], keystore: &KeystoreMaterial, v3: bool) -> SignResult<Vec<u8>> {
    if v3 {
        sign_v2_v3(apk, keystore, false, true)
    } else {
        sign_v2_v3(apk, keystore, true, false)
    }
}

fn compute_content_digest(sections: &[&[u8]]) -> [u8; 32] {
    let mut chunk_digests: Vec<u8> = Vec::new();
    let mut chunk_count: u32 = 0;
    for section in sections {
        if section.is_empty() {
            continue;
        }
        for chunk in section.chunks(CHUNK_SIZE) {
            chunk_digests.extend_from_slice(&hash_chunk(chunk));
            chunk_count += 1;
        }
    }
    if chunk_count == 0 {
        chunk_digests.extend_from_slice(&hash_chunk(&[]));
        chunk_count = 1;
    }
    let mut h = Sha256::new();
    h.update([0x5a]);
    h.update(chunk_count.to_le_bytes());
    h.update(&chunk_digests);
    h.finalize().into()
}

fn hash_chunk(chunk: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update([0xa5]);
    h.update((chunk.len() as u32).to_le_bytes());
    h.update(chunk);
    h.finalize().into()
}

fn build_v2_signed_data(
    sig_alg: u32,
    content_digest: &[u8; 32],
    cert_der: &[u8],
) -> SignResult<Vec<u8>> {
    let digests = length_prefixed_sequence(&[digest_entry(sig_alg, content_digest)]);
    let certs = length_prefixed_sequence(&[cert_der.to_vec()]);
    let mut out = Vec::new();
    out.extend_from_slice(&length_prefixed(&digests));
    out.extend_from_slice(&length_prefixed(&certs));
    out.extend_from_slice(&0u32.to_le_bytes()); // additional attributes
    Ok(out)
}

fn build_v3_signed_data(
    sig_alg: u32,
    content_digest: &[u8; 32],
    cert_der: &[u8],
    min_sdk: u32,
    max_sdk: u32,
) -> SignResult<Vec<u8>> {
    let digests = length_prefixed_sequence(&[digest_entry(sig_alg, content_digest)]);
    let certs = length_prefixed_sequence(&[cert_der.to_vec()]);
    let mut out = Vec::new();
    out.extend_from_slice(&length_prefixed(&digests));
    out.extend_from_slice(&length_prefixed(&certs));
    out.extend_from_slice(&min_sdk.to_le_bytes());
    out.extend_from_slice(&max_sdk.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // additional attributes
    Ok(out)
}

fn digest_entry(sig_alg: u32, digest: &[u8; 32]) -> Vec<u8> {
    let mut entry = Vec::new();
    entry.extend_from_slice(&sig_alg.to_le_bytes());
    entry.extend_from_slice(&length_prefixed(digest));
    entry
}

fn signature_entry(sig_alg: u32, signature: &[u8]) -> Vec<u8> {
    let mut entry = Vec::new();
    entry.extend_from_slice(&sig_alg.to_le_bytes());
    entry.extend_from_slice(&length_prefixed(signature));
    entry
}

fn build_v2_signer(
    signed_data: &[u8],
    sig_alg: u32,
    signature: &[u8],
    cert_der: &[u8],
) -> SignResult<Vec<u8>> {
    let public_key = extract_public_key_der(cert_der)?;
    let sigs = length_prefixed_sequence(&[signature_entry(sig_alg, signature)]);
    let mut out = Vec::new();
    out.extend_from_slice(&length_prefixed(signed_data));
    out.extend_from_slice(&length_prefixed(&sigs));
    out.extend_from_slice(&length_prefixed(&public_key));
    Ok(out)
}

fn build_v3_signer(
    signed_data: &[u8],
    min_sdk: u32,
    max_sdk: u32,
    sig_alg: u32,
    signature: &[u8],
    cert_der: &[u8],
) -> SignResult<Vec<u8>> {
    let public_key = extract_public_key_der(cert_der)?;
    let sigs = length_prefixed_sequence(&[signature_entry(sig_alg, signature)]);
    let mut out = Vec::new();
    out.extend_from_slice(&length_prefixed(signed_data));
    out.extend_from_slice(&min_sdk.to_le_bytes());
    out.extend_from_slice(&max_sdk.to_le_bytes());
    out.extend_from_slice(&length_prefixed(&sigs));
    out.extend_from_slice(&length_prefixed(&public_key));
    Ok(out)
}

fn sign_signed_data(signed_data: &[u8], keystore: &KeystoreMaterial) -> SignResult<Vec<u8>> {
    if keystore.is_ec {
        let key = EcSigningKey::from_pkcs8_der(&keystore.private_key_der)
            .map_err(|e| SignError::Signing(format!("EC key: {e}")))?;
        let sig: p256::ecdsa::Signature = EcSignerTrait::sign(&key, signed_data);
        // APK Signature Scheme requires ECDSA signatures in ASN.1 DER form.
        Ok(sig.to_der().as_bytes().to_vec())
    } else {
        let private_key = rsa::pkcs8::DecodePrivateKey::from_pkcs8_der(&keystore.private_key_der)
            .map_err(|e| SignError::Signing(format!("RSA key: {e}")))?;
        let signing_key = RsaSigningKey::<Sha256>::new(private_key);
        Ok(RsaSignerTrait::sign(&signing_key, signed_data)
            .to_bytes()
            .to_vec())
    }
}

fn extract_public_key_der(cert_der: &[u8]) -> SignResult<Vec<u8>> {
    use x509_parser::prelude::FromDer;
    let (_, cert) = x509_parser::certificate::X509Certificate::from_der(cert_der)
        .map_err(|e| SignError::Signing(format!("cert parse: {e:?}")))?;
    Ok(cert.public_key().raw.to_vec())
}

fn length_prefixed(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + data.len());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(data);
    out
}

/// length-prefixed sequence of length-prefixed elements.
fn length_prefixed_sequence(elements: &[Vec<u8>]) -> Vec<u8> {
    let mut body = Vec::new();
    for el in elements {
        body.extend_from_slice(&length_prefixed(el));
    }
    body
}

fn build_pair(id: u32, value: &[u8]) -> Vec<u8> {
    // pair = uint64(len(id + value)) || id || value
    let len = (value.len() as u64) + 4;
    let mut out = Vec::new();
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(&id.to_le_bytes());
    out.extend_from_slice(value);
    out
}

fn build_apk_signing_block(pairs: &[u8]) -> Vec<u8> {
    // size || pairs || size || magic
    let block_size = (pairs.len() + 8 + 16) as u64;
    let mut block = Vec::new();
    block.extend_from_slice(&block_size.to_le_bytes());
    block.extend_from_slice(pairs);
    block.extend_from_slice(&block_size.to_le_bytes());
    block.extend_from_slice(APK_SIG_MAGIC);
    block
}

fn insert_signing_block(apk: &[u8], cd_offset: usize, block: &[u8]) -> SignResult<Vec<u8>> {
    let mut out = Vec::with_capacity(apk.len() + block.len());
    out.extend_from_slice(&apk[..cd_offset]);
    out.extend_from_slice(block);
    out.extend_from_slice(&apk[cd_offset..]);
    patch_eocd_offset(&mut out, cd_offset + block.len())?;
    Ok(out)
}

fn patch_eocd_offset(apk: &mut [u8], new_cd_offset: usize) -> SignResult<()> {
    let eocd = find_eocd(apk).ok_or_else(|| SignError::InvalidApk("EOCD not found".into()))?;
    let off = (new_cd_offset as u32).to_le_bytes();
    apk[eocd + 16..eocd + 20].copy_from_slice(&off);
    Ok(())
}
