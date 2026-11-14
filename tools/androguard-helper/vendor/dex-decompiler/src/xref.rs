//! Cross-references for DEX methods (callers / callees).
//!
//! Scans bytecode for `invoke-*` instructions that reference a given `method_ids` index.
//! Intended for reuse by CLIs, WASM UIs (droid2web), and analysis tools.

use dex_bytecode::decode_all;
use dex_parser::{DexFile, EncodedMethod};
use serde::Serialize;

use crate::error::{DexDecompilerError, Result};
use crate::java;

/// One invoke site that calls a given method_ids entry.
#[derive(Debug, Clone, Serialize)]
pub struct MethodCaller {
    /// Caller class in Java form (`com.foo.Bar`).
    pub class_name: String,
    /// Caller method name (DEX name; `<init>` kept as-is).
    pub method_name: String,
    /// Caller prototype string, e.g. `V(Landroid/os/Bundle;)`.
    pub method_descriptor: String,
    /// Index into `class_defs` (same order as light DEX parse UIs).
    pub class_idx: usize,
    /// Index into direct_methods then virtual_methods for that class.
    pub method_idx_in_class: usize,
    /// `method_ids` index of the caller method itself.
    pub caller_method_idx: u32,
    /// Instruction offset within the caller method bytecode.
    pub offset: u32,
    /// Absolute file offset of the invoke instruction in the DEX.
    pub file_offset: u32,
    /// Mnemonic (`invoke-virtual`, `invoke-static/range`, …).
    pub invoke_kind: String,
    /// Callee `method_ids` index that was matched.
    pub callee_method_idx: u32,
}

/// Result of looking up callers for one method.
#[derive(Debug, Clone, Serialize)]
pub struct MethodCallersInfo {
    pub callee_method_idx: u32,
    pub callee_class_name: String,
    pub callee_method_name: String,
    pub callee_descriptor: String,
    pub callers: Vec<MethodCaller>,
    /// True when more sites exist than were returned (capped).
    pub truncated: bool,
}

const METHOD_CALLERS_CAP: usize = 500;

/// Invoke opcodes that encode a `method_ids` index at bytes `[offset+2 .. offset+4)` (u16 LE).
///
/// - `0x6e..=0x72` — invoke-{virtual,super,direct,static,interface} (F35c)
/// - `0x74..=0x78` — same `/range` forms (F3rc)
/// - `0xfa`, `0xfb` — invoke-polymorphic{,/range} (method + proto)
///
fn invoke_method_index(insns: &[u8], offset: u32, opcode: u8) -> Option<u32> {
    let o = offset as usize;
    let is_method_invoke = matches!(
        opcode,
        0x6e..=0x72 | 0x74..=0x78 | 0xfa | 0xfb
    );
    if !is_method_invoke || o + 4 > insns.len() {
        return None;
    }
    Some(u16::from_le_bytes([insns[o + 2], insns[o + 3]]) as u32)
}

/// Resolve normal invokes and invoke-custom lambda implementation handles.
fn resolved_invoke_method_index(
    dex: &DexFile,
    insns: &[u8],
    offset: u32,
    opcode: u8,
) -> Option<u32> {
    if let Some(method_idx) = invoke_method_index(insns, offset, opcode) {
        return Some(method_idx);
    }
    if !matches!(opcode, 0xfc | 0xfd) {
        return None;
    }
    let o = offset as usize;
    if o + 4 > insns.len() {
        return None;
    }
    let callsite_idx = u16::from_le_bytes([insns[o + 2], insns[o + 3]]) as u32;
    let callsite = dex.get_call_site(callsite_idx).ok()?;
    dex_parser::DexCallSites::impl_method_id(&callsite).map(u32::from)
}

fn invoke_kind_name(opcode: u8) -> &'static str {
    match opcode {
        0x6e => "invoke-virtual",
        0x6f => "invoke-super",
        0x70 => "invoke-direct",
        0x71 => "invoke-static",
        0x72 => "invoke-interface",
        0x74 => "invoke-virtual/range",
        0x75 => "invoke-super/range",
        0x76 => "invoke-direct/range",
        0x77 => "invoke-static/range",
        0x78 => "invoke-interface/range",
        0xfa => "invoke-polymorphic",
        0xfb => "invoke-polymorphic/range",
        0xfc => "invoke-custom",
        0xfd => "invoke-custom/range",
        _ => "invoke",
    }
}

fn format_method_descriptor(return_type: &str, params: &[String]) -> String {
    let mut s = String::from("(");
    for (i, p) in params.iter().enumerate() {
        if i > 0 {
            s.push_str(", ");
        }
        s.push_str(&java::descriptor_to_java(p));
    }
    s.push(')');
    s.push_str(&java::descriptor_to_java(return_type));
    s
}

fn method_descriptor_from_dex(dex: &DexFile, method_idx: u32) -> Result<(String, String, String)> {
    let info = dex
        .get_method_info(method_idx)
        .map_err(|e| DexDecompilerError::Parse(e.to_string()))?;
    let class_name = java::descriptor_to_java(&info.class);
    let descriptor = format_method_descriptor(&info.return_type, &info.params);
    Ok((class_name, info.name.to_string(), descriptor))
}

fn normalize_java_class_name(name: &str) -> String {
    let mut n = name.trim().to_string();
    if n.starts_with('L') && n.ends_with(';') {
        n = n[1..n.len() - 1].replace('/', ".");
    } else {
        n = n.replace('/', ".");
    }
    n
}

