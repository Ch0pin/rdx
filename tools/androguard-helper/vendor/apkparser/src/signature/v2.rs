//! APK V2 Signing Block parsing.

use std::io::Cursor;

use crate::error::{BrokenAPKError, Result};
use crate::signature::utils::{parse_signatures_or_digests, read_uint32_le};

#[derive(Debug, Clone)]
pub struct ApkV2SignedData {
    pub bytes: Vec<u8>,
    pub digests: Vec<(u32, Vec<u8>)>,
    pub certificates: Vec<Vec<u8>>,
    pub additional_attributes: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct ApkV2Signer {
    pub bytes: Vec<u8>,
    pub signed_data: ApkV2SignedData,
    pub signatures: Vec<(u32, Vec<u8>)>,
    pub public_key: Vec<u8>,
}

pub fn parse_v2_signing_block(block_bytes: &[u8]) -> Result<Vec<ApkV2Signer>> {
    let mut block = Cursor::new(block_bytes);
    let size_sequence = read_uint32_le(&mut block)?;
    if (size_sequence as usize) + 4 != block_bytes.len() {
        return Err(
            BrokenAPKError("size of sequence and blocksize does not match".to_string()).into(),
        );
    }
    let mut signers = Vec::new();
    while (block.position() as usize) < block_bytes.len() {
        let off_signer = block.position() as usize;
        let size_signer = read_uint32_le(&mut block)? as usize;
        let len_signed_data = read_uint32_le(&mut block)? as usize;
        let start_sd = block.position() as usize;
        if start_sd + len_signed_data > block_bytes.len() {
            break;
        }
        let signed_data_bytes = block_bytes[start_sd..start_sd + len_signed_data].to_vec();
        block.set_position((start_sd + len_signed_data) as u64);

        let mut signed_data_cursor = Cursor::new(&signed_data_bytes);
        let len_digests = read_uint32_le(&mut signed_data_cursor)? as usize;
        let start_digests = signed_data_cursor.position() as usize;
        if start_digests + len_digests > signed_data_bytes.len() {
            break;
        }
        let raw_digests = &signed_data_bytes[start_digests..start_digests + len_digests];
        let digests = parse_signatures_or_digests(raw_digests)?;
        signed_data_cursor.set_position((start_digests + len_digests) as u64);

        let len_certs = read_uint32_le(&mut signed_data_cursor)? as usize;
        let start_certs = signed_data_cursor.position() as usize;
        let mut certs = Vec::new();
        let mut cert_pos = start_certs;
        signed_data_cursor.set_position(start_certs as u64);
        while cert_pos < start_certs + len_certs && cert_pos + 4 <= signed_data_bytes.len() {
            let len_cert =
                read_uint32_le(&mut Cursor::new(&signed_data_bytes[cert_pos..]))? as usize;
            cert_pos += 4;
            if cert_pos + len_cert > signed_data_bytes.len() {
                break;
            }
            certs.push(signed_data_bytes[cert_pos..cert_pos + len_cert].to_vec());
            cert_pos += len_cert;
        }
        signed_data_cursor.set_position(cert_pos as u64);

        let len_attr = read_uint32_le(&mut signed_data_cursor)? as usize;
        let attr_start = signed_data_cursor.position() as usize;
        let attributes = if attr_start + len_attr <= signed_data_bytes.len() {
            signed_data_bytes[attr_start..attr_start + len_attr].to_vec()
        } else {
            Vec::new()
        };

        let signed_data_object = ApkV2SignedData {
            bytes: signed_data_bytes,
            digests,
            certificates: certs,
            additional_attributes: attributes,
        };

        let len_sigs = read_uint32_le(&mut block)? as usize;
        let start_sigs = block.position() as usize;
        if start_sigs + len_sigs > block_bytes.len() {
            break;
        }
        let raw_sigs = &block_bytes[start_sigs..start_sigs + len_sigs];
        let sigs = parse_signatures_or_digests(raw_sigs)?;
        block.set_position((start_sigs + len_sigs) as u64);

        let len_publickey = read_uint32_le(&mut block)? as usize;
        let start_pk = block.position() as usize;
        if start_pk + len_publickey > block_bytes.len() {
            break;
        }
        let publickey = block_bytes[start_pk..start_pk + len_publickey].to_vec();
        block.set_position((start_pk + len_publickey) as u64);

        let signer_bytes = if off_signer + size_signer <= block_bytes.len() {
            block_bytes[off_signer..off_signer + size_signer].to_vec()
        } else {
            block_bytes[off_signer..].to_vec()
        };

        signers.push(ApkV2Signer {
            bytes: signer_bytes,
            signed_data: signed_data_object,
            signatures: sigs,
            public_key: publickey,
        });
    }
    Ok(signers)
}
