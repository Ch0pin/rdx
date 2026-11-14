//! APK Signature Scheme v1 (JAR signing).

use std::collections::BTreeMap;

use ecdsa::signature::Signer as EcSignerTrait;
use p256::ecdsa::SigningKey as EcSigningKey;
use p256::pkcs8::DecodePrivateKey;
use rsa::pkcs1v15::SigningKey as RsaSigningKey;
use rsa::signature::{SignatureEncoding, Signer as RsaSignerTrait};
use sha2::Sha256;
use x509_parser::prelude::FromDer;

use super::{jar_manifest_digest, read_zip_entries, KeystoreMaterial, SignError, SignResult};

const MANIFEST_NAME: &str = "META-INF/MANIFEST.MF";
const SF_NAME: &str = "META-INF/APKPATCH.SF";

pub fn sign_v1(apk: &[u8], keystore: &KeystoreMaterial) -> SignResult<Vec<u8>> {
    let entries = read_zip_entries(apk)?;
    let manifest_mf = build_manifest_mf(&entries);
    let sf = build_signature_file(&manifest_mf);
    let (sig_name, sig_bytes) = sign_signature_file(sf.as_bytes(), keystore)?;

    let mut writer = crate::zip_write::ApkWriter::new();
    for (name, data) in &entries {
        if name.starts_with("META-INF/") {
            continue;
        }
        let compress = crate::zip_write::should_compress(name, &[]);
        writer.add_entry(name, data, compress);
    }
    writer.add_entry(MANIFEST_NAME, manifest_mf.as_bytes(), false);
    writer.add_entry(SF_NAME, sf.as_bytes(), false);
    writer.add_entry(&sig_name, &sig_bytes, false);
    writer
        .finish()
        .map_err(|e| SignError::Signing(e.to_string()))
}

fn build_manifest_mf(entries: &BTreeMap<String, Vec<u8>>) -> String {
    let mut out = String::from("Manifest-Version: 1.0\r\nCreated-By: apk-patch\r\n\r\n");
    for (name, data) in entries {
        if name.starts_with("META-INF/") {
            continue;
        }
        out.push_str(&format!("Name: {name}\r\n"));
        out.push_str(&jar_manifest_digest(data));
        out.push('\n');
    }
    out
}

fn build_signature_file(manifest_mf: &str) -> String {
    let mut out = String::from("Signature-Version: 1.0\r\nCreated-By: apk-patch\r\n\r\n");
    // Digest of the whole MANIFEST.MF (main attrs + all entries).
    out.push_str(&jar_manifest_digest(manifest_mf.as_bytes()));
    out.push('\n');
    out
}

fn sign_signature_file(sf: &[u8], keystore: &KeystoreMaterial) -> SignResult<(String, Vec<u8>)> {
    let signature = if keystore.is_ec {
        let key = EcSigningKey::from_pkcs8_der(&keystore.private_key_der)
            .map_err(|e| SignError::Signing(format!("EC key: {e}")))?;
        let sig: p256::ecdsa::Signature = EcSignerTrait::sign(&key, sf);
        // JAR ECDSA signatures are ASN.1 DER.
        sig.to_der().as_bytes().to_vec()
    } else {
        let private_key = rsa::pkcs8::DecodePrivateKey::from_pkcs8_der(&keystore.private_key_der)
            .map_err(|e| SignError::Signing(format!("RSA key: {e}")))?;
        let signing_key = RsaSigningKey::<Sha256>::new(private_key);
        RsaSignerTrait::sign(&signing_key, sf).to_bytes().to_vec()
    };

    let ext = if keystore.is_ec { "EC" } else { "RSA" };
    let sig_name = format!("META-INF/APKPATCH.{ext}");
    let pkcs7 = build_pkcs7(
        &keystore.certificate_der,
        &signature,
        keystore.is_ec,
    )?;
    Ok((sig_name, pkcs7))
}

fn build_pkcs7(cert_der: &[u8], signature: &[u8], is_ec: bool) -> SignResult<Vec<u8>> {
    let (_, cert) = x509_parser::certificate::X509Certificate::from_der(cert_der)
        .map_err(|e| SignError::Signing(format!("cert parse: {e:?}")))?;
    let issuer_der = cert.tbs_certificate.issuer.as_raw();
    let serial = cert.tbs_certificate.raw_serial();

    // SignedData
    let mut inner = Vec::new();
    // version
    inner.extend_from_slice(&[0x02, 0x01, 0x01]);
    // digestAlgorithms SET OF AlgorithmIdentifier (SHA-256)
    inner.extend_from_slice(&[
        0x31, 0x0d, 0x30, 0x0b, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x01,
    ]);
    // contentInfo: empty data OID
    inner.extend_from_slice(&[
        0x30, 0x0b, 0x06, 0x09, 0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x07, 0x01,
    ]);
    // certificates [0] IMPLICIT
    let mut certs = vec![0xa0];
    push_len(&mut certs, cert_der.len());
    certs.extend_from_slice(cert_der);
    inner.extend_from_slice(&certs);

    // SignerInfo
    let mut signer = Vec::new();
    signer.extend_from_slice(&[0x02, 0x01, 0x01]); // version
    // issuerAndSerialNumber
    let mut iasn = Vec::new();
    iasn.extend_from_slice(issuer_der);
    iasn.push(0x02);
    push_len(&mut iasn, serial.len());
    iasn.extend_from_slice(serial);
    signer.push(0x30);
    push_len(&mut signer, iasn.len());
    signer.extend_from_slice(&iasn);
    // digestAlgorithm SHA-256
    signer.extend_from_slice(&[
        0x30, 0x0b, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x01,
    ]);
    // signatureAlgorithm
    if is_ec {
        // ecdsa-with-SHA256
        signer.extend_from_slice(&[
            0x30, 0x0a, 0x06, 0x08, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x04, 0x03, 0x02,
        ]);
    } else {
        // sha256WithRSAEncryption
        signer.extend_from_slice(&[
            0x30, 0x0d, 0x06, 0x09, 0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x0b, 0x05,
            0x00,
        ]);
    }
    // signature OCTET STRING
    signer.push(0x04);
    push_len(&mut signer, signature.len());
    signer.extend_from_slice(signature);

    let mut signer_set = vec![0x31];
    push_len(&mut signer_set, signer.len());
    signer_set.extend_from_slice(&signer);
    inner.extend_from_slice(&signer_set);

    let mut signed_data = vec![0x30];
    push_len(&mut signed_data, inner.len());
    signed_data.extend_from_slice(&inner);

    let mut content_info = Vec::new();
    content_info.extend_from_slice(&[
        0x06, 0x09, 0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x07, 0x02,
    ]);
    let mut wrapped = vec![0xa0];
    push_len(&mut wrapped, signed_data.len());
    wrapped.extend_from_slice(&signed_data);
    content_info.extend_from_slice(&wrapped);

    let mut out = vec![0x30];
    push_len(&mut out, content_info.len());
    out.extend_from_slice(&content_info);
    Ok(out)
}

fn len_short(n: usize) -> Vec<u8> {
    if n <= 127 {
        vec![n as u8]
    } else if n <= 255 {
        vec![0x81, n as u8]
    } else if n <= 65535 {
        vec![0x82, (n >> 8) as u8, n as u8]
    } else {
        panic!("DER length > 65535");
    }
}

fn push_len(buf: &mut Vec<u8>, n: usize) {
    buf.extend_from_slice(&len_short(n));
}