/// Scan class code for invokes whose `method_ids` index is in `callee_idxs`.
fn find_method_callers_for_set(
    dex: &DexFile,
    callee_idxs: &std::collections::HashSet<u32>,
    primary_idx: u32,
    callee_class_name: String,
    callee_method_name: String,
    callee_descriptor: String,
) -> Result<MethodCallersInfo> {
    let mut callers = Vec::new();
    let mut truncated = false;

    'classes: for (class_idx, class_result) in dex.class_defs().enumerate() {
        let Ok(class_def) = class_result else {
            continue;
        };
        let Ok(class_type) = dex.get_type(class_def.class_idx) else {
            continue;
        };
        let class_name = java::descriptor_to_java(&class_type);
        let Ok(Some(class_data)) = dex.get_class_data(&class_def) else {
            continue;
        };

        let all_methods: Vec<&EncodedMethod> = class_data
            .direct_methods
            .iter()
            .chain(class_data.virtual_methods.iter())
            .collect();

        for (method_idx_in_class, encoded) in all_methods.into_iter().enumerate() {
            if encoded.code_off == 0 {
                continue;
            }
            let Ok(method_info) = dex.get_method_info(encoded.method_idx) else {
                continue;
            };
            let method_name = method_info.name.to_string();
            let method_descriptor =
                format_method_descriptor(&method_info.return_type, &method_info.params);
            let Ok(code) = dex.get_code_item(encoded.code_off) else {
                continue;
            };
            let insns = code.insns_slice(&dex.data);
            let Ok(decoded) = decode_all(insns, 0) else {
                continue;
            };
            let insns_base = code.insns_off as u32;
            for ins in decoded {
                let Some(midx) = resolved_invoke_method_index(dex, insns, ins.offset, ins.opcode)
                else {
                    continue;
                };
                if !callee_idxs.contains(&midx) {
                    continue;
                }
                if callers.len() >= METHOD_CALLERS_CAP {
                    truncated = true;
                    break 'classes;
                }
                callers.push(MethodCaller {
                    class_name: class_name.clone(),
                    method_name: method_name.clone(),
                    method_descriptor: method_descriptor.clone(),
                    class_idx,
                    method_idx_in_class,
                    caller_method_idx: encoded.method_idx,
                    offset: ins.offset,
                    file_offset: insns_base.saturating_add(ins.offset),
                    invoke_kind: invoke_kind_name(ins.opcode).to_string(),
                    callee_method_idx: midx,
                });
            }
        }
    }

    Ok(MethodCallersInfo {
        callee_method_idx: primary_idx,
        callee_class_name,
        callee_method_name,
        callee_descriptor,
        callers,
        truncated,
    })
}

/// Find all invoke sites that reference `callee_method_idx` in `method_ids`.
pub fn find_method_callers(dex: &DexFile, callee_method_idx: u32) -> Result<MethodCallersInfo> {
    // ASC fast path — same result shape as the decode-all scan, with insn offsets.
    find_method_callers_fast(dex, callee_method_idx)
}

/// Find invoke sites calling any `method_ids` entry matching class + method name
/// (all overloads). Useful for JNI / multidex where the defining class may be absent
/// from the DEX that contains the call site.
pub fn find_method_callers_by_name(
    dex: &DexFile,
    class_name: &str,
    method_name: &str,
) -> Result<MethodCallersInfo> {
    let want_class = normalize_java_class_name(class_name);
    let want_method = method_name.trim();
    let mut matched = std::collections::HashSet::new();
    let mut primary_idx = 0u32;
    let mut callee_class_name = want_class.clone();
    let mut callee_descriptor = String::new();

    for i in 0..dex.header.method_ids_size {
        let Ok((cn, mn, desc)) = method_descriptor_from_dex(dex, i) else {
            continue;
        };
        if mn != want_method {
            continue;
        }
        if normalize_java_class_name(&cn) != want_class {
            continue;
        }
        if matched.is_empty() {
            primary_idx = i;
            callee_class_name = cn;
            callee_descriptor = desc;
        }
        matched.insert(i);
    }

    if matched.is_empty() {
        return Ok(MethodCallersInfo {
            callee_method_idx: 0,
            callee_class_name: want_class,
            callee_method_name: want_method.to_string(),
            callee_descriptor: String::new(),
            callers: Vec::new(),
            truncated: false,
        });
    }

    // Single overload → ASC fast path; multi-overload keeps the set scan.
    if matched.len() == 1 {
        return find_method_callers_fast(dex, primary_idx);
    }

    find_method_callers_for_set(
        dex,
        &matched,
        primary_idx,
        callee_class_name,
        want_method.to_string(),
        callee_descriptor,
    )
}

/// Resolve a UI class/method pair (light-parse indices) to a `method_ids` index, then find callers.
pub fn find_method_callers_by_class_method(
    dex: &DexFile,
    class_idx: usize,
    method_idx_in_class: usize,
) -> Result<MethodCallersInfo> {
    let mut n = 0usize;
    for class_result in dex.class_defs() {
        let Ok(class_def) = class_result else {
            n += 1;
            continue;
        };
        if n != class_idx {
            n += 1;
            continue;
        }
        let Ok(Some(class_data)) = dex.get_class_data(&class_def) else {
            return Err(DexDecompilerError::Parse(format!(
                "class_idx {} has no class_data",
                class_idx
            )));
        };
        let all: Vec<&EncodedMethod> = class_data
            .direct_methods
            .iter()
            .chain(class_data.virtual_methods.iter())
            .collect();
        let encoded = all.get(method_idx_in_class).ok_or_else(|| {
            DexDecompilerError::Parse(format!(
                "method_idx {} out of range (class has {} methods)",
                method_idx_in_class,
                all.len()
            ))
        })?;
        return find_method_callers(dex, encoded.method_idx);
    }
    Err(DexDecompilerError::Parse(format!(
        "class_idx {} out of range",
        class_idx
    )))
}

