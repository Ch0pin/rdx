//! APK V1 / JAR signature: META-INF/*.RSA with .SF, PKCS7/CMS parsing.

use regex::Regex;

use crate::error::Result;

pub fn get_signature_names(zip_names: &[String]) -> Vec<String> {
    let re = Regex::new(r"^META-INF/.+\.(DSA|EC|RSA)$").unwrap();
    let mut signatures = Vec::new();
    for name in zip_names {
        if re.is_match(name) {
            let base = name.rsplitn(2, '.').nth(1).unwrap_or(name);
            let sf_name = format!("{}.SF", base);
            if zip_names.contains(&sf_name) {
                signatures.push(name.clone());
            }
        }
    }
    signatures
}

#[cfg(feature = "sign")]
pub fn extract_first_certificate_from_pkcs7(pkcs7_bytes: &[u8]) -> Result<Option<Vec<u8>>> {
    use der::Decode;
    use der::Encode;
    use der::SliceReader;
    use pkcs7::ContentInfo;
    use x509_parser::prelude::FromDer;

    let mut reader = SliceReader::new(pkcs7_bytes)
        .map_err(|e| crate::error::Error::Parse(format!("PKCS7 slice reader: {}", e)))?;
    let content_info = ContentInfo::decode(&mut reader)
        .map_err(|e| crate::error::Error::Parse(format!("PKCS7 parse failed: {}", e)))?;
    let signed_data = match content_info {
        ContentInfo::SignedData(sd) => sd,
        _ => return Ok(None),
    };
    let certificates = match &signed_data.certificates {
        Some(certs) => certs,
        None => return Ok(None),
    };
    use pkcs7::certificate_choices::CertificateChoices;
    let signer_infos = &signed_data.signer_infos;
    let first_signer = signer_infos.get(0);
    let (issuer_der_opt, serial_opt) = match first_signer {
        Some(s) => {
            use pkcs7::signer_info::SignerIdentifier;
            match &s.sid {
                SignerIdentifier::IssuerAndSerialNumber(ias) => {
                    let issuer_der = ias
                        .name
                        .to_der()
                        .map_err(|e| crate::error::Error::Parse(format!("issuer to_der: {}", e)))?;
                    let serial = ias.serial_number.as_bytes();
                    (Some(issuer_der), Some(serial.to_vec()))
                }
                _ => (None, None),
            }
        }
        None => (None, None),
    };
    for cert_choice in certificates.iter() {
        let cert_der: Vec<u8> = match cert_choice {
            CertificateChoices::Certificate(c) => c
                .to_der()
                .map_err(|e| crate::error::Error::Parse(format!("cert to_der: {}", e)))?,
            _ => continue,
        };
        if let (Some(ref issuer_der), Some(ref serial)) =
            (issuer_der_opt.as_ref(), serial_opt.as_ref())
        {
            if let Ok((_, cert)) =
                x509_parser::prelude::X509Certificate::from_der(cert_der.as_slice())
            {
                let cert_serial_raw: &[u8] = cert.tbs_certificate.raw_serial();
                let cert_issuer_raw: &[u8] = cert.tbs_certificate.issuer.as_raw();
                if cert_issuer_raw == issuer_der.as_slice() && cert_serial_raw == serial.as_slice()
                {
                    return Ok(Some(cert_der));
                }
            }
        } else {
            return Ok(Some(cert_der));
        }
    }
    if let Some(cert_choice) = certificates.get(0) {
        if let CertificateChoices::Certificate(c) = cert_choice {
            return Ok(Some(c.to_der().map_err(|e| {
                crate::error::Error::Parse(format!("cert to_der: {}", e))
            })?));
        }
    }
    Ok(None)
}

#[cfg(not(feature = "sign"))]
pub fn extract_first_certificate_from_pkcs7(_pkcs7_bytes: &[u8]) -> Result<Option<Vec<u8>>> {
    Err(crate::error::Error::Unsupported(
        "v1 PKCS7 certificate extraction requires the `sign` feature".into(),
    ))
}

pub fn get_certificate_der_from_pkcs7(
    pkcs7_bytes: &[u8],
    _sf_bytes: &[u8],
) -> Result<Option<Vec<u8>>> {
    extract_first_certificate_from_pkcs7(pkcs7_bytes)
}
