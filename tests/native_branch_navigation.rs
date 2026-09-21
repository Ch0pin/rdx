//! End-to-end native DEX loading and navigation through a reconstructed branch.
use rdx::engine::{DecompilerEngine, NativeEngine};
use std::path::PathBuf;

struct Fixture(PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
fn word(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}
fn put(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}
fn uleb(bytes: &mut Vec<u8>, mut value: u32) {
    loop {
        let low = (value & 127) as u8;
        value >>= 7;
        bytes.push(low | if value == 0 { 0 } else { 128 });
        if value == 0 {
            break;
        }
    }
}
fn fixture() -> Fixture {
    let mut bytes = include_bytes!("fixtures/hello.dex").to_vec();
    let old_map = word(&bytes, 52) as usize;
    let map_count = word(&bytes, old_map) as usize;
    let mut map = bytes[old_map..old_map + 4 + map_count * 12].to_vec();
    while !bytes.len().is_multiple_of(4) {
        bytes.push(0);
    }
    let code = bytes.len() as u32;
    // if (0 == 0) return 42; else return answer();
    // The self-call is retained for checking exact symbol mappings, not executed.
    let instructions: [u16; 11] = [
        0x0012, 0x0038, 7, 0x0071, 0, 0, 0x000a, 0x000f, 0x0013, 42, 0x000f,
    ];
    for value in [1u16, 0, 0, 0] {
        bytes.extend(value.to_le_bytes());
    }
    bytes.extend(0u32.to_le_bytes());
    bytes.extend((instructions.len() as u32).to_le_bytes());
    for instruction in instructions {
        bytes.extend(instruction.to_le_bytes());
    }
    let class_data = bytes.len() as u32;
    bytes.extend([0, 0, 1, 0, 0, 9]);
    uleb(&mut bytes, code);
    while !bytes.len().is_multiple_of(4) {
        bytes.push(0);
    }
    let map_offset = bytes.len() as u32;
    for i in 0..map_count {
        let p = 4 + i * 12;
        let ty = u16::from_le_bytes(map[p..p + 2].try_into().unwrap());
        let offset = match ty {
            0x2001 => Some(code),
            0x2000 => Some(class_data),
            0x1000 => Some(map_offset),
            _ => None,
        };
        if let Some(offset) = offset {
            put(&mut map, p + 8, offset);
        }
    }
    bytes.extend(map);
    let class_defs = word(&bytes, 100) as usize;
    put(&mut bytes, class_defs + 24, class_data);
    put(&mut bytes, 52, map_offset);
    let len = bytes.len() as u32;
    let data_start = word(&bytes, 108);
    put(&mut bytes, 32, len);
    put(&mut bytes, 104, len - data_start);
    // Parser intentionally does not verify signatures/checksums. This fixture
    // tests its supported DEX metadata/code path, not APK installation validity.
    let path =
        std::env::temp_dir().join(format!("rdx-native-branch-nav-{}.dex", std::process::id()));
    std::fs::write(&path, bytes).unwrap();
    Fixture(path)
}
#[test]
fn reconstructed_branch_keeps_exact_navigation_and_single_usage() {
    let input = fixture();
    let mut engine = NativeEngine::start().unwrap();
    engine.open(&input.0).unwrap();
    let code = engine.decompile_with_metadata("sample.Hello").unwrap();
    assert!(code.source.contains("if ("), "{}", code.source);
    assert!(!code.source.contains(".method"));
    let usages = engine
        .usages_in_class("sample.Hello.answer()I", "sample.Hello")
        .unwrap();
    assert_eq!(usages.occurrences.len(), 1);
    let usage = &usages.occurrences[0];
    assert_eq!(
        code.source
            .chars()
            .skip(usage.start)
            .take(usage.end - usage.start)
            .collect::<String>(),
        "answer"
    );
    let target = engine
        .navigate("sample.Hello", usage.start, &code.source_hash)
        .unwrap();
    let symbol = engine
        .resolve_usage_target("sample.Hello", target.position, &target.code.source_hash)
        .unwrap();
    assert_eq!(symbol.id, "sample.Hello.answer()I");
    assert_eq!(code.source_hash, target.code.source_hash);
}