/// One callee referenced by invokes inside a method.
#[derive(Debug, Clone, Serialize)]
pub struct MethodCallee {
    pub method_idx: u32,
    pub class_name: String,
    pub method_name: String,
    pub method_descriptor: String,
    pub offset: u32,
    pub invoke_kind: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct MethodCalleesInfo {
    pub caller_method_idx: u32,
    pub callees: Vec<MethodCallee>,
    pub truncated: bool,
}

const METHOD_CALLEES_CAP: usize = 400;

/// List methods invoked from the given `method_ids` entry (unique by method_idx, first site kept).
pub fn find_method_callees(dex: &DexFile, caller_method_idx: u32) -> Result<MethodCalleesInfo> {
    let mut callees = Vec::new();
    let mut truncated = false;
    let mut seen = std::collections::HashSet::new();

    'classes: for class_result in dex.class_defs() {
        let Ok(class_def) = class_result else {
            continue;
        };
        let Ok(Some(class_data)) = dex.get_class_data(&class_def) else {
            continue;
        };
        for encoded in class_data
            .direct_methods
            .iter()
            .chain(class_data.virtual_methods.iter())
        {
            if encoded.method_idx != caller_method_idx || encoded.code_off == 0 {
                continue;
            }
            let Ok(code) = dex.get_code_item(encoded.code_off) else {
                continue;
            };
            let insns = code.insns_slice(&dex.data);
            let Ok(decoded) = decode_all(insns, 0) else {
                continue;
            };
            for ins in decoded {
                let Some(midx) = resolved_invoke_method_index(dex, insns, ins.offset, ins.opcode)
                else {
                    continue;
                };
                if !seen.insert(midx) {
                    continue;
                }
                if callees.len() >= METHOD_CALLEES_CAP {
                    truncated = true;
                    break 'classes;
                }
                let (class_name, method_name, method_descriptor) =
                    method_descriptor_from_dex(dex, midx)
                        .unwrap_or_else(|_| ("?".into(), "?".into(), "()".into()));
                callees.push(MethodCallee {
                    method_idx: midx,
                    class_name,
                    method_name,
                    method_descriptor,
                    offset: ins.offset,
                    invoke_kind: invoke_kind_name(ins.opcode).to_string(),
                });
            }
        }
    }

    Ok(MethodCalleesInfo {
        caller_method_idx,
        callees,
        truncated,
    })
}

pub fn find_method_callees_by_class_method(
    dex: &DexFile,
    class_idx: usize,
    method_idx_in_class: usize,
) -> Result<MethodCalleesInfo> {
    let callers = find_method_callers_by_class_method(dex, class_idx, method_idx_in_class)?;
    // Reuse index resolution via callers API's callee idx… actually we need the method itself.
    let mut n = 0usize;
    for class_result in dex.class_defs() {
        let Ok(class_def) = class_result else {
            n += 1;
            continue;
        };
        if n != class_idx {
            n += 1;
            continue;
        }
        let Ok(Some(class_data)) = dex.get_class_data(&class_def) else {
            return Err(DexDecompilerError::Parse("no class_data".into()));
        };
        let all: Vec<&EncodedMethod> = class_data
            .direct_methods
            .iter()
            .chain(class_data.virtual_methods.iter())
            .collect();
        let encoded = all
            .get(method_idx_in_class)
            .ok_or_else(|| DexDecompilerError::Parse("method_idx out of range".into()))?;
        let _ = callers;
        return find_method_callees(dex, encoded.method_idx);
    }
    Err(DexDecompilerError::Parse("class_idx out of range".into()))
}

/// One frame on a reverse call path (caller → … → target).
#[derive(Debug, Clone, Serialize)]
pub struct CallTraceFrame {
    pub class_name: String,
    pub method_name: String,
    pub method_descriptor: String,
    pub class_idx: usize,
    pub method_idx_in_class: usize,
    pub method_idx: u32,
    /// Invoke offset inside this method that reaches the next frame (None on the target).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub invoke_kind: Option<String>,
}

/// One full reverse call path: entry/root first, target last.
#[derive(Debug, Clone, Serialize)]
pub struct CallTracePath {
    pub frames: Vec<CallTraceFrame>,
}

/// Reverse call traces for a method (who calls it, and who calls those callers, …).
#[derive(Debug, Clone, Serialize)]
pub struct MethodCallTracesInfo {
    pub target_method_idx: u32,
    pub target_class_name: String,
    pub target_method_name: String,
    pub target_descriptor: String,
    pub paths: Vec<CallTracePath>,
    /// Depth / fan-in / path caps were hit.
    pub truncated: bool,
    pub max_depth: usize,
}

const CALL_TRACE_MAX_DEPTH: usize = 6;
const CALL_TRACE_MAX_PATHS: usize = 48;
const CALL_TRACE_MAX_FANIN: usize = 16;

#[derive(Clone, Copy, Debug)]
struct CompactInEdge {
    caller_method_idx: u32,
    class_idx: u32,
    method_idx_in_class: u32,
    offset: u32,
    opcode: u8,
}

fn resolve_encoded_method_idx(
    dex: &DexFile,
    class_idx: usize,
    method_idx_in_class: usize,
) -> Result<u32> {
    let mut n = 0usize;
    for class_result in dex.class_defs() {
        let Ok(class_def) = class_result else {
            n += 1;
            continue;
        };
        if n != class_idx {
            n += 1;
            continue;
        }
        let Ok(Some(class_data)) = dex.get_class_data(&class_def) else {
            return Err(DexDecompilerError::Parse(format!(
                "class_idx {} has no class_data",
                class_idx
            )));
        };
        let all: Vec<&EncodedMethod> = class_data
            .direct_methods
            .iter()
            .chain(class_data.virtual_methods.iter())
            .collect();
        let encoded = all.get(method_idx_in_class).ok_or_else(|| {
            DexDecompilerError::Parse(format!(
                "method_idx {} out of range (class has {} methods)",
                method_idx_in_class,
                all.len()
            ))
        })?;
        return Ok(encoded.method_idx);
    }
    Err(DexDecompilerError::Parse(format!(
        "class_idx {} out of range",
        class_idx
    )))
}

/// One-pass reverse call index: callee method_ids → incoming invoke edges (capped per callee).
///
/// Build once per DEX and reuse with [`find_method_call_traces_with_index`] — rebuilding this
/// for every sink is the main cost of entry-point reachability.
#[derive(Debug, Clone, Default)]
pub struct ReverseCallIndex {
    rev: std::collections::HashMap<u32, Vec<CompactInEdge>>,
}

impl ReverseCallIndex {
    pub fn build(dex: &DexFile) -> Self {
        Self::build_with_progress(dex, |_, _| {})
    }

    /// Like [`build`], but invokes `on_progress(classes_done, classes_total)` periodically.
    pub fn build_with_progress(dex: &DexFile, on_progress: impl FnMut(usize, usize)) -> Self {
        Self {
            rev: build_reverse_call_index(dex, on_progress),
        }
    }

    pub fn incoming_count(&self) -> usize {
        self.rev.len()
    }
}

/// One-pass reverse call index: callee method_ids → incoming invoke edges (capped per callee).
fn build_reverse_call_index(
    dex: &DexFile,
    mut on_progress: impl FnMut(usize, usize),
) -> std::collections::HashMap<u32, Vec<CompactInEdge>> {
    use std::collections::HashMap;
    let mut rev: HashMap<u32, Vec<CompactInEdge>> = HashMap::new();
    let total = dex.header.class_defs_size as usize;
    let mut last_report = 0usize;

    for (class_idx, class_result) in dex.class_defs().enumerate() {
        let done = class_idx + 1;
        // ~2% steps or every 250 classes, whichever is more frequent for small DEXes.
        let step = (total / 50).max(250).min(2000).max(1);
        if done == total || done - last_report >= step {
            on_progress(done, total);
            last_report = done;
        }
        let Ok(class_def) = class_result else {
            continue;
        };
        let Ok(Some(class_data)) = dex.get_class_data(&class_def) else {
            continue;
        };
        let all: Vec<&EncodedMethod> = class_data
            .direct_methods
            .iter()
            .chain(class_data.virtual_methods.iter())
            .collect();
        for (method_idx_in_class, encoded) in all.into_iter().enumerate() {
            if encoded.code_off == 0 {
                continue;
            }
            let Ok(code) = dex.get_code_item(encoded.code_off) else {
                continue;
            };
            let insns = code.insns_slice(&dex.data);
            let Ok(decoded) = decode_all(insns, 0) else {
                continue;
            };
            for ins in decoded {
                let Some(callee) = resolved_invoke_method_index(dex, insns, ins.offset, ins.opcode)
                else {
                    continue;
                };
                let entry = rev.entry(callee).or_default();
                if entry.len() >= CALL_TRACE_MAX_FANIN {
                    continue;
                }
                // Prefer unique callers (first invoke site wins).
                if entry
                    .iter()
                    .any(|e| e.caller_method_idx == encoded.method_idx)
                {
                    continue;
                }
                entry.push(CompactInEdge {
                    caller_method_idx: encoded.method_idx,
                    class_idx: class_idx as u32,
                    method_idx_in_class: method_idx_in_class as u32,
                    offset: ins.offset,
                    opcode: ins.opcode,
                });
            }
        }
    }
    rev
}

fn frame_from_method(
    dex: &DexFile,
    method_idx: u32,
    class_idx: usize,
    method_idx_in_class: usize,
    offset: Option<u32>,
    invoke_kind: Option<String>,
) -> CallTraceFrame {
    let (class_name, method_name, method_descriptor) = method_descriptor_from_dex(dex, method_idx)
        .unwrap_or_else(|_| ("?".into(), "?".into(), "()".into()));
    CallTraceFrame {
        class_name,
        method_name,
        method_descriptor,
        class_idx,
        method_idx_in_class,
        method_idx,
        offset,
        invoke_kind,
    }
}

/// Build reverse call traces ending at `callee_method_idx` (paths are root → … → target).
pub fn find_method_call_traces(
    dex: &DexFile,
    callee_method_idx: u32,
) -> Result<MethodCallTracesInfo> {
    let index = ReverseCallIndex::build(dex);
    find_method_call_traces_with_index(dex, callee_method_idx, &index)
}

/// Like [`find_method_call_traces`], but reuses a precomputed [`ReverseCallIndex`].
pub fn find_method_call_traces_with_index(
    dex: &DexFile,
    callee_method_idx: u32,
    index: &ReverseCallIndex,
) -> Result<MethodCallTracesInfo> {
    if callee_method_idx >= dex.header.method_ids_size {
        return Err(DexDecompilerError::Parse(format!(
            "method index {} out of range (size {})",
            callee_method_idx, dex.header.method_ids_size
        )));
    }
    let (target_class_name, target_method_name, target_descriptor) =
        method_descriptor_from_dex(dex, callee_method_idx)?;

    let rev = &index.rev;
    let mut paths = Vec::new();
    let mut truncated = false;

    // Locate UI indices for the target (best-effort).
    let (target_class_idx, target_method_idx_in_class) = {
        let mut found = (0usize, 0usize);
        'outer: for (ci, class_result) in dex.class_defs().enumerate() {
            let Ok(class_def) = class_result else {
                continue;
            };
            let Ok(Some(class_data)) = dex.get_class_data(&class_def) else {
                continue;
            };
            for (mi, encoded) in class_data
                .direct_methods
                .iter()
                .chain(class_data.virtual_methods.iter())
                .enumerate()
            {
                if encoded.method_idx == callee_method_idx {
                    found = (ci, mi);
                    break 'outer;
                }
            }
        }
        found
    };

    let target_frame = frame_from_method(
        dex,
        callee_method_idx,
        target_class_idx,
        target_method_idx_in_class,
        None,
        None,
    );

    fn walk(
        dex: &DexFile,
        rev: &std::collections::HashMap<u32, Vec<CompactInEdge>>,
        current: u32,
        path_to_target: &[CallTraceFrame], // current … target (current first)
        depth: usize,
        paths: &mut Vec<CallTracePath>,
        truncated: &mut bool,
        on_path: &mut std::collections::HashSet<u32>,
    ) {
        if paths.len() >= CALL_TRACE_MAX_PATHS {
            *truncated = true;
            return;
        }
        let incoming = rev.get(&current).map(|v| v.as_slice()).unwrap_or(&[]);
        let mut preferred: Vec<&CompactInEdge> = incoming
            .iter()
            .filter(|e| {
                method_descriptor_from_dex(dex, e.caller_method_idx)
                    .map(|(c, _, _)| !crate::detectors::is_library_class(&c))
                    .unwrap_or(true)
            })
            .collect();
        if preferred.is_empty() {
            preferred = incoming.iter().collect();
        }

        if preferred.is_empty() || depth >= CALL_TRACE_MAX_DEPTH {
            let mut frames = path_to_target.to_vec();
            frames.reverse();
            paths.push(CallTracePath { frames });
            return;
        }

        let mut expanded = false;
        for edge in preferred.into_iter().take(CALL_TRACE_MAX_FANIN) {
            if paths.len() >= CALL_TRACE_MAX_PATHS {
                *truncated = true;
                break;
            }
            if !on_path.insert(edge.caller_method_idx) {
                continue;
            }
            expanded = true;
            let caller_frame = frame_from_method(
                dex,
                edge.caller_method_idx,
                edge.class_idx as usize,
                edge.method_idx_in_class as usize,
                Some(edge.offset),
                Some(invoke_kind_name(edge.opcode).to_string()),
            );
            let mut next_path = Vec::with_capacity(path_to_target.len() + 1);
            next_path.push(caller_frame);
            next_path.extend_from_slice(path_to_target);
            walk(
                dex,
                rev,
                edge.caller_method_idx,
                &next_path,
                depth + 1,
                paths,
                truncated,
                on_path,
            );
            on_path.remove(&edge.caller_method_idx);
        }
        if !expanded {
            let mut frames = path_to_target.to_vec();
            frames.reverse();
            paths.push(CallTracePath { frames });
        }
    }

    let mut on_path = std::collections::HashSet::new();
    on_path.insert(callee_method_idx);
    walk(
        dex,
        rev,
        callee_method_idx,
        &[target_frame],
        0,
        &mut paths,
        &mut truncated,
        &mut on_path,
    );

    // Prefer longer paths first (deeper reverse chains).
    paths.sort_by(|a, b| b.frames.len().cmp(&a.frames.len()));

    Ok(MethodCallTracesInfo {
        target_method_idx: callee_method_idx,
        target_class_name,
        target_method_name,
        target_descriptor,
        paths,
        truncated,
        max_depth: CALL_TRACE_MAX_DEPTH,
    })
}

/// Resolve UI class/method indices, then build reverse call traces.
pub fn find_method_call_traces_by_class_method(
    dex: &DexFile,
    class_idx: usize,
    method_idx_in_class: usize,
) -> Result<MethodCallTracesInfo> {
    let method_idx = resolve_encoded_method_idx(dex, class_idx, method_idx_in_class)?;
    find_method_call_traces(dex, method_idx)
}

/// One field get/put site.
#[derive(Debug, Clone, Serialize)]
pub struct FieldXref {
    pub class_name: String,
    pub method_name: String,
    pub class_idx: usize,
    pub method_idx_in_class: usize,
    pub offset: u32,
    pub access_kind: String,
    pub field_idx: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct FieldXrefsInfo {
    pub field_idx: u32,
    pub field_class: String,
    pub field_name: String,
    pub field_type: String,
    pub xrefs: Vec<FieldXref>,
    pub truncated: bool,
}

const FIELD_XREFS_CAP: usize = 500;

fn field_index(insns: &[u8], offset: u32, opcode: u8) -> Option<u32> {
    // iget/iput/sget/sput family: field idx at +2 (u16), except some range forms.
    let o = offset as usize;
    let is_field = matches!(
        opcode,
        0x52..=0x6d // iget* / iput* / sget* / sput*
    );
    if !is_field || o + 4 > insns.len() {
        return None;
    }
    Some(u16::from_le_bytes([insns[o + 2], insns[o + 3]]) as u32)
}

fn field_access_kind(opcode: u8) -> &'static str {
    match opcode {
        0x52..=0x58 => "iget",
        0x59..=0x5f => "iput",
        0x60..=0x66 => "sget",
        0x67..=0x6d => "sput",
        _ => "field",
    }
}

/// Find all iget/iput/sget/sput sites for a field_ids index.
pub fn find_field_xrefs(dex: &DexFile, field_idx: u32) -> Result<FieldXrefsInfo> {
    if field_idx >= dex.header.field_ids_size {
        return Err(DexDecompilerError::Parse(format!(
            "field index {} out of range",
            field_idx
        )));
    }
    let fi = dex
        .get_field_info(field_idx)
        .map_err(|e| DexDecompilerError::Parse(e.to_string()))?;
    let field_class = java::descriptor_to_java(&fi.class);
    let field_name = fi.name.to_string();
    let field_type = java::descriptor_to_java(&fi.typ);

    let mut xrefs = Vec::new();
    let mut truncated = false;
    'classes: for (class_idx, class_result) in dex.class_defs().enumerate() {
        let Ok(class_def) = class_result else {
            continue;
        };
        let Ok(class_type) = dex.get_type(class_def.class_idx) else {
            continue;
        };
        let class_name = java::descriptor_to_java(&class_type);
        let Ok(Some(class_data)) = dex.get_class_data(&class_def) else {
            continue;
        };
        let all: Vec<&EncodedMethod> = class_data
            .direct_methods
            .iter()
            .chain(class_data.virtual_methods.iter())
            .collect();
        for (method_idx_in_class, encoded) in all.into_iter().enumerate() {
            if encoded.code_off == 0 {
                continue;
            }
            let Ok(method_info) = dex.get_method_info(encoded.method_idx) else {
                continue;
            };
            let Ok(code) = dex.get_code_item(encoded.code_off) else {
                continue;
            };
            let insns = code.insns_slice(&dex.data);
            let Ok(decoded) = decode_all(insns, 0) else {
                continue;
            };
            for ins in decoded {
                let Some(fidx) = field_index(insns, ins.offset, ins.opcode) else {
                    continue;
                };
                if fidx != field_idx {
                    continue;
                }
                if xrefs.len() >= FIELD_XREFS_CAP {
                    truncated = true;
                    break 'classes;
                }
                xrefs.push(FieldXref {
                    class_name: class_name.clone(),
                    method_name: method_info.name.to_string(),
                    class_idx,
                    method_idx_in_class,
                    offset: ins.offset,
                    access_kind: field_access_kind(ins.opcode).to_string(),
                    field_idx,
                });
            }
        }
    }

    Ok(FieldXrefsInfo {
        field_idx,
        field_class,
        field_name,
        field_type,
        xrefs,
        truncated,
    })
}

/// Generic reference site from the fast path (string / type / field / method).
#[derive(Debug, Clone, Serialize)]
pub struct FastRefSite {
    pub class_name: String,
    pub method_name: String,
    pub method_idx: u32,
    pub file_offset: u32,
    /// Method-relative instruction offset (bytes), for Code-view navigation.
    pub offset: u32,
    pub pool_idx: u32,
    pub class_idx: usize,
    pub method_idx_in_class: usize,
}

/// Prefer [`find_method_callers_fast`] for a single query; keep [`ReverseCallIndex`] if you
/// expect more than ~20 call-trace queries on the same DEX.
pub fn find_method_callers_fast(dex: &DexFile, callee_method_idx: u32) -> Result<MethodCallersInfo> {
    if callee_method_idx >= dex.header.method_ids_size {
        return Err(DexDecompilerError::Parse(format!(
            "method index {} out of range (size {})",
            callee_method_idx, dex.header.method_ids_size
        )));
    }
    let (callee_class_name, callee_method_name, callee_descriptor) =
        method_descriptor_from_dex(dex, callee_method_idx)?;
    let raw = dex_parser::RawDex::from_dex(dex);
    let mut fr = dex_parser::FastRef::from_raw(raw);
    let sites = fr
        .find_method_idx(callee_method_idx)
        .map_err(|e| DexDecompilerError::Parse(e.to_string()))?;
    callers_from_fast_sites(
        dex,
        sites,
        callee_method_idx,
        callee_class_name,
        callee_method_name,
        callee_descriptor,
    )
}

fn callers_from_fast_sites(
    dex: &DexFile,
    sites: Vec<dex_parser::RefSite>,
    primary_idx: u32,
    callee_class_name: String,
    callee_method_name: String,
    callee_descriptor: String,
) -> Result<MethodCallersInfo> {
    let wanted: std::collections::HashSet<u32> = sites.iter().map(|s| s.method_idx).collect();
    let ui = method_ui_map(dex, &wanted)?;
    let mut callers = Vec::new();
    let mut truncated = false;
    for s in sites {
        if callers.len() >= METHOD_CALLERS_CAP {
            truncated = true;
            break;
        }
        let (class_idx, method_idx_in_class, class_name) = ui
            .get(&s.method_idx)
            .cloned()
            .unwrap_or((0, 0, String::new()));
        let info = dex.get_method_info(s.method_idx).ok();
        let method_name = info
            .as_ref()
            .map(|i| i.name.to_string())
            .unwrap_or_default();
        let method_descriptor = info
            .as_ref()
            .map(|i| format_method_descriptor(&i.return_type, &i.params))
            .unwrap_or_default();
        let class_name = if class_name.is_empty() {
            info.map(|i| java::descriptor_to_java(&i.class))
                .unwrap_or_default()
        } else {
            class_name
        };
        callers.push(MethodCaller {
            class_name,
            method_name,
            method_descriptor,
            class_idx,
            method_idx_in_class,
            caller_method_idx: s.method_idx,
            offset: s.insn_off,
            file_offset: s.file_off,
            invoke_kind: "invoke".into(),
            callee_method_idx: s.pool_idx,
        });
    }
    Ok(MethodCallersInfo {
        callee_method_idx: primary_idx,
        callee_class_name,
        callee_method_name,
        callee_descriptor,
        callers,
        truncated,
    })
}

fn method_ui_map(
    dex: &DexFile,
    wanted: &std::collections::HashSet<u32>,
) -> Result<std::collections::HashMap<u32, (usize, usize, String)>> {
    let mut out = std::collections::HashMap::new();
    if wanted.is_empty() {
        return Ok(out);
    }
    for (class_idx, class_result) in dex.class_defs().enumerate() {
        let Ok(class_def) = class_result else { continue };
        let Ok(class_type) = dex.get_type(class_def.class_idx) else { continue };
        let class_name = java::descriptor_to_java(&class_type);
        let Ok(Some(class_data)) = dex.get_class_data(&class_def) else { continue };
        for (mi, encoded) in class_data
            .direct_methods
            .iter()
            .chain(class_data.virtual_methods.iter())
            .enumerate()
        {
            if wanted.contains(&encoded.method_idx) {
                out.insert(encoded.method_idx, (class_idx, mi, class_name.clone()));
            }
        }
        if out.len() >= wanted.len() {
            break;
        }
    }
    Ok(out)
}

/// Normalize a user type name to a Dalvik descriptor for hierarchy queries.
fn hierarchy_descriptor(name: &str) -> String {
    let n = name.trim();
    if n.starts_with('L') && n.ends_with(';') {
        return n.to_string();
    }
    if n.len() == 1 || n.starts_with('[') {
        return n.to_string();
    }
    format!("L{};", n.replace('.', "/"))
}

fn type_eq(a: &str, b: &str) -> bool {
    a == b || hierarchy_descriptor(a) == hierarchy_descriptor(b)
}

/// Classes whose direct superclass is `base` (ASC `findrefs extends`).
///
/// Returns Dalvik descriptors of matching classes.
pub fn find_extends(dex: &DexFile, base: &str) -> Result<Vec<String>> {
    use dex_parser::NO_INDEX;
    let target = hierarchy_descriptor(base);
    let mut out = Vec::new();
    for class_result in dex.class_defs() {
        let class_def = class_result.map_err(|e| DexDecompilerError::Parse(e.to_string()))?;
        if class_def.superclass_idx == NO_INDEX {
            continue;
        }
        let super_desc = dex
            .get_type(class_def.superclass_idx)
            .map_err(|e| DexDecompilerError::Parse(e.to_string()))?;
        if type_eq(&super_desc, &target) {
            let name = dex
                .get_type(class_def.class_idx)
                .map_err(|e| DexDecompilerError::Parse(e.to_string()))?;
            out.push(name);
        }
    }
    Ok(out)
}

/// Classes that directly implement interface `iface` (ASC `findrefs impl`).
pub fn find_impls(dex: &DexFile, iface: &str) -> Result<Vec<String>> {
    let target = hierarchy_descriptor(iface);
    let mut out = Vec::new();
    for class_result in dex.class_defs() {
        let class_def = class_result.map_err(|e| DexDecompilerError::Parse(e.to_string()))?;
        let ifaces = dex
            .get_interfaces(&class_def)
            .map_err(|e| DexDecompilerError::Parse(e.to_string()))?;
        if ifaces.iter().any(|i| type_eq(i, &target)) {
            let name = dex
                .get_type(class_def.class_idx)
                .map_err(|e| DexDecompilerError::Parse(e.to_string()))?;
            out.push(name);
        }
    }
    Ok(out)
}

/// Hierarchy hits as FastRefSite-shaped rows (class_name set; method fields empty).
///
/// `class_name` keeps the Dalvik descriptor so ASC-style CLIs can print it as-is.
pub fn hierarchy_as_sites(classes: &[String]) -> Vec<FastRefSite> {
    classes
        .iter()
        .map(|c| FastRefSite {
            class_name: c.clone(),
            method_name: String::new(),
            method_idx: 0,
            file_offset: 0,
            offset: 0,
            pool_idx: 0,
            class_idx: 0,
            method_idx_in_class: 0,
        })
        .collect()
}

/// One-DEX findrefs for ASC kinds (`string|type|method|field|extends|impl`).
pub fn findrefs_kind(
    dex: &DexFile,
    kind: &str,
    value: &str,
    class_desc: Option<&str>,
    exact_class: bool,
) -> Result<Vec<FastRefSite>> {
    match kind {
        "string" => find_string_xrefs(dex, value),
        "type" => find_type_xrefs(dex, value),
        "method" => find_method_xrefs_fast(dex, class_desc, Some(value), exact_class),
        "field" => find_field_xrefs_fast(dex, class_desc, Some(value), exact_class),
        "extends" => Ok(hierarchy_as_sites(&find_extends(dex, value)?)),
        "impl" => Ok(hierarchy_as_sites(&find_impls(dex, value)?)),
        other => Err(DexDecompilerError::Parse(format!(
            "unknown findrefs kind {other:?} (string|type|method|field|extends|impl)"
        ))),
    }
}

/// Multi-DEX findrefs. Parallel over DEX files on native; sequential on WASM.
pub fn findrefs_kind_on_dexes(
    dexes: &[DexFile],
    kind: &str,
    value: &str,
    class_desc: Option<&str>,
    exact_class: bool,
) -> Result<Vec<(usize, Vec<FastRefSite>)>> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        use rayon::prelude::*;
        dexes
            .par_iter()
            .enumerate()
            .map(|(dex_i, dex)| {
                findrefs_kind(dex, kind, value, class_desc, exact_class)
                    .map(|sites| (dex_i, sites))
            })
            .collect()
    }
    #[cfg(target_arch = "wasm32")]
    {
        let mut out = Vec::with_capacity(dexes.len());
        for (dex_i, dex) in dexes.iter().enumerate() {
            out.push((
                dex_i,
                findrefs_kind(dex, kind, value, class_desc, exact_class)?,
            ));
        }
        Ok(out)
    }
}

pub fn find_string_xrefs(dex: &DexFile, needle: &str) -> Result<Vec<FastRefSite>> {
    let raw = dex_parser::RawDex::from_dex(dex);
    let mut fr = dex_parser::FastRef::from_raw(raw);
    let sites = fr
        .find_strings(needle)
        .map_err(|e| DexDecompilerError::Parse(e.to_string()))?;
    fast_sites_to_ui(dex, sites)
}

pub fn find_type_xrefs(dex: &DexFile, needle: &str) -> Result<Vec<FastRefSite>> {
    let raw = dex_parser::RawDex::from_dex(dex);
    let mut fr = dex_parser::FastRef::from_raw(raw);
    let sites = fr
        .find_types(needle)
        .map_err(|e| DexDecompilerError::Parse(e.to_string()))?;
    fast_sites_to_ui(dex, sites)
}

pub fn find_field_xrefs_fast(
    dex: &DexFile,
    class: Option<&str>,
    name: Option<&str>,
    exact_class: bool,
) -> Result<Vec<FastRefSite>> {
    let raw = dex_parser::RawDex::from_dex(dex);
    let mut fr = dex_parser::FastRef::from_raw(raw);
    let q = dex_parser::MemberQuery {
        class: class.map(|c| (c.to_string(), exact_class)),
        name: name.map(|s| s.to_string()),
    };
    let sites = fr
        .find_fields(&q)
        .map_err(|e| DexDecompilerError::Parse(e.to_string()))?;
    fast_sites_to_ui(dex, sites)
}

/// ASC fast path: locate invoke / method-ref sites by optional class + method name.
pub fn find_method_xrefs_fast(
    dex: &DexFile,
    class: Option<&str>,
    name: Option<&str>,
    exact_class: bool,
) -> Result<Vec<FastRefSite>> {
    let raw = dex_parser::RawDex::from_dex(dex);
    let mut fr = dex_parser::FastRef::from_raw(raw);
    let q = dex_parser::MemberQuery {
        class: class.map(|c| (c.to_string(), exact_class)),
        name: name.map(|s| s.to_string()),
    };
    let sites = fr
        .find_methods(&q)
        .map_err(|e| DexDecompilerError::Parse(e.to_string()))?;
    fast_sites_to_ui(dex, sites)
}

fn fast_sites_to_ui(dex: &DexFile, sites: Vec<dex_parser::RefSite>) -> Result<Vec<FastRefSite>> {
    // Resolve class/method from method_ids only — do NOT walk class_data
    // (method_ui_map). That dominated WeChat-scale string findrefs with 100k+ hits.
    let mut out = Vec::with_capacity(sites.len());
    for s in sites {
        let info = dex.get_method_info(s.method_idx).ok();
        let method_name = info
            .as_ref()
            .map(|i| i.name.to_string())
            .unwrap_or_default();
        let class_name = info
            .map(|i| java::descriptor_to_java(&i.class))
            .unwrap_or_default();
        out.push(FastRefSite {
            class_name,
            method_name,
            method_idx: s.method_idx,
            file_offset: s.file_off,
            offset: s.insn_off,
            pool_idx: s.pool_idx,
            class_idx: 0,
            method_idx_in_class: 0,
        });
    }
    Ok(out)
}

/// Resolve containing method class+name from `method_ids` via [`RawDex`] (no full parse).
fn raw_sites_to_ui(data: &[u8], sites: Vec<dex_parser::RefSite>) -> Result<Vec<FastRefSite>> {
    let raw = dex_parser::RawDex::parse(data).map_err(|e| DexDecompilerError::Parse(e.to_string()))?;
    let mut out = Vec::with_capacity(sites.len());
    for s in sites {
        let (class_desc, method_name) = raw_method_names(&raw, s.method_idx).unwrap_or_default();
        out.push(FastRefSite {
            class_name: java::descriptor_to_java(&class_desc),
            method_name,
            method_idx: s.method_idx,
            file_offset: s.file_off,
            offset: s.insn_off,
            pool_idx: s.pool_idx,
            class_idx: 0,
            method_idx_in_class: 0,
        });
    }
    Ok(out)
}

fn raw_method_names(raw: &dex_parser::RawDex<'_>, method_idx: u32) -> Result<(String, String)> {
    if method_idx >= raw.method_ids.size {
        return Ok((String::new(), String::new()));
    }
    let off = raw.method_ids.off as usize + method_idx as usize * 8;
    let class_idx = raw
        .u16_at(off)
        .map_err(|e| DexDecompilerError::Parse(e.to_string()))? as u32;
    let name_idx = raw
        .u32_at(off + 4)
        .map_err(|e| DexDecompilerError::Parse(e.to_string()))?;
    let desc_idx = raw
        .type_descriptor_idx(class_idx)
        .map_err(|e| DexDecompilerError::Parse(e.to_string()))?;
    let class_bytes = raw
        .string_payload_bytes(desc_idx)
        .map_err(|e| DexDecompilerError::Parse(e.to_string()))?;
    let name_bytes = raw
        .string_payload_bytes(name_idx)
        .map_err(|e| DexDecompilerError::Parse(e.to_string()))?;
    let class = dex_parser::dex::decode_mutf8(class_bytes).unwrap_or_default();
    let name = dex_parser::dex::decode_mutf8(name_bytes).unwrap_or_default();
    Ok((class, name))
}

/// FastRefs on raw DEX bytes (no full [`DexFile`] parse).
pub fn findrefs_kind_on_bytes(
    data: &[u8],
    kind: &str,
    value: &str,
    class_desc: Option<&str>,
    exact_class: bool,
) -> Result<Vec<FastRefSite>> {
    let mut fr =
        dex_parser::FastRef::new(data).map_err(|e| DexDecompilerError::Parse(e.to_string()))?;
    let sites = match kind {
        "string" => fr
            .find_strings(value)
            .map_err(|e| DexDecompilerError::Parse(e.to_string()))?,
        "type" => fr
            .find_types(value)
            .map_err(|e| DexDecompilerError::Parse(e.to_string()))?,
        "method" => {
            let q = dex_parser::MemberQuery {
                class: class_desc.map(|c| (c.to_string(), exact_class)),
                name: Some(value.to_string()),
            };
            fr.find_methods(&q)
                .map_err(|e| DexDecompilerError::Parse(e.to_string()))?
        }
        "field" => {
            let q = dex_parser::MemberQuery {
                class: class_desc.map(|c| (c.to_string(), exact_class)),
                name: Some(value.to_string()),
            };
            fr.find_fields(&q)
                .map_err(|e| DexDecompilerError::Parse(e.to_string()))?
        }
        "extends" | "impl" => {
            // Hierarchy needs class_defs iteration; fall back to light DexFile parse.
            let dex = crate::parse_dex(data)?;
            return findrefs_kind(&dex, kind, value, class_desc, exact_class);
        }
        other => {
            return Err(DexDecompilerError::Parse(format!(
                "unknown findrefs kind {other:?} (string|type|method|field|extends|impl)"
            )));
        }
    };
    raw_sites_to_ui(data, sites)
}

#[cfg(test)]
mod tests {
    use super::invoke_method_index;

    #[test]
    fn invoke_virtual_reads_method_index() {
        // invoke-virtual {v0}, method@0x1234  — F35c
        let bytes = [0x6e, 0x10, 0x34, 0x12, 0x00, 0x00];
        assert_eq!(invoke_method_index(&bytes, 0, 0x6e), Some(0x1234));
    }

    #[test]
    fn invoke_static_range_reads_method_index() {
        let bytes = [0x77, 0x02, 0xcd, 0xab, 0x00, 0x00];
        assert_eq!(invoke_method_index(&bytes, 0, 0x77), Some(0xabcd));
    }

    #[test]
    fn invoke_custom_skipped() {
        let bytes = [0xfc, 0x10, 0x34, 0x12, 0x00, 0x00];
        assert_eq!(invoke_method_index(&bytes, 0, 0xfc), None);
    }
}
