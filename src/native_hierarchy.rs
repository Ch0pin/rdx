//! Immutable, bounded type relationships assembled from all parsed DEX classes.
//!
//! A missing external definition is never guessed from its name. The small
//! platform graph below only records relationships guaranteed by the Java type
//! hierarchy and needed to anchor exception ancestry.

#[path = "native_cast_calls.rs"]
mod cast_calls;
#[path = "native_constructor_recovery.rs"]
mod constructor_recovery;
#[path = "native_future_headers.rs"]
mod future_headers;
#[path = "native_static_no_return.rs"]
mod static_no_return;
#[path = "native_this_receiver_identity.rs"]
pub(crate) mod this_receiver_identity;
use crate::native_dex::DexClass;
use anyhow::{Result, ensure};
pub use constructor_recovery::RecoveredConstructor;
pub(crate) use constructor_recovery::platform_member_type;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};

const MAX_CLASSES: usize = 1_000_000;
const MAX_EDGES: usize = 4_000_000;
const MAX_QUERY_NODES: usize = 131_072;
const MAX_QUERY_EDGES: usize = 524_288;
const MAX_CACHE_ENTRIES: usize = 8_192;
const MAX_CACHE_KEY_BYTES: usize = 512;
type WritableFieldIndex = HashMap<Arc<str>, HashMap<Arc<str>, Arc<str>>>;
type WritableFieldQueries = HashMap<Arc<str>, HashSet<Arc<str>>>;
type SneakyReadableFieldIndex = HashMap<Arc<str>, HashMap<Arc<str>, Arc<str>>>;

/// Populate additional declaration queries only for typed constructor bodies
/// newly entering the strict protected-final proof. Existing catchall queries
/// have priority; an exhausted added pass never replaces their authority.
fn seed_typed_constructor_field_queries(
    classes: &[&DexClass],
    queries: &mut WritableFieldQueries,
    seed_words: &mut usize,
    seed_queries: &mut usize,
) {
    let mut added: WritableFieldQueries = HashMap::new();
    let mut added_words = 0usize;
    let mut added_queries = 0usize;
    let mut prefilter_words = 0usize;
    for class in classes {
        if class.fields.len() > 256 {
            continue;
        }
        let finals: HashSet<_> = class
            .fields
            .iter()
            .filter(|field| !field.is_static && field.access_flags & 16 != 0)
            .map(|field| (field.name.as_ref(), field.field_type.as_ref()))
            .collect();
        if finals.is_empty() || finals.len() > 64 {
            continue;
        }
        for method in &class.methods {
            let Some(code) = method.code.as_ref() else {
                continue;
            };
            if method.name.as_ref() != "<init>"
                || method.access_flags & 8 != 0
                || code.instructions.len() > 4096
                || code.registers > 256
                || code.try_regions.is_empty()
                || code.try_regions.len() > 16
                || code
                    .try_regions
                    .iter()
                    .map(|r| r.catches.len())
                    .sum::<usize>()
                    > 64
                || code
                    .try_regions
                    .iter()
                    .any(|r| r.catches.iter().any(|(ty, _)| ty.is_none()))
                || code
                    .instructions
                    .len()
                    .checked_mul(code.try_regions.len() + finals.len())
                    .is_none_or(|n| n > 20_000)
            {
                continue;
            }
            prefilter_words = prefilter_words.saturating_add(code.instructions.len());
            if prefilter_words > MAX_QUERY_NODES * 8 {
                return;
            }
            let own_final = |index: usize| {
                class
                    .symbols
                    .fields
                    .get(index)
                    .is_some_and(|&(owner, ty, name)| {
                        let (Some(owner), Some(ty), Some(name)) = (
                            class.symbols.types.get(owner as usize),
                            class.symbols.types.get(ty as usize),
                            class.symbols.strings.get(name as usize),
                        ) else {
                            return false;
                        };
                        owner == &class.descriptor && finals.contains(&(name.as_str(), ty.as_ref()))
                    })
            };
            // A raw superset avoids decoding ordinary typed constructors.
            // Canonical instruction boundaries confirm the store before use.
            if !code.instructions.windows(2).enumerate().any(|(pc, words)| {
                matches!(words[0] as u8, 0x59..=0x5f)
                    && code
                        .try_regions
                        .iter()
                        .any(|r| r.start as usize <= pc && pc < r.end as usize)
                    && own_final(words[1] as usize)
            }) {
                continue;
            }
            added_words = added_words.saturating_add(code.instructions.len());
            if seed_words.saturating_add(added_words) > MAX_QUERY_NODES {
                return;
            }
            let Ok(ir) = crate::native_ir::DecodedMethod::decode(code) else {
                continue;
            };
            if !ir.instructions.iter().any(|i| {
                matches!(i.opcode, 0x59..=0x5f)
                    && code
                        .try_regions
                        .iter()
                        .any(|r| r.start as usize <= i.pc && i.pc < r.end as usize)
                    && i.reference.is_some_and(|r| own_final(r.index as usize))
            }) {
                continue;
            }
            for instruction in &ir.instructions {
                if !matches!(instruction.opcode, 0x59..=0x5f) {
                    continue;
                }
                let Some((owner, name)) = instruction
                    .reference
                    .and_then(|r| class.symbols.fields.get(r.index as usize))
                    .and_then(|&(owner, _, name)| {
                        class
                            .symbols
                            .types
                            .get(owner as usize)
                            .zip(class.symbols.strings.get(name as usize))
                    })
                else {
                    continue;
                };
                if owner == &class.descriptor
                    || owner.len() > MAX_CACHE_KEY_BYTES
                    || name.len() > MAX_CACHE_KEY_BYTES
                    || queries
                        .get(owner)
                        .is_some_and(|names| names.contains(name.as_str()))
                {
                    continue;
                }
                let names = added.entry(owner.clone()).or_default();
                if names.insert(Arc::from(name.as_str())) {
                    added_queries += 1;
                    if seed_queries.saturating_add(added_queries) > MAX_CACHE_ENTRIES
                        || queries.get(owner).map_or(0, HashSet::len) + names.len() > 256
                    {
                        return;
                    }
                }
            }
        }
    }
    // Declaration validation below is unchanged. Queries grant no permission.
    for (owner, names) in added {
        queries.entry(owner).or_default().extend(names);
    }
    *seed_words += added_words;
    *seed_queries += added_queries;
}

#[path = "native_checked_call_certificates.rs"]
mod checked_call_certificates;
#[path = "native_enum_array_retainer.rs"]
mod enum_array_retainer;

// Large multidex APKs can exceed 390k families. Keep a finite certificate
// budget while allowing their argument-cast proofs to remain available.
const MAX_READABILITY_FAMILIES: usize = 512_000;
const OBJECT: &str = "Ljava/lang/Object;";

pub(crate) fn object_method_signature(signature: &str) -> bool {
    matches!(
        signature,
        "clone()"
            | "equals(Ljava/lang/Object;)"
            | "finalize()"
            | "getClass()"
            | "hashCode()"
            | "notify()"
            | "notifyAll()"
            | "toString()"
            | "wait()"
            | "wait(J)"
            | "wait(JI)"
    )
}

fn known_platform_interface_excludes_signature(owner: &str, signature: &str) -> bool {
    match owner {
        // Marker interfaces have no methods to constrain a declaration.
        "Ljava/io/Serializable;" | "Ljava/lang/Cloneable;" => true,
        // Comparable<T> erases to this one method. Other names are unrelated.
        "Ljava/lang/Comparable;" => signature != "compareTo(Ljava/lang/Object;)",
        _ => false,
    }
}

// Complete public/protected member sets from class files extracted directly
// from Android API 35 android.jar (SHA-256 4566663c3876e022b4fa4ced8c8697c4ab1688267f090114fd92d027b32e619b).
// `javap -sysinfo -s` on those files avoids host-JDK java.* substitution.
// Keeping the full sets makes an absent signature a proof of absence.
#[derive(Clone, Copy)]
enum PinnedIoMember {
    Absent,
    Throws(&'static str),
    NoThrows,
    Mismatch,
}

const PINNED_IO_CONTRACT_OWNERS: &[&str] = &[
    "Ljava/io/DataInput;",
    "Ljava/io/InputStream;",
    "Ljava/io/FilterInputStream;",
    "Ljava/io/Closeable;",
    "Ljava/io/Flushable;",
    "Ljava/io/Reader;",
    "Ljava/lang/Readable;",
    "Ljava/nio/channels/ByteChannel;",
    "Ljava/nio/channels/Channel;",
    "Ljava/nio/channels/ReadableByteChannel;",
    "Ljava/nio/channels/WritableByteChannel;",
];

fn pinned_io_member(owner: &str, signature: &str, ret: &str) -> Option<PinnedIoMember> {
    const IO: &str = "Ljava/io/IOException;";
    const EXCEPTION: &str = "Ljava/lang/Exception;";
    type Entry = (&'static str, &'static str, Option<&'static str>, bool);
    const DATA_INPUT: &[Entry] = &[
        ("readFully([B)", "V", Some(IO), false),
        ("readFully([BII)", "V", Some(IO), false),
        ("skipBytes(I)", "I", Some(IO), false),
        ("readBoolean()", "Z", Some(IO), false),
        ("readByte()", "B", Some(IO), false),
        ("readUnsignedByte()", "I", Some(IO), false),
        ("readShort()", "S", Some(IO), false),
        ("readUnsignedShort()", "I", Some(IO), false),
        ("readChar()", "C", Some(IO), false),
        ("readInt()", "I", Some(IO), false),
        ("readLong()", "J", Some(IO), false),
        ("readFloat()", "F", Some(IO), false),
        ("readDouble()", "D", Some(IO), false),
        ("readLine()", "Ljava/lang/String;", Some(IO), false),
        ("readUTF()", "Ljava/lang/String;", Some(IO), false),
    ];
    const INPUT_STREAM: &[Entry] = &[
        ("<init>()", "V", None, false),
        ("nullInputStream()", "Ljava/io/InputStream;", None, true),
        ("read()", "I", Some(IO), false),
        ("read([B)", "I", Some(IO), false),
        ("read([BII)", "I", Some(IO), false),
        ("readAllBytes()", "[B", Some(IO), false),
        ("readNBytes(I)", "[B", Some(IO), false),
        ("readNBytes([BII)", "I", Some(IO), false),
        ("skip(J)", "J", Some(IO), false),
        ("skipNBytes(J)", "V", Some(IO), false),
        ("available()", "I", Some(IO), false),
        ("close()", "V", Some(IO), false),
        ("mark(I)", "V", None, false),
        ("reset()", "V", Some(IO), false),
        ("markSupported()", "Z", None, false),
        ("transferTo(Ljava/io/OutputStream;)", "J", Some(IO), false),
    ];
    const FILTER_INPUT_STREAM: &[Entry] = &[
        ("<init>(Ljava/io/InputStream;)", "V", None, false),
        ("read()", "I", Some(IO), false),
        ("read([B)", "I", Some(IO), false),
        ("read([BII)", "I", Some(IO), false),
        ("skip(J)", "J", Some(IO), false),
        ("available()", "I", Some(IO), false),
        ("close()", "V", Some(IO), false),
        ("mark(I)", "V", None, false),
        ("reset()", "V", Some(IO), false),
        ("markSupported()", "Z", None, false),
    ];
    const CLOSEABLE: &[Entry] = &[("close()", "V", Some(IO), false)];
    const AUTO_CLOSEABLE: &[Entry] = &[("close()", "V", Some(EXCEPTION), false)];
    // Exhaustive public/protected local declarations from Android API 35
    // android.jar SHA-256 4566663c3876e022b4fa4ced8c8697c4ab1688267f090114fd92d027b32e619b.
    // Ancestors are traversed separately; ByteChannel declares no local methods.
    const FLUSHABLE: &[Entry] = &[("flush()", "V", Some(IO), false)];
    const READER: &[Entry] = &[
        ("<init>()", "V", None, false),
        ("<init>(Ljava/lang/Object;)", "V", None, false),
        ("nullReader()", "Ljava/io/Reader;", None, true),
        ("read(Ljava/nio/CharBuffer;)", "I", Some(IO), false),
        ("read()", "I", Some(IO), false),
        ("read([C)", "I", Some(IO), false),
        ("read([CII)", "I", Some(IO), false),
        ("skip(J)", "J", Some(IO), false),
        ("ready()", "Z", Some(IO), false),
        ("markSupported()", "Z", None, false),
        ("mark(I)", "V", Some(IO), false),
        ("reset()", "V", Some(IO), false),
        ("close()", "V", Some(IO), false),
        ("transferTo(Ljava/io/Writer;)", "J", Some(IO), false),
    ];
    const READABLE: &[Entry] = &[("read(Ljava/nio/CharBuffer;)", "I", Some(IO), false)];
    const BYTECHANNEL: &[Entry] = &[];
    const CHANNEL: &[Entry] = &[
        ("isOpen()", "Z", None, false),
        ("close()", "V", Some(IO), false),
    ];
    const READABLEBYTECHANNEL: &[Entry] = &[("read(Ljava/nio/ByteBuffer;)", "I", Some(IO), false)];
    const WRITABLEBYTECHANNEL: &[Entry] = &[("write(Ljava/nio/ByteBuffer;)", "I", Some(IO), false)];
    let members = match owner {
        "Ljava/io/DataInput;" => DATA_INPUT,
        "Ljava/io/Flushable;" => FLUSHABLE,
        "Ljava/io/Reader;" => READER,
        "Ljava/lang/Readable;" => READABLE,
        "Ljava/nio/channels/ByteChannel;" => BYTECHANNEL,
        "Ljava/nio/channels/Channel;" => CHANNEL,
        "Ljava/nio/channels/ReadableByteChannel;" => READABLEBYTECHANNEL,
        "Ljava/nio/channels/WritableByteChannel;" => WRITABLEBYTECHANNEL,

        "Ljava/io/InputStream;" => INPUT_STREAM,
        "Ljava/io/FilterInputStream;" => FILTER_INPUT_STREAM,
        "Ljava/io/Closeable;" => CLOSEABLE,
        "Ljava/lang/AutoCloseable;" => AUTO_CLOSEABLE,
        _ => return None,
    };
    Some(
        match members.iter().find(|(name, _, _, _)| *name == signature) {
            None => PinnedIoMember::Absent,
            Some((_, expected, _, true)) if *expected != ret => PinnedIoMember::Mismatch,
            Some((_, _, _, true)) => PinnedIoMember::Mismatch,
            Some((_, expected, _, _)) if *expected != ret => PinnedIoMember::Mismatch,
            Some((_, _, Some(ty), _)) => PinnedIoMember::Throws(ty),
            Some(_) => PinnedIoMember::NoThrows,
        },
    )
}

fn io_method_key(class: &DexClass, method: &crate::native_dex::DexMethod) -> String {
    format!(
        "{}->{}({}){}",
        class.descriptor,
        method.name,
        method
            .parameters
            .iter()
            .map(AsRef::as_ref)
            .collect::<String>(),
        method.return_type
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Relation {
    Proven,
    Disproven,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TypeEntry {
    superclass: Option<Arc<str>>,
    interfaces: Arc<[Arc<str>]>,
}

type DeclaredThrows = HashMap<String, (bool, Arc<[Arc<str>]>)>;

#[derive(Debug, Clone)]
struct MethodContract {
    access_flags: u32,
    return_type: Arc<str>,
    thrown_types: Arc<[Arc<str>]>,
    inferred_throws: bool,
    owner_final: bool,
}

// An Object-root method with no competing interface declaration can legally
// print exceptions from exact uncaught static calls. This summary is used only
// as an inherited override contract; it does not imply arbitrary call-graph
// propagation or prove whole-class Java compilation.
fn inferred_root_static_call_throws(
    class: &DexClass,
    method: &crate::native_dex::DexMethod,
    signature: &str,
    loaded: &HashMap<&str, &DexClass>,
) -> Vec<Arc<str>> {
    if class.superclass.as_deref() != Some(OBJECT)
        || object_method_signature(signature)
        || !class.interfaces.iter().all(|interface| {
            !loaded.contains_key(interface.as_ref())
                && known_platform_interface_excludes_signature(interface, signature)
        })
    {
        return Vec::new();
    }
    let Some(code) = method.code.as_ref() else {
        return Vec::new();
    };
    if !code.try_regions.is_empty() {
        return Vec::new();
    }
    let Ok(decoded) = crate::native_ir::DecodedMethod::decode(code) else {
        return Vec::new();
    };
    let mut types = HashSet::new();
    for instruction in decoded.instructions {
        if matches!(instruction.opcode, 0x71 | 0x77) {
            let Some(&method_index) = code.instructions.get(instruction.pc + 1) else {
                return Vec::new();
            };
            let Some(&(owner_index, proto_index, name_index)) =
                class.symbols.methods.get(method_index as usize)
            else {
                return Vec::new();
            };
            let (Some(owner), Some((ret, args)), Some(name)) = (
                class.symbols.types.get(owner_index as usize),
                class.symbols.protos.get(proto_index as usize),
                class.symbols.strings.get(name_index as usize),
            ) else {
                return Vec::new();
            };
            if !loaded.contains_key(owner.as_ref()) {
                let key = format!(
                    "{owner}->{name}({}){ret}",
                    args.iter().map(AsRef::as_ref).collect::<String>()
                );
                if let Some((true, declared)) = platform_exceptions().methods.get(&key) {
                    types.extend(declared.iter().map(|ty| Arc::<str>::from(ty.as_str())));
                }
            }
        }
    }
    types.into_iter().collect()
}

#[derive(Debug, Default, Clone)]
struct CheckedCallerIndex {
    ready: bool,
    complete: bool,
    candidates: HashSet<String>,
    blocked: HashSet<String>,
    // One small set of declared/caught supertypes per exact loaded call site.
    conditional: HashMap<String, Vec<CheckedCallerSite>>,
}

#[derive(Debug, Clone)]
struct CheckedCallerSite {
    types: Vec<Arc<str>>,
    inherited_io_caller: Option<String>,
    future_caller: Option<(String, Vec<Arc<str>>)>,
}

struct CallerProofBudget {
    active: HashSet<String>,
    completed: HashMap<String, bool>,
    remaining_work: usize,
    max_depth: usize,
}

impl CheckedCallerIndex {
    fn permits(&self, key: &str, exception: &str, hierarchy: &TypeHierarchy) -> bool {
        const MAX_CALLER_DEPTH: usize = 64;
        const MAX_CALLER_WORK: usize = 100_000;
        if !self.ready {
            return true; // Only while building the index from provisional contracts.
        }
        let mut proof = CallerProofBudget {
            active: HashSet::new(),
            completed: HashMap::new(),
            remaining_work: MAX_CALLER_WORK,
            max_depth: MAX_CALLER_DEPTH,
        };
        self.permits_inner(key, exception, hierarchy, &mut proof)
    }

    fn permits_inner(
        &self,
        key: &str,
        exception: &str,
        hierarchy: &TypeHierarchy,
        proof: &mut CallerProofBudget,
    ) -> bool {
        if let Some(&permitted) = proof.completed.get(key) {
            return permitted;
        }
        if proof.active.len() >= proof.max_depth
            || proof.remaining_work == 0
            || !proof.active.insert(key.to_owned())
        {
            return false;
        }
        proof.remaining_work -= 1;
        let permitted = self.complete
            && self.candidates.contains(key)
            && !self.blocked.contains(key)
            && self.conditional.get(key).is_none_or(|sites| {
                sites.iter().all(|site| {
                    if proof.remaining_work == 0 {
                        return false;
                    }
                    proof.remaining_work -= 1;
                    site.types
                        .iter()
                        .any(|ty| hierarchy.assignable(exception, ty) == Relation::Proven)
                        || site.future_caller.as_ref().is_some_and(|(caller, types)| {
                            types
                                .iter()
                                .any(|ty| hierarchy.assignable(exception, ty) == Relation::Proven)
                                && self.permits_inner(caller, exception, hierarchy, proof)
                        })
                        || (hierarchy.assignable(exception, "Ljava/io/IOException;")
                            == Relation::Proven
                            && site.inherited_io_caller.as_ref().is_some_and(|caller| {
                                self.permits_inner(caller, exception, hierarchy, proof)
                            }))
                })
            });
        proof.active.remove(key);
        proof.completed.insert(key.to_owned(), permitted);
        permitted
    }
}

// A newly printed checked declaration changes Java callers. Index only
// methods that can infer such a declaration, matching exact call sites after
// a raw invoke+method-ID prefilter. The symbol-ID map is shared per DEX and
// the call index is built once per hierarchy, never per rendered method.
fn checked_caller_index(hierarchy: &TypeHierarchy, classes: &[&DexClass]) -> CheckedCallerIndex {
    const IO: &str = "Ljava/io/IOException;";
    // Large multidex apps can exceed 65k candidates before any calls are indexed.
    const MAX_CANDIDATES: usize = 262_144;
    const MAX_TABLES: usize = 256;
    const MAX_SYMBOLS: usize = 2_000_000;
    const MAX_WORDS: usize = 64_000_000;
    const MAX_DECODED: usize = 262_144;
    const MAX_CALLS: usize = 500_000;
    const MAX_ALIAS_CHECKS: usize = 100_000;
    const MAX_ALIAS_STEPS: usize = 32_000_000;
    let mut index = CheckedCallerIndex {
        ready: true,
        complete: true,
        ..Default::default()
    };
    let mut candidates = HashSet::new();
    for class in classes {
        if hierarchy.ambiguous.contains(class.descriptor.as_ref())
            || hierarchy
                .override_ambiguous_owners
                .contains(class.descriptor.as_ref())
        {
            continue;
        }
        for method in &class.methods {
            if method.declaring_type != class.descriptor
                || method.access_flags & 2 != 0
                || method.name.as_ref() == "<clinit>"
                || !method.thrown_types.is_empty()
            {
                continue;
            }
            let Some(code) = method.code.as_ref() else {
                continue;
            };
            if method.name.as_ref() != "<init>"
                && method.access_flags & 8 == 0
                && class.access_flags & 0x10 == 0
                && hierarchy.raw_future_header(class, method).is_empty()
            {
                continue;
            }
            let signature = format!(
                "{}({})",
                method.name,
                method
                    .parameters
                    .iter()
                    .map(AsRef::as_ref)
                    .collect::<String>()
            );
            let pinned_io = PINNED_IO_CONTRACT_OWNERS.iter().any(|owner| {
                matches!(
                    pinned_io_member(owner, &signature, &method.return_type),
                    Some(PinnedIoMember::Throws(IO))
                ) && hierarchy.assignable(&class.descriptor, owner) == Relation::Proven
            });
            let raw_inference = code
                .instructions
                .iter()
                .any(|word| matches!((*word & 0xff) as u8, 0x27 | 0x71 | 0x77));
            if pinned_io || raw_inference || !hierarchy.raw_future_header(class, method).is_empty()
            {
                candidates.insert(io_method_key(class, method));
            }
        }
    }
    if candidates.is_empty() {
        return index;
    }
    if candidates.len() > MAX_CANDIDATES {
        index.complete = false;
        return index;
    }
    // A static method can be named through a subclass. Its declaring class
    // need not be final, but every symbolic alias must resolve through a
    // complete loaded superclass chain without an intervening hiding method.
    let loaded: HashMap<&str, &DexClass> = classes
        .iter()
        .map(|class| (class.descriptor.as_ref(), *class))
        .collect();
    let mut static_signatures: HashMap<String, Vec<(&str, &str)>> = HashMap::new();
    for class in classes {
        if class.access_flags & 0x10 != 0 {
            continue;
        }
        for method in &class.methods {
            if method.access_flags & 8 == 0 {
                continue;
            }
            let key = io_method_key(class, method);
            if let Some(candidate) = candidates.get(&key) {
                let tail = key.split_once("->").unwrap().1.to_owned();
                static_signatures
                    .entry(tail)
                    .or_default()
                    .push((class.descriptor.as_ref(), candidate.as_str()));
            }
        }
    }
    let owners: HashSet<&str> = candidates
        .iter()
        .filter_map(|key| key.split_once("->").map(|(owner, _)| owner))
        .collect();
    let mut call_count = 0usize;
    let mut symbol_refs: HashMap<usize, HashMap<usize, &str>> = HashMap::new();
    let mut alias_cache: HashMap<String, Option<&str>> = HashMap::new();
    let mut symbols_scanned = 0usize;
    let mut words_scanned = 0usize;
    let mut decoded_count = 0usize;
    let mut alias_checks = 0usize;
    let mut alias_steps = 0usize;
    let mut alias_exhausted = false;
    for class in classes {
        // DexSymbols is shared by every class in one DEX. Index its method
        // IDs once, then check raw call sites before decoding any body.
        let symbols_id = Arc::as_ptr(&class.symbols) as usize;
        if !symbol_refs.contains_key(&symbols_id) && symbol_refs.len() == MAX_TABLES {
            index.complete = false;
            return index;
        }
        let refs = symbol_refs.entry(symbols_id).or_insert_with(|| {
            symbols_scanned = symbols_scanned.saturating_add(class.symbols.methods.len());
            if static_signatures.is_empty()
                && !class
                    .symbols
                    .types
                    .iter()
                    .any(|ty| owners.contains(ty.as_ref()))
            {
                return HashMap::new();
            }
            class
                .symbols
                .methods
                .iter()
                .enumerate()
                .filter_map(|(method_id, &(owner_index, proto_index, name_index))| {
                    let owner = class.symbols.types.get(owner_index as usize)?;
                    let (ret, args) = class.symbols.protos.get(proto_index as usize)?;
                    let name = class.symbols.strings.get(name_index as usize)?;
                    let signature = format!(
                        "{name}({}){ret}",
                        args.iter().map(AsRef::as_ref).collect::<String>()
                    );
                    if owners.contains(owner.as_ref()) {
                        let key = format!("{owner}->{signature}");
                        if let Some(target) = candidates.get(&key) {
                            return Some((method_id, target.as_str()));
                        }
                    }
                    let aliases = static_signatures.get(&signature)?;
                    let alias_key = format!("{owner}->{signature}");
                    if let Some(&resolved) = alias_cache.get(&alias_key) {
                        return resolved.map(|target| (method_id, target));
                    }
                    if alias_checks == MAX_ALIAS_CHECKS {
                        alias_exhausted = true;
                        return None;
                    }
                    alias_checks += 1;
                    let mut current = owner.as_ref();
                    let mut seen = HashSet::new();
                    let resolved = loop {
                        if current == OBJECT {
                            break None;
                        }
                        if !seen.insert(current)
                            || hierarchy.ambiguous.contains(current)
                            || hierarchy.override_ambiguous_owners.contains(current)
                        {
                            index.blocked.extend(
                                aliases.iter().map(|(_, candidate)| (*candidate).to_owned()),
                            );
                            break None;
                        }
                        let Some(ancestor) = loaded.get(current) else {
                            index.blocked.extend(
                                aliases.iter().map(|(_, candidate)| (*candidate).to_owned()),
                            );
                            break None;
                        };
                        let Some(work) = alias_steps.checked_add(ancestor.methods.len() + 1) else {
                            alias_exhausted = true;
                            return None;
                        };
                        if work > MAX_ALIAS_STEPS {
                            alias_exhausted = true;
                            return None;
                        }
                        alias_steps = work;
                        if let Some(member) = ancestor.methods.iter().find(|method| {
                            method.name.as_ref() == name.as_str()
                                && method.parameters.as_slice() == args.as_slice()
                        }) {
                            if member.access_flags & 8 == 0
                                || member.return_type.as_ref() != ret.as_ref()
                            {
                                index.blocked.extend(
                                    aliases.iter().map(|(_, candidate)| (*candidate).to_owned()),
                                );
                                break None;
                            }
                            let key = io_method_key(ancestor, member);
                            break candidates.get(&key).map(|target| target.as_str());
                        }
                        let Some(parent) = ancestor.superclass.as_deref() else {
                            index.blocked.extend(
                                aliases.iter().map(|(_, candidate)| (*candidate).to_owned()),
                            );
                            break None;
                        };
                        current = parent;
                    };
                    alias_cache.insert(alias_key, resolved);
                    resolved.map(|target| (method_id, target))
                })
                .collect()
        });
        if symbols_scanned > MAX_SYMBOLS || alias_exhausted {
            index.complete = false;
            return index;
        }
        if refs.is_empty() {
            continue;
        }
        for method in &class.methods {
            let Some(code) = method.code.as_ref() else {
                continue;
            };
            words_scanned = words_scanned.saturating_add(code.instructions.len());
            if words_scanned > MAX_WORDS {
                index.complete = false;
                return index;
            }
            if !code.instructions.iter().enumerate().any(|(pc, word)| {
                matches!((*word & 0xff) as u8, 0x6e..=0x72 | 0x74..=0x78)
                    && code
                        .instructions
                        .get(pc + 1)
                        .is_some_and(|index| refs.contains_key(&(*index as usize)))
            }) {
                continue;
            }
            decoded_count += 1;
            if decoded_count > MAX_DECODED {
                index.complete = false;
                return index;
            }
            let Ok(decoded) = crate::native_ir::DecodedMethod::decode(code) else {
                index
                    .blocked
                    .extend(refs.values().map(|key| (*key).to_owned()));
                continue;
            };
            for instruction in decoded.instructions {
                if !matches!(instruction.opcode, 0x6e..=0x72 | 0x74..=0x78) {
                    continue;
                }
                let Some(&method_index) = code.instructions.get(instruction.pc + 1) else {
                    index
                        .blocked
                        .extend(refs.values().map(|key| (*key).to_owned()));
                    continue;
                };
                let Some(&callee) = refs.get(&(method_index as usize)) else {
                    continue;
                };
                call_count += 1;
                if call_count > MAX_CALLS {
                    index.complete = false;
                    return index;
                }
                let mut types = method.thrown_types.clone();
                // Java cannot wrap a `super(...)` or `this(...)` delegation
                // in a try block. A DEX handler around that invoke is not a
                // sufficient source-level caller proof.
                if !(method.name.as_ref() == "<init>" && callee.contains("-><init>(")) {
                    for region in &code.try_regions {
                        if region.start as usize <= instruction.pc
                            && instruction.pc < region.end as usize
                        {
                            types.extend(region.catches.iter().map(|(ty, _)| {
                                ty.clone()
                                    .unwrap_or_else(|| Arc::from("Ljava/lang/Throwable;"))
                            }));
                        }
                    }
                }
                let future_types = hierarchy.raw_future_header(class, method);
                let future_caller = (!future_types.is_empty()).then(|| {
                    (
                        io_method_key(class, method),
                        future_types.into_iter().map(Arc::from).collect(),
                    )
                });
                let inherited_io_caller =
                    if types.is_empty() && hierarchy.inherited_io_exception(class, method) {
                        Some(io_method_key(class, method))
                    } else {
                        None
                    };
                if types.is_empty() && inherited_io_caller.is_none() && future_caller.is_none() {
                    index.blocked.insert(callee.to_owned());
                } else {
                    index
                        .conditional
                        .entry(callee.to_owned())
                        .or_default()
                        .push(CheckedCallerSite {
                            types,
                            inherited_io_caller,
                            future_caller,
                        });
                }
            }
        }
    }
    drop(symbol_refs);
    drop(owners);
    index.candidates = candidates;
    index
}

/// Read-only project-wide class hierarchy. It is cheap to share behind an
/// `Arc<TypeHierarchy>` from each DEX symbol table.
#[derive(Debug, Clone)]
pub struct TypeHierarchy {
    entries: Arc<HashMap<Arc<str>, TypeEntry>>,
    ambiguous: Arc<HashSet<Arc<str>>>,
    cache: Arc<Mutex<RelationCache>>,
    trivial_constructors: Arc<HashMap<Arc<str>, Arc<str>>>,
    fresh_null_constructors: Arc<HashSet<Arc<str>>>,
    recovered_constructors: Arc<HashMap<Arc<str>, Vec<RecoveredConstructor>>>,
    accessible_noarg_superclasses: Arc<HashSet<Arc<str>>>,
    object_varargs: Arc<HashSet<String>>,
    object_calls: Arc<HashSet<String>>,
    static_throws: Arc<DeclaredThrows>,
    method_contracts: Arc<HashMap<String, MethodContract>>,
    ambiguous_method_contracts: Arc<HashSet<String>>,
    indexed_candidate_contracts: Arc<HashSet<String>>,
    checked_callers: Arc<CheckedCallerIndex>,
    override_declarations: Arc<HashSet<String>>,
    override_ambiguous_owners: Arc<HashSet<Arc<str>>>,
    loaded_owners: Arc<HashSet<Arc<str>>>,
    writable_fields: Arc<WritableFieldIndex>,
    sneaky_readable_fields: Arc<SneakyReadableFieldIndex>,
    loaded_call_families: Arc<HashMap<Arc<str>, LoadedCallOwner>>,
    static_sneaky_source_headers: Arc<HashSet<String>>,
    enum_array_retainers: Arc<HashSet<Arc<str>>>,
    checked_call_certificates: Arc<checked_call_certificates::Certificates>,
    noninstantiable_owners: Arc<HashSet<Arc<str>>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExactSourceDeclaration {
    pub owner_access_flags: u32,
    pub member_access_flags: u32,
    pub thrown_types: Vec<Arc<str>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExactAbstractObligation {
    pub name: Arc<str>,
    pub parameters: Vec<Arc<str>>,
    pub return_type: Arc<str>,
}

type ExactOverloadContracts = HashMap<Arc<str>, HashMap<Vec<Arc<str>>, Option<LoadedCallMethod>>>;

#[derive(Debug)]
struct LoadedCallOwner {
    access_flags: u32,
    erased_interface_source_header: bool,
    public: bool,
    root_object: bool,
    generic_class: bool,
    varargs_names: HashSet<Arc<str>>,
    // None means at least two declarations share a Java name and arity.
    methods: HashMap<Arc<str>, HashMap<usize, Option<LoadedCallMethod>>>,
    exact_overloads: Option<Box<ExactOverloadContracts>>,
}

#[derive(Debug)]
struct LoadedCallMethod {
    static_no_return: bool,
    declared_throws_empty: bool,
    generic_method: bool,
    legal_source_declaration: bool,
    no_new_checked_header: bool,
    future_checked_effects_closed: bool,
    may_infer_checked: bool,
    parameters: Vec<Arc<str>>,
    return_type: Arc<str>,
    access_flags: u32,
    annotated: bool,
}

// Only a complete bounded original-width walk can discard a raw-word seed.
// Payloads and malformed instructions retain the conservative unknown contract.
fn complete_no_inference_trigger(words: &[u16]) -> bool {
    if words.is_empty() || words.len() > 4096 {
        return false;
    }
    let mut pc = 0;
    while pc < words.len() {
        let Ok((width, payload)) = crate::native_cfg::instruction_width(words, pc) else {
            return false;
        };
        if payload || width == 0 || matches!(words[pc] as u8, 0x27 | 0x71 | 0x77) {
            return false;
        }
        pc += width;
    }
    pc == words.len()
}

pub(crate) enum SourceStaticDeclaration<'a> {
    Declared(Vec<Arc<str>>),
    ParameterThrow(&'a crate::native_dex::DexMethod),
    Absent,
    Unknown,
}

pub(crate) fn static_parameter_throw_shape(method: &crate::native_dex::DexMethod) -> Option<&str> {
    if method.access_flags & 8 == 0
        || !method.thrown_types.is_empty()
        || method.parameters.len() != 1
    {
        return None;
    }
    let code = method.code.as_ref()?;
    let parameter_register = code.registers.checked_sub(code.ins)?;
    (code.ins == 1
        && parameter_register <= u16::from(u8::MAX)
        && code.tries == 0
        && code.try_regions.is_empty()
        && code.instructions.as_slice() == [0x27 | (parameter_register << 8)])
    .then(|| method.parameters[0].as_ref())
}

#[path = "native_header_throws.rs"]
mod header_throws;

fn complete_no_checked_header(
    class: &DexClass,
    method: &crate::native_dex::DexMethod,
    hierarchy: &TypeHierarchy,
    throw_budget: &mut usize,
) -> bool {
    let Some(code) = &method.code else {
        return false;
    };
    let words = &code.instructions;
    if words.is_empty() || words.len() > 4096 {
        return false;
    }
    // Check original instruction boundaries before resolving static-call symbols.
    let mut has_throw = false;
    let mut cursor = 0;
    while cursor < words.len() {
        let Ok((width, payload)) = crate::native_cfg::instruction_width(words, cursor) else {
            return false;
        };
        if payload || width == 0 {
            return false;
        }
        has_throw |= words[cursor] as u8 == 0x27;
        cursor += width;
    }
    if cursor != words.len() {
        return false;
    }
    let mut pc = 0;
    while pc < words.len() {
        let Ok((width, payload)) = crate::native_cfg::instruction_width(words, pc) else {
            return false;
        };
        if payload || width == 0 {
            return false;
        }
        if matches!(words[pc] as u8, 0x71 | 0x77) {
            let Some(&(owner_index, proto_index, name_index)) =
                class.symbols.methods.get(words[pc + 1] as usize)
            else {
                return false;
            };
            let (Some(owner), Some((ret, args)), Some(name)) = (
                class.symbols.types.get(owner_index as usize),
                class.symbols.protos.get(proto_index as usize),
                class.symbols.strings.get(name_index as usize),
            ) else {
                return false;
            };
            if !hierarchy.header_static_shape_known(class, owner, name, args, ret) {
                return false;
            }
            match hierarchy.source_static_declaration(class, owner, name, args, ret) {
                SourceStaticDeclaration::Absent => {}
                SourceStaticDeclaration::Declared(types)
                    if types.iter().all(|ty| {
                        hierarchy.assignable(ty, "Ljava/lang/RuntimeException;") == Relation::Proven
                            || hierarchy.assignable(ty, "Ljava/lang/Error;") == Relation::Proven
                    }) => {}
                _ => {
                    return false;
                }
            }
        }
        pc += width;
    }
    pc == words.len() && (!has_throw || header_throws::closed(class, method, throw_budget))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PlatformInvokeKind {
    Virtual,
    Super,
    Direct,
    Static,
    Interface,
}

impl PlatformInvokeKind {
    fn from_opcode(opcode: u8) -> Option<Self> {
        match opcode {
            0x6e | 0x74 => Some(Self::Virtual),
            0x6f | 0x75 => Some(Self::Super),
            0x70 | 0x76 => Some(Self::Direct),
            0x71 | 0x77 => Some(Self::Static),
            0x72 | 0x78 => Some(Self::Interface),
            _ => None,
        }
    }
}

#[derive(Debug)]
struct SneakyHeaderSummary {
    dependencies: Vec<String>,
}

struct SneakyMethodAnalysis {
    ir: crate::native_ir::DecodedMethod,
    ssa: crate::native_ssa::SsaMethod,
    bound: crate::native_calls::BoundCalls,
    calls: crate::native_call_values::SsaCalls,
}

impl SneakyMethodAnalysis {
    fn build(
        class: &DexClass,
        method: &crate::native_dex::DexMethod,
        local_work_limit: usize,
    ) -> Result<Self> {
        let code = method
            .code
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("method has no code"))?;
        let ir = crate::native_ir::DecodedMethod::decode(code)?;
        let bound = crate::native_calls::BoundCalls::bind(code, &ir, &class.symbols)?;
        let cfg = crate::native_cfg::ControlFlowGraph::build(code)?;
        let ssa = crate::native_ssa::SsaMethod::build_with_local_and_dominator_limits(
            code,
            &ir,
            &cfg,
            local_work_limit,
            local_work_limit,
        )?;
        let calls = crate::native_call_values::SsaCalls::bind(&bound, &ssa)?;
        Ok(Self {
            ir,
            ssa,
            bound,
            calls,
        })
    }
}

fn reserve_sneaky_analysis_work(
    reserved: &mut usize,
    local_work: usize,
    aggregate_work: usize,
) -> bool {
    // SSA, its dominator tree, and final constructor-to-use dominance.
    let Some(charge) = local_work.checked_mul(3) else {
        return false;
    };
    let Some(next) = reserved.checked_add(charge) else {
        return false;
    };
    if next > aggregate_work {
        return false;
    }
    *reserved = next;
    true
}

fn close_sneaky_header_summaries(
    summaries: &BTreeMap<String, SneakyHeaderSummary>,
) -> HashSet<String> {
    let mut reverse = BTreeMap::<String, Vec<String>>::new();
    let mut remaining = BTreeMap::<String, usize>::new();
    for (key, summary) in summaries {
        if summary
            .dependencies
            .iter()
            .all(|dependency| summaries.contains_key(dependency))
        {
            remaining.insert(key.clone(), summary.dependencies.len());
            for dependency in &summary.dependencies {
                reverse
                    .entry(dependency.clone())
                    .or_default()
                    .push(key.clone());
            }
        }
    }
    let mut ready: BTreeSet<String> = remaining
        .iter()
        .filter(|(_, count)| **count == 0)
        .map(|(key, _)| key.clone())
        .collect();
    let mut proven = HashSet::new();
    while let Some(key) = ready.pop_first() {
        if !proven.insert(key.clone()) {
            continue;
        }
        for dependent in reverse.get(&key).into_iter().flatten() {
            let Some(count) = remaining.get_mut(dependent) else {
                continue;
            };
            *count -= 1;
            if *count == 0 {
                ready.insert(dependent.clone());
            }
        }
    }
    proven
}

fn prepare_sneaky_no_checked_header(
    class: &DexClass,
    method: &crate::native_dex::DexMethod,
    hierarchy: &TypeHierarchy,
    local_work_limit: usize,
) -> Result<Option<SneakyHeaderSummary>> {
    let Some(code) = &method.code else {
        return Ok(None);
    };
    let words = &code.instructions;
    if words.is_empty()
        || words.len() > 4096
        || code.registers > 1024
        || code.ins > code.registers
        || code.tries != 0
        || !code.try_regions.is_empty()
    {
        return Ok(None);
    }
    let mut cursor = 0;
    let mut has_explicit_throw = false;
    while cursor < words.len() {
        let Ok((width, payload)) = crate::native_cfg::instruction_width(words, cursor) else {
            return Ok(None);
        };
        if payload || width == 0 {
            return Ok(None);
        }
        has_explicit_throw |= words[cursor] as u8 == 0x27;
        cursor += width;
    }
    if cursor != words.len() {
        return Ok(None);
    }
    let analysis = if has_explicit_throw {
        Some(SneakyMethodAnalysis::build(
            class,
            method,
            local_work_limit,
        )?)
    } else {
        None
    };
    let mut explicit_throws = 0usize;
    let mut thrown_allocations = Vec::new();
    let mut dependencies = HashSet::new();
    let mut pc = 0;
    while pc < words.len() {
        let Ok((width, payload)) = crate::native_cfg::instruction_width(words, pc) else {
            return Ok(None);
        };
        if payload || width == 0 {
            return Ok(None);
        }
        let opcode = words[pc] as u8;
        if opcode == 0x27 {
            let Some(analysis) = analysis.as_ref() else {
                return Ok(None);
            };
            explicit_throws += 1;
            if explicit_throws > 64 {
                return Ok(None);
            }
            let Some(ssa_instruction) = analysis
                .ssa
                .instructions
                .iter()
                .find(|instruction| instruction.pc == pc)
            else {
                return Ok(None);
            };
            let [read] = ssa_instruction.reads.as_slice() else {
                return Ok(None);
            };
            let [value] = read.words.as_slice() else {
                return Ok(None);
            };
            let Some(crate::native_ssa::DefinitionKind::Instruction {
                pc: allocation_pc,
                word: 0,
                ..
            }) = analysis
                .ssa
                .definitions
                .get(*value)
                .map(|definition| &definition.kind)
            else {
                return Ok(None);
            };
            let Some(allocation) = analysis
                .ir
                .instructions
                .iter()
                .find(|instruction| instruction.pc == *allocation_pc)
            else {
                return Ok(None);
            };
            let Some(crate::native_ir::PoolReference {
                kind: crate::native_ir::PoolKind::Type,
                index,
            }) = allocation.reference
            else {
                return Ok(None);
            };
            let Some(allocated) = class.symbols.types.get(index as usize) else {
                return Ok(None);
            };
            if allocation.opcode != 0x22
                || (hierarchy.assignable(allocated, "Ljava/lang/RuntimeException;")
                    != Relation::Proven
                    && hierarchy.assignable(allocated, "Ljava/lang/Error;") != Relation::Proven)
            {
                return Ok(None);
            }
            thrown_allocations.push((*allocation_pc, allocated.clone(), pc, *value));
        }
        if let Some(kind) = PlatformInvokeKind::from_opcode(opcode) {
            let Some(&(owner_index, proto_index, name_index)) =
                class.symbols.methods.get(words[pc + 1] as usize)
            else {
                return Ok(None);
            };
            let (Some(owner), Some((ret, args)), Some(name)) = (
                class.symbols.types.get(owner_index as usize),
                class.symbols.protos.get(proto_index as usize),
                class.symbols.strings.get(name_index as usize),
            ) else {
                return Ok(None);
            };
            if hierarchy.loaded_owners.contains(owner.as_ref()) {
                if kind != PlatformInvokeKind::Static {
                    return Ok(None);
                }
                let Some(requires_proof) =
                    hierarchy.loaded_static_source_dependency(class, owner, name, args, ret)
                else {
                    return Ok(None);
                };
                if let Some(key) = requires_proof {
                    if key.len() > MAX_CACHE_KEY_BYTES {
                        return Ok(None);
                    }
                    dependencies.insert(key);
                }
            } else {
                let Some(declaration) = hierarchy.exact_platform_source_declaration_for_invoke(
                    &class.descriptor,
                    owner,
                    name,
                    args,
                    ret,
                    kind,
                ) else {
                    return Ok(None);
                };
                if declaration.thrown_types.iter().any(|ty| {
                    hierarchy.assignable(ty, "Ljava/lang/RuntimeException;") != Relation::Proven
                        && hierarchy.assignable(ty, "Ljava/lang/Error;") != Relation::Proven
                }) {
                    return Ok(None);
                }
            }
        }
        if matches!(opcode, 0xfa..=0xfd) {
            return Ok(None);
        }
        pc += width;
    }
    if pc != words.len() {
        return Ok(None);
    }
    if !thrown_allocations.is_empty() {
        let Some(analysis) = analysis.as_ref() else {
            return Ok(None);
        };
        if analysis.calls.unreachable_calls != 0 {
            return Ok(None);
        }
        let dominators = crate::native_dominators::DominatorTree::compute_with_work_limit(
            &analysis.ssa.graph,
            local_work_limit,
        )?;
        for (allocation_pc, allocated, throw_pc, value) in thrown_allocations {
            let mut initializers = analysis.bound.calls.iter().filter_map(|bound| {
                let crate::native_calls::CallTarget::Method { name, .. } = &bound.target else {
                    return None;
                };
                if name.as_ref() != "<init>" {
                    return None;
                }
                let mut value_matches = analysis
                    .calls
                    .calls
                    .iter()
                    .filter(|values| values.pc == bound.pc);
                let values = value_matches.next()?;
                if value_matches.next().is_some()
                    || values
                        .receiver
                        .as_ref()
                        .is_none_or(|receiver| receiver.words.as_slice() != [value])
                {
                    return None;
                }
                Some((bound, values))
            });
            let Some((bound_constructor, constructor_values)) = initializers.next() else {
                return Ok(None);
            };
            if initializers.next().is_some() {
                return Ok(None);
            }
            if bound_constructor.kind != crate::native_calls::CallKind::Direct
                || bound_constructor.return_type.as_ref() != "V"
                || !matches!(
                    &bound_constructor.target,
                    crate::native_calls::CallTarget::Method {
                        declaring_type,
                        name,
                        ..
                    } if declaring_type == &allocated && name.as_ref() == "<init>"
                )
            {
                return Ok(None);
            }
            if constructor_values
                .arguments
                .iter()
                .flat_map(|argument| argument.words.iter())
                .any(|word| *word == value)
            {
                return Ok(None);
            }
            let graph = &analysis.ssa.graph;
            let (Some(&allocation_block), Some(&constructor_block), Some(&throw_block)) = (
                graph.block_at.get(&allocation_pc),
                graph.block_at.get(&bound_constructor.pc),
                graph.block_at.get(&throw_pc),
            ) else {
                return Ok(None);
            };
            if !dominators.dominates(allocation_block, constructor_block)
                || (allocation_block == constructor_block && allocation_pc >= bound_constructor.pc)
                || !dominators.dominates(constructor_block, throw_block)
                || (constructor_block == throw_block && bound_constructor.pc >= throw_pc)
            {
                return Ok(None);
            }
            for instruction in analysis.ssa.instructions.iter().filter(|instruction| {
                instruction.pc != bound_constructor.pc
                    && instruction.pc != throw_pc
                    && instruction
                        .reads
                        .iter()
                        .flat_map(|read| read.words.iter())
                        .any(|word| *word == value)
            }) {
                let Some(&read_block) = graph.block_at.get(&instruction.pc) else {
                    return Ok(None);
                };
                if !dominators.dominates(constructor_block, read_block)
                    || (constructor_block == read_block && bound_constructor.pc >= instruction.pc)
                {
                    return Ok(None);
                }
                let mut bound_matches = analysis
                    .bound
                    .calls
                    .iter()
                    .filter(|call| call.pc == instruction.pc);
                let mut value_matches = analysis
                    .calls
                    .calls
                    .iter()
                    .filter(|call| call.pc == instruction.pc);
                let (Some(bound), Some(values)) = (bound_matches.next(), value_matches.next())
                else {
                    return Ok(None);
                };
                if bound_matches.next().is_some()
                    || value_matches.next().is_some()
                    || bound.kind != crate::native_calls::CallKind::Static
                    || values.receiver.is_some()
                    || !values
                        .arguments
                        .iter()
                        .flat_map(|argument| argument.words.iter())
                        .any(|word| *word == value)
                {
                    return Ok(None);
                }
            }
        }
    }
    let mut dependencies: Vec<_> = dependencies.into_iter().collect();
    dependencies.sort();
    Ok(Some(SneakyHeaderSummary { dependencies }))
}

fn checked_inference_eligible(class: &DexClass, method: &crate::native_dex::DexMethod) -> bool {
    method.thrown_types.is_empty()
        && method.name.as_ref() != "<clinit>"
        && (method.access_flags & (2 | 8) != 0
            || class.access_flags & 0x10 != 0
            || method.name.as_ref() == "<init>")
}

fn method_may_infer_checked(class: &DexClass, method: &crate::native_dex::DexMethod) -> bool {
    checked_inference_eligible(class, method)
        && method.code.as_ref().is_some_and(|code| {
            code.instructions
                .iter()
                .any(|word| matches!((*word & 0xff) as u8, 0x27 | 0x71 | 0x77))
                && !complete_no_inference_trigger(&code.instructions)
        })
}

fn summarize_loaded_call_owner(
    class: &DexClass,
    future_words_left: &mut usize,
) -> Option<LoadedCallOwner> {
    let directory = class.symbols.annotations.get(&class.annotations_offset);
    if class.annotations_offset != 0 && directory.is_none() {
        return None;
    }
    let mut summary = LoadedCallOwner {
        access_flags: class.access_flags,
        // Interface-super uses the current descriptor-only class header.
        // Signature metadata is omitted by that renderer; generic loaded-call
        // certificates below retain their separate conservative restrictions.
        erased_interface_source_header: class.access_flags & 0x200 != 0
            && crate::native_java::source_header_authority(class),
        public: class.access_flags & 1 != 0,
        root_object: class.superclass.as_deref() == Some(OBJECT)
            && class.interfaces.is_empty()
            && class.access_flags & 0x200 == 0,
        generic_class: directory
            .and_then(|directory| directory.class.as_ref())
            .is_some_and(|annotations| {
                annotations.iter().any(|annotation| {
                    class
                        .symbols
                        .types
                        .get(annotation.type_idx as usize)
                        .is_none_or(|ty| ty.as_ref() == "Ldalvik/annotation/Signature;")
                })
            }),
        varargs_names: HashSet::new(),
        methods: HashMap::new(),
        exact_overloads: None,
    };
    for (index, method) in class.methods.iter().enumerate() {
        if method.declaring_type != class.descriptor {
            return None;
        }
        if method.access_flags & 0x80 != 0 {
            summary.varargs_names.insert(method.name.clone());
        }
        let annotated = directory.is_some_and(|directory| {
            // A class-only annotation directory stores an empty methods Vec.
            // A nonempty Vec is indexed in declaration order.
            !directory.methods.is_empty()
                && directory.methods.get(index).is_none_or(|annotations| {
                    annotations.as_ref().is_some_and(|set| !set.is_empty())
                })
        });
        let generic_method = directory
            .and_then(|directory| directory.methods.get(index))
            .and_then(Option::as_ref)
            .is_some_and(|set| {
                set.iter().any(|annotation| {
                    class
                        .symbols
                        .types
                        .get(annotation.type_idx as usize)
                        .is_none_or(|ty| ty.as_ref() == "Ldalvik/annotation/Signature;")
                })
            });
        let future_checked_effects_closed = if future_headers::signature(method) {
            let charge = method
                .code
                .as_ref()
                .map_or(0, |code| code.instructions.len().saturating_mul(3));
            if charge <= *future_words_left {
                *future_words_left -= charge;
                future_headers::checked_effects_closed(method)
            } else {
                false
            }
        } else {
            false
        };
        let may_infer_checked = method_may_infer_checked(class, method);
        let method = LoadedCallMethod {
            static_no_return: static_no_return::prove(method),
            declared_throws_empty: class.methods[index].thrown_types.is_empty(),
            generic_method,
            legal_source_declaration: method.access_flags & !0x31dff == 0
                && matches!(method.access_flags & 7, 0 | 1 | 2 | 4)
                && (method.access_flags & 0x80 == 0
                    || method
                        .parameters
                        .last()
                        .is_some_and(|ty| ty.starts_with('[')))
                && (method.access_flags & (0x400 | 0x100) != 0) == method.code.is_none()
                && (method.access_flags & 0x400 == 0
                    || method.access_flags & (2 | 8 | 0x10 | 0x20 | 0x100 | 0x800 | 0x20000) == 0),
            no_new_checked_header: false,
            future_checked_effects_closed,
            may_infer_checked,
            parameters: method.parameters.clone(),
            return_type: method.return_type.clone(),
            access_flags: method.access_flags,
            annotated,
        };
        let family = summary
            .methods
            .entry(class.methods[index].name.clone())
            .or_default();
        match family.entry(method.parameters.len()) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(Some(method));
            }
            std::collections::hash_map::Entry::Occupied(mut entry) => {
                let exact = summary
                    .exact_overloads
                    .get_or_insert_with(Default::default)
                    .entry(class.methods[index].name.clone())
                    .or_default();
                if let Some(first) = entry.insert(None) {
                    exact.insert(first.parameters.clone(), Some(first));
                }
                match exact.entry(method.parameters.clone()) {
                    std::collections::hash_map::Entry::Vacant(entry) => {
                        entry.insert(Some(method));
                    }
                    std::collections::hash_map::Entry::Occupied(mut entry) => {
                        entry.insert(None);
                    }
                }
            }
        }
    }
    Some(summary)
}

#[derive(Debug, Default)]
struct RelationCache {
    values: HashMap<Arc<str>, HashMap<Arc<str>, Relation>>,
    order: VecDeque<(Arc<str>, Arc<str>)>,
    len: usize,
}

impl RelationCache {
    fn get(&self, source: &str, target: &str) -> Option<Relation> {
        self.values
            .get(source)
            .and_then(|targets| targets.get(target))
            .copied()
    }

    fn insert(&mut self, source: &str, target: &str, relation: Relation) {
        if source.len() > MAX_CACHE_KEY_BYTES || target.len() > MAX_CACHE_KEY_BYTES {
            return;
        }
        if self
            .values
            .get(source)
            .is_some_and(|targets| targets.contains_key(target))
        {
            return;
        }
        if self.len == MAX_CACHE_ENTRIES
            && let Some((old_source, old_target)) = self.order.pop_front()
        {
            if let Some(targets) = self.values.get_mut(old_source.as_ref()) {
                targets.remove(old_target.as_ref());
                if targets.is_empty() {
                    self.values.remove(old_source.as_ref());
                }
            }
            self.len -= 1;
        }
        let key = (Arc::from(source), Arc::from(target));
        self.order.push_back(key.clone());
        self.values
            .entry(key.0)
            .or_default()
            .insert(key.1, relation);
        self.len += 1;
    }
}

impl TypeHierarchy {
    pub(crate) fn loaded_source_owner(&self, owner: &str) -> bool {
        self.loaded_owners.contains(owner) && !self.ambiguous.contains(owner)
    }
    /// Exact source declaration used only by the bounded precise-rethrow retry.
    /// Loaded methods require a closed source header, never an inferred runtime
    /// effect approximation. Missing/ambiguous declarations retain the refusal.
    pub(crate) fn exact_rethrow_source_declaration(
        &self,
        caller: &str,
        owner: &str,
        name: &str,
        args: &[Arc<str>],
        ret: &str,
        opcode: u8,
    ) -> Option<ExactSourceDeclaration> {
        let kind = PlatformInvokeKind::from_opcode(opcode)?;
        if !self.loaded_owners.contains(owner) {
            return self.exact_platform_source_declaration_for_invoke(
                caller, owner, name, args, ret, kind,
            );
        }
        if self.ambiguous.contains(owner) || self.override_ambiguous_owners.contains(owner) {
            return None;
        }
        let summary = self.loaded_call_families.get(owner)?;
        let candidate = summary.methods.get(name)?.get(&args.len())?.as_ref()?;
        let erased_abstract_interface =
            summary.erased_interface_source_header && candidate.access_flags & 0x400 != 0;
        if ((summary.generic_class || candidate.generic_method) && !erased_abstract_interface)
            || !candidate.legal_source_declaration
            || candidate.parameters != args
            || candidate.return_type.as_ref() != ret
            || !platform_source_accessible(
                caller,
                owner,
                summary.access_flags,
                candidate.access_flags,
            )
        {
            return None;
        }
        let flags = candidate.access_flags;
        let interface = summary.access_flags & 0x200 != 0;
        let permitted = match kind {
            PlatformInvokeKind::Static => !name.starts_with('<') && flags & 8 != 0,
            PlatformInvokeKind::Virtual => {
                !name.starts_with('<') && !interface && flags & (8 | 2) == 0
            }
            PlatformInvokeKind::Interface => {
                !name.starts_with('<') && interface && flags & (8 | 2) == 0
            }
            PlatformInvokeKind::Direct => {
                name == "<init>" && !interface && ret == "V" && flags & (8 | 0x400 | 0x100) == 0
            }
            PlatformInvokeKind::Super => false,
        };
        if !permitted {
            return None;
        }
        let mut thrown_types = if candidate.declared_throws_empty {
            if candidate.may_infer_checked && !candidate.no_new_checked_header {
                return None;
            }
            Vec::new()
        } else {
            let key = format!(
                "{owner}->{name}({}){ret}",
                args.iter().map(AsRef::as_ref).collect::<String>()
            );
            let (is_static, types) = self.static_throws.get(&key)?;
            if *is_static != (flags & 8 != 0) {
                return None;
            }
            types.to_vec()
        };
        // The ordinary writer also restores a bounded set of inherited source
        // contracts (IO/Future/Android callbacks). Reconstruct that declaration
        // view from the immutable owner index, rather than mistaking empty DEX
        // Throws metadata for an empty emitted Java header.
        let entry = self.entries.get(owner)?;
        let declaration_method = crate::native_dex::DexMethod {
            declaring_type: Arc::from(owner),
            name: Arc::from(name),
            return_type: Arc::from(ret),
            parameters: args.to_vec(),
            thrown_types: thrown_types.clone(),
            access_flags: flags,
            code: None,
        };
        let declaration_class = DexClass {
            descriptor: Arc::from(owner),
            superclass: entry.superclass.clone(),
            interfaces: entry.interfaces.to_vec(),
            access_flags: summary.access_flags,
            annotations_offset: 0,
            static_values_offset: 0,
            static_values: vec![],
            fields: vec![],
            methods: vec![],
            symbols: Arc::new(crate::native_dex::DexSymbols::default()),
        };
        thrown_types.extend(
            source_inherited_exceptions(&declaration_class, &declaration_method, Some(self))
                .into_iter()
                .map(Arc::from),
        );
        Some(ExactSourceDeclaration {
            owner_access_flags: summary.access_flags,
            member_access_flags: flags,
            thrown_types,
        })
    }

    pub(crate) fn enum_array_retainer(&self, owner: &str) -> bool {
        !self.ambiguous.contains(owner)
            && !self.loaded_owners.contains(OBJECT)
            && self.enum_array_retainers.contains(owner)
    }
    /// A concrete superclass declaration usable from a private instance bridge.
    /// The bridge retains Java `super` dispatch and catches/rethrows the same
    /// Throwable, so this grants no inferred checked-exception contract.
    pub(crate) fn callable_super_declaration(
        &self,
        caller: &str,
        owner: &str,
        name: &str,
        args: &[Arc<str>],
        ret: &str,
    ) -> bool {
        if self.strict_superclass(caller, owner) != Relation::Proven
            || name.starts_with('<')
            || self.ambiguous.contains(owner)
        {
            return false;
        }
        let flags = if self.loaded_owners.contains(owner) {
            let Some(summary) = self.loaded_call_families.get(owner) else {
                return false;
            };
            let Some(method) = summary
                .methods
                .get(name)
                .and_then(|f| f.get(&args.len()))
                .and_then(Option::as_ref)
            else {
                return false;
            };
            if summary.access_flags & 0x200 != 0
                || method.parameters != args
                || method.return_type.as_ref() != ret
                || !method.legal_source_declaration
            {
                return false;
            }
            method.access_flags
        } else {
            let facts = platform_source_declarations();
            if !facts.complete
                || facts.schema != 1
                || facts.android_jar_sha256 != platform_exceptions().sha256
                || facts.class_count != facts.classes.len()
                || facts.method_count != facts.methods.len()
                || facts.ambiguous_source_signature_count != facts.ambiguous_source_signatures.len()
                || facts
                    .classes
                    .get(owner)
                    .is_none_or(|flags| flags & 0x200 != 0)
            {
                return false;
            }
            let source_key = format!(
                "{owner}->{name}({})",
                args.iter().map(AsRef::as_ref).collect::<String>()
            );
            if facts.ambiguous_source_signatures.contains(&source_key) {
                return false;
            }
            let Some(method) = facts.methods.get(&format!("{source_key}{ret}")) else {
                return false;
            };
            method.access_flags
        };
        let visibility = flags & 7;
        flags & (8 | 2 | 0x400) == 0
            && (matches!(visibility, 1 | 4)
                || visibility == 0 && descriptor_package(caller) == descriptor_package(owner))
    }

    /// Proof-only exact Java declaration lookup. It never feeds the generic
    /// checked-effect machinery and never follows inheritance.
    pub(crate) fn exact_platform_source_declaration(
        &self,
        caller: &str,
        owner: &str,
        name: &str,
        args: &[Arc<str>],
        ret: &str,
        static_call: bool,
    ) -> Option<ExactSourceDeclaration> {
        if self.loaded_owners.contains(owner)
            || self.ambiguous.contains(owner)
            || self.override_ambiguous_owners.contains(owner)
            || owner.contains('$')
        {
            return None;
        }
        let facts = platform_source_declarations();
        if !facts.complete
            || facts.schema != 1
            || facts.android_jar_sha256 != platform_exceptions().sha256
            || facts.class_count != facts.classes.len()
            || facts.method_count != facts.methods.len()
            || facts.ambiguous_source_signature_count != facts.ambiguous_source_signatures.len()
            || facts.class_count > 100_000
            || facts.method_count > 2_000_000
            || facts.ambiguous_source_signature_count > facts.method_count
        {
            return None;
        }
        let owner_flags = *facts.classes.get(owner)?;
        if owner_flags & !0x7631 != 0 || !valid_visibility(owner_flags) {
            return None;
        }
        let key = format!(
            "{owner}->{name}({}){ret}",
            args.iter().map(AsRef::as_ref).collect::<String>()
        );
        let source_key = format!(
            "{owner}->{name}({})",
            args.iter().map(AsRef::as_ref).collect::<String>()
        );
        if facts.ambiguous_source_signatures.contains(&source_key) {
            return None;
        }
        let member = facts.methods.get(&key)?;
        if member.access_flags & !0x1dff != 0
            || !valid_visibility(member.access_flags)
            || (member.access_flags & 8 != 0) != static_call
        {
            return None;
        }
        if !platform_source_accessible(caller, owner, owner_flags, member.access_flags) {
            return None;
        }
        if member
            .exceptions
            .iter()
            .any(|ty| !valid_reference_descriptor(ty))
        {
            return None;
        }
        Some(ExactSourceDeclaration {
            owner_access_flags: owner_flags,
            member_access_flags: member.access_flags,
            thrown_types: member
                .exceptions
                .iter()
                .map(|ty| Arc::<str>::from(ty.as_str()))
                .collect(),
        })
    }

    fn exact_platform_source_declaration_for_invoke(
        &self,
        caller: &str,
        owner: &str,
        name: &str,
        args: &[Arc<str>],
        ret: &str,
        invoke_kind: PlatformInvokeKind,
    ) -> Option<ExactSourceDeclaration> {
        if self.loaded_owners.contains(owner)
            || self.ambiguous.contains(owner)
            || self.override_ambiguous_owners.contains(owner)
            || owner.contains('$')
        {
            return None;
        }
        let facts = platform_source_declarations();
        if !facts.complete
            || facts.schema != 1
            || facts.android_jar_sha256 != platform_exceptions().sha256
            || facts.class_count != facts.classes.len()
            || facts.method_count != facts.methods.len()
            || facts.ambiguous_source_signature_count != facts.ambiguous_source_signatures.len()
            || facts.class_count > 100_000
            || facts.method_count > 2_000_000
            || facts.ambiguous_source_signature_count > facts.method_count
        {
            return None;
        }
        let owner_flags = *facts.classes.get(owner)?;
        if owner_flags & !0x7631 != 0 || !valid_visibility(owner_flags) {
            return None;
        }
        let key = format!(
            "{owner}->{name}({}){ret}",
            args.iter().map(AsRef::as_ref).collect::<String>()
        );
        let source_key = format!(
            "{owner}->{name}({})",
            args.iter().map(AsRef::as_ref).collect::<String>()
        );
        if facts.ambiguous_source_signatures.contains(&source_key) {
            return None;
        }
        let member = facts.methods.get(&key)?;
        if member.access_flags & !0x1dff != 0 || !valid_visibility(member.access_flags) {
            return None;
        }
        let is_interface = owner_flags & 0x200 != 0;
        let is_static = member.access_flags & 8 != 0;
        let is_private = member.access_flags & 2 != 0;
        let invocation_matches = match invoke_kind {
            PlatformInvokeKind::Static => !name.starts_with('<') && is_static,
            PlatformInvokeKind::Direct => {
                name == "<init>"
                    && ret == "V"
                    && !is_interface
                    && !is_static
                    && member.access_flags & (0x400 | 0x100) == 0
            }
            PlatformInvokeKind::Virtual => {
                !name.starts_with('<') && !is_interface && !is_static && !is_private
            }
            PlatformInvokeKind::Interface => {
                !name.starts_with('<') && is_interface && !is_static && !is_private
            }
            // The first certificate does not prove exact invokespecial-style
            // owner selection for superclass/default-interface dispatch.
            PlatformInvokeKind::Super => false,
        };
        if !invocation_matches {
            return None;
        }
        if !platform_source_accessible(caller, owner, owner_flags, member.access_flags) {
            return None;
        }
        if member
            .exceptions
            .iter()
            .any(|ty| !valid_reference_descriptor(ty))
        {
            return None;
        }
        Some(ExactSourceDeclaration {
            owner_access_flags: owner_flags,
            member_access_flags: member.access_flags,
            thrown_types: member
                .exceptions
                .iter()
                .map(|ty| Arc::<str>::from(ty.as_str()))
                .collect(),
        })
    }

    /// A checked declaration is legal only when every inherited declaration
    /// with the same Java signature permits it. Missing external definitions
    /// cannot establish that contract. Private methods and constructors do not
    /// participate in override families.
    pub fn permits_inferred_checked_throw(
        &self,
        class: &DexClass,
        method: &crate::native_dex::DexMethod,
        exception: &str,
    ) -> bool {
        if method.access_flags & 2 != 0 {
            return true;
        }
        if self.ambiguous.contains(class.descriptor.as_ref())
            || self
                .override_ambiguous_owners
                .contains(class.descriptor.as_ref())
            || self.assignable(exception, "Ljava/lang/Throwable;") != Relation::Proven
        {
            return false;
        }
        let signature = format!(
            "{}({})",
            method.name,
            method
                .parameters
                .iter()
                .map(AsRef::as_ref)
                .collect::<String>()
        );
        if method.name.as_ref() != "<init>"
            && method.access_flags & 8 == 0
            && class.access_flags & 0x10 == 0
        {
            return false;
        }
        let caller_safe = || {
            self.checked_callers
                .permits(&io_method_key(class, method), exception, self)
        };
        if method.name.as_ref() == "<init>" {
            return caller_safe();
        }
        if !self
            .indexed_candidate_contracts
            .contains(&format!("{}->{signature}", class.descriptor))
        {
            return false;
        }
        let pinned_io_signature = PINNED_IO_CONTRACT_OWNERS.iter().any(|owner| {
            !matches!(
                pinned_io_member(owner, &signature, &method.return_type),
                None | Some(PinnedIoMember::Absent)
            )
        });
        let static_method = method.access_flags & 8 != 0;
        let mut pending: Vec<(Arc<str>, bool)> = class
            .superclass
            .iter()
            .chain(class.interfaces.iter().filter(|_| !static_method))
            .cloned()
            .map(|owner| (owner, false))
            .collect();
        let mut seen = HashSet::from([class.descriptor.clone()]);
        let mut active = HashSet::from([class.descriptor.clone()]);
        let mut pinned_io_seen = false;
        let mut pinned_io_contract = false;
        for _ in 0..(MAX_QUERY_NODES * 2) {
            let Some((owner, exit)) = pending.pop() else {
                return (!pinned_io_seen || pinned_io_contract) && caller_safe();
            };
            if exit {
                active.remove(owner.as_ref());
                continue;
            }
            if active.contains(owner.as_ref()) {
                return false;
            }
            if !seen.insert(owner.clone()) {
                continue;
            }
            active.insert(owner.clone());
            if self.ambiguous.contains(owner.as_ref())
                || self.override_ambiguous_owners.contains(owner.as_ref())
            {
                return false;
            }
            // SDK ancestry tables never establish a source contract through
            // a loaded replacement of one of their owners.
            if self.loaded_owners.contains(owner.as_ref())
                && pinned_io_member(&owner, &signature, &method.return_type).is_some()
            {
                return false;
            }
            let key = format!("{owner}->{signature}");
            if self.ambiguous_method_contracts.contains(&key) {
                return false;
            }
            if let Some(contract) = self.method_contracts.get(&key) {
                // A private ancestor does not constrain a new declaration.
                if contract.access_flags & 2 == 0
                    && ((contract.inferred_throws
                        && (!contract.owner_final
                            || !contract.thrown_types.iter().all(|declared| {
                                self.checked_callers.permits(
                                    &format!("{key}{}", contract.return_type),
                                    declared,
                                    self,
                                )
                            })))
                        || (pinned_io_signature
                            && contract.return_type.as_ref() != method.return_type.as_ref()
                            && self.assignable(&method.return_type, &contract.return_type)
                                != Relation::Proven)
                        || !contract.thrown_types.iter().any(|declared| {
                            self.assignable(exception, declared) == Relation::Proven
                        }))
                {
                    return false;
                }
            }
            if owner.as_ref() == OBJECT {
                // Object methods may be overridden even when not loaded.
                if object_method_signature(&signature) {
                    return false;
                }
                active.remove(owner.as_ref());
                continue;
            }
            if !self.loaded_owners.contains(owner.as_ref())
                && let Some(contract) = pinned_io_member(&owner, &signature, &method.return_type)
            {
                match contract {
                    // The table describes this owner's declarations only. A local
                    // absence establishes no inherited exception contract.
                    PinnedIoMember::Absent => {}
                    PinnedIoMember::Throws(declared)
                        if !static_method
                            && method.access_flags & 7 == 1
                            && self.assignable(exception, declared) == Relation::Proven =>
                    {
                        pinned_io_seen = true;
                        pinned_io_contract = true;
                    }
                    _ => return false,
                }
                // Continue through the exact SDK ancestry, including loaded
                // shadows of an ancestor. The ordinary node/cycle budget and
                // declaration ambiguity checks apply to every inherited owner.
                let Some((parent, interfaces)) = platform_exceptions().types.get(owner.as_ref())
                else {
                    return false;
                };
                pending.push((owner, true));
                pending.extend(
                    parent
                        .iter()
                        .chain(interfaces.iter().filter(|_| !static_method))
                        .map(|ancestor| (Arc::from(ancestor.as_str()), false)),
                );
                continue;
            }
            if !self.loaded_owners.contains(owner.as_ref())
                && known_platform_interface_excludes_signature(&owner, &signature)
            {
                active.remove(owner.as_ref());
                continue;
            }
            let Some(entry) = self.entries.get(owner.as_ref()) else {
                return false;
            };
            if !self.loaded_owners.contains(owner.as_ref()) {
                return false;
            }
            pending.push((owner, true));
            pending.extend(
                entry
                    .superclass
                    .iter()
                    .chain(entry.interfaces.iter().filter(|_| !static_method))
                    .cloned()
                    .map(|owner| (owner, false)),
            );
        }
        false
    }

    /// Reconstitute an omitted Java declaration only for a complete, exact
    /// Android-35 I/O ancestor contract. The ordinary override query still
    /// rejects loaded shadows, unknown extra parents, and return mismatches.
    pub fn inherited_io_exception(
        &self,
        class: &DexClass,
        method: &crate::native_dex::DexMethod,
    ) -> bool {
        if class.access_flags & 0x10 == 0
            || method.access_flags & (2 | 8) != 0
            || method.name.as_ref() == "<init>"
        {
            return false;
        }
        let signature = format!(
            "{}({})",
            method.name,
            method
                .parameters
                .iter()
                .map(AsRef::as_ref)
                .collect::<String>()
        );
        PINNED_IO_CONTRACT_OWNERS.iter().any(|owner| {
            matches!(
                pinned_io_member(owner, &signature, &method.return_type),
                Some(PinnedIoMember::Throws("Ljava/io/IOException;"))
            ) && self.assignable(&class.descriptor, owner) == Relation::Proven
        }) && self.permits_inferred_checked_throw(class, method, "Ljava/io/IOException;")
    }

    fn header_static_shape_known(
        &self,
        class: &DexClass,
        owner: &str,
        name: &str,
        args: &[Arc<str>],
        ret: &str,
    ) -> bool {
        if self.ambiguous.contains(owner) || self.override_ambiguous_owners.contains(owner) {
            return false;
        }
        let key = format!(
            "{owner}->{name}({}){ret}",
            args.iter().map(AsRef::as_ref).collect::<String>()
        );
        if self
            .ambiguous_method_contracts
            .contains(&key[..key.len() - ret.len()])
        {
            return false;
        }
        if !self.loaded_owners.contains(owner) {
            return platform_exceptions()
                .methods
                .get(&key)
                .is_none_or(|(is_static, _)| *is_static);
        }
        let Some(summary) = self.loaded_call_families.get(owner) else {
            return false;
        };
        let candidate = summary
            .methods
            .get(name)
            .and_then(|family| family.get(&args.len()))
            .and_then(|candidate| {
                candidate.as_ref().or_else(|| {
                    summary
                        .exact_overloads
                        .as_ref()
                        .and_then(|exact| exact.get(name))
                        .and_then(|family| family.get(args))
                        .and_then(Option::as_ref)
                })
            });
        candidate.is_some_and(|candidate| {
            candidate.parameters == args
                && candidate.return_type.as_ref() == ret
                && candidate.access_flags & 8 != 0
                && (candidate.access_flags & 2 == 0 || owner == class.descriptor.as_ref())
        })
    }

    fn loaded_static_source_dependency(
        &self,
        class: &DexClass,
        owner: &str,
        name: &str,
        args: &[Arc<str>],
        ret: &str,
    ) -> Option<Option<String>> {
        if owner.contains('$') || !self.header_static_shape_known(class, owner, name, args, ret) {
            return None;
        }
        let summary = self.loaded_call_families.get(owner)?;
        let candidate = summary
            .methods
            .get(name)
            .and_then(|family| family.get(&args.len()))
            .and_then(|candidate| {
                candidate.as_ref().or_else(|| {
                    summary
                        .exact_overloads
                        .as_ref()
                        .and_then(|exact| exact.get(name))
                        .and_then(|family| family.get(args))
                        .and_then(Option::as_ref)
                })
            });
        let key = format!(
            "{owner}->{name}({}){ret}",
            args.iter().map(AsRef::as_ref).collect::<String>()
        );
        let candidate = candidate?;
        (candidate.parameters == args
            && candidate.return_type.as_ref() == ret
            && (owner == class.descriptor.as_ref()
                || summary.public && candidate.access_flags & 7 == 1)
            && candidate.declared_throws_empty
            && !candidate.generic_method
            && candidate.legal_source_declaration)
            .then_some(())?;
        Some((candidate.may_infer_checked && !candidate.no_new_checked_header).then_some(key))
    }

    pub(crate) fn exact_loaded_static_source_call(
        &self,
        class: &DexClass,
        owner: &str,
        name: &str,
        args: &[Arc<str>],
        ret: &str,
    ) -> bool {
        if !self.loaded_owners.contains(owner)
            || owner.contains('$')
            || !self.header_static_shape_known(class, owner, name, args, ret)
        {
            return false;
        }
        let Some(summary) = self.loaded_call_families.get(owner) else {
            return false;
        };
        let candidate = summary
            .methods
            .get(name)
            .and_then(|family| family.get(&args.len()))
            .and_then(Option::as_ref);
        summary.public
            && summary.access_flags & !0x7631 == 0
            && candidate.is_some_and(|candidate| {
                candidate.parameters == args
                    && candidate.return_type.as_ref() == ret
                    && candidate.access_flags & 7 == 1
                    && candidate.access_flags & 8 != 0
                    && candidate.declared_throws_empty
                    && (candidate.no_new_checked_header
                        || !candidate.may_infer_checked
                        || self.static_sneaky_source_headers.contains(&format!(
                            "{owner}->{name}({}){ret}",
                            args.iter().map(AsRef::as_ref).collect::<String>()
                        )))
                    && !candidate.generic_method
                    && candidate.legal_source_declaration
            })
    }

    // Shared with the exact post-render declaration writer. This does not
    // summarize the runtime effects or recursively generated callee headers.
    pub(crate) fn source_static_declaration<'a>(
        &self,
        class: &'a DexClass,
        owner: &str,
        name: &str,
        args: &[Arc<str>],
        ret: &str,
    ) -> SourceStaticDeclaration<'a> {
        if let Some(types) = self.exact_static_call_thrown_types(owner, name, args, ret) {
            return SourceStaticDeclaration::Declared(types);
        }
        if owner == class.descriptor.as_ref() {
            let mut methods = class.methods.iter().filter(|candidate| {
                candidate.access_flags & 8 != 0
                    && candidate.name.as_ref() == name
                    && candidate.parameters == args
                    && candidate.return_type.as_ref() == ret
            });
            let Some(method) = methods.next() else {
                return SourceStaticDeclaration::Unknown;
            };
            if methods.next().is_some() {
                return SourceStaticDeclaration::Unknown;
            }
            if static_parameter_throw_shape(method).is_some() {
                return SourceStaticDeclaration::ParameterThrow(method);
            }
        }
        SourceStaticDeclaration::Absent
    }

    /// Exact declaration only. A loaded definition shadows the SDK contract,
    /// including when it declares a narrower exception or no exceptions.
    pub fn exact_static_call_thrown_types(
        &self,
        owner: &str,
        name: &str,
        args: &[Arc<str>],
        ret: &str,
    ) -> Option<Vec<Arc<str>>> {
        if self.ambiguous.contains(owner) || self.override_ambiguous_owners.contains(owner) {
            return None;
        }
        let key = format!(
            "{owner}->{name}({}){ret}",
            args.iter().map(AsRef::as_ref).collect::<String>()
        );
        if let Some((is_static, types)) = self.static_throws.get(&key) {
            return is_static.then(|| types.to_vec());
        }
        if self.loaded_owners.contains(owner) {
            return None;
        }
        platform_exceptions()
            .methods
            .get(&key)
            .and_then(|(is_static, types)| {
                is_static.then(|| {
                    types
                        .iter()
                        .map(|ty| Arc::<str>::from(ty.as_str()))
                        .collect()
                })
            })
    }

    /// Source-visible known checked contracts, or a conservative Throwable effect
    /// when loaded inheritance/overload facts are incomplete. Unknown external
    /// SDK members without a pinned declaration remain unavailable.
    pub fn call_checked_effects(
        &self,
        original_owner: &str,
        name: &str,
        args: &[Arc<str>],
        ret: &str,
        static_call: bool,
    ) -> Option<Vec<Arc<str>>> {
        let unknown = || Some(vec![Arc::<str>::from("Ljava/lang/Throwable;")]);
        let local = |owner: &str| -> Result<Option<Vec<Arc<str>>>, ()> {
            if self.ambiguous.contains(owner) || self.override_ambiguous_owners.contains(owner) {
                return Err(());
            }
            let signature = format!(
                "{owner}->{name}({})",
                args.iter().map(AsRef::as_ref).collect::<String>()
            );
            let key = format!("{signature}{ret}");
            if self.ambiguous_method_contracts.contains(&signature) {
                return Err(());
            }
            if self.loaded_owners.contains(owner) {
                let (candidate, owner_flags) = if let Some(summary) =
                    self.loaded_call_families.get(owner)
                {
                    let candidate = match summary
                        .methods
                        .get(name)
                        .and_then(|family| family.get(&args.len()))
                    {
                        Some(Some(candidate)) => Some(candidate),
                        Some(None) => match summary
                            .exact_overloads
                            .as_ref()
                            .and_then(|exact| exact.get(name))
                            .and_then(|family| family.get(args))
                        {
                            Some(Some(candidate)) => Some(candidate),
                            Some(None) => return Err(()),
                            None => None,
                        },
                        None => None,
                    };
                    (candidate, summary.access_flags)
                } else {
                    match self
                        .checked_call_certificates
                        .get(owner, name, args, ret, static_call)
                    {
                        Some(checked_call_certificates::Certificate::Present {
                            method,
                            owner_flags,
                        }) => (Some(method), *owner_flags),
                        Some(checked_call_certificates::Certificate::Absent) => (None, 0),
                        _ => return Err(()),
                    }
                };
                if let Some(candidate) = candidate {
                    if candidate.return_type.as_ref() != ret
                        && self.assignable(&candidate.return_type, ret) != Relation::Proven
                    {
                        return Err(());
                    }
                    if candidate.parameters == args {
                        if (candidate.access_flags & 8 != 0) != static_call {
                            return Err(());
                        }
                        let actual_key = format!(
                            "{owner}->{name}({}){}",
                            args.iter().map(AsRef::as_ref).collect::<String>(),
                            candidate.return_type
                        );
                        if let Some((_, types)) = self.static_throws.get(&actual_key) {
                            return Ok(Some(types.to_vec()));
                        }
                        if !static_call
                            && name == "get"
                            && (args.is_empty()
                                || args.iter().map(AsRef::as_ref).collect::<String>()
                                    == "JLjava/util/concurrent/TimeUnit;")
                            && self.assignable(owner, "Ljava/util/concurrent/Future;")
                                == Relation::Proven
                            && !candidate.future_checked_effects_closed
                        {
                            return Err(());
                        }
                        // Existing inferred-root contracts are necessary declarations,
                        // not complete summaries of additional explicit/callee effects.
                        let inferred = self.method_contracts.get(&signature).filter(|contract| {
                            contract.inferred_throws
                                && contract.return_type == candidate.return_type
                                && (contract.access_flags & 8 != 0) == static_call
                        });
                        if (inferred.is_some() && !candidate.no_new_checked_header)
                            || (candidate.may_infer_checked
                                && (candidate.access_flags & 2 != 0
                                    || !self.checked_callers.ready
                                    || !self.checked_callers.complete
                                    || (self.checked_callers.candidates.contains(&actual_key)
                                        && !self.checked_callers.blocked.contains(&actual_key))))
                        {
                            return Err(());
                        }
                        // Source emission may append an exact inherited SDK header
                        // independently of body inference. Consume the same authority.
                        if candidate.access_flags & 15 == 1 {
                            let entry = self.entries.get(owner).ok_or(())?;
                            if entry.superclass.as_deref() != Some(OBJECT)
                                || !entry.interfaces.is_empty()
                            {
                                let class = DexClass {
                                    symbols: Arc::new(crate::native_dex::DexSymbols::default()),
                                    descriptor: Arc::from(owner),
                                    superclass: entry.superclass.clone(),
                                    interfaces: entry.interfaces.to_vec(),
                                    access_flags: owner_flags,
                                    annotations_offset: 0,
                                    static_values_offset: 0,
                                    static_values: Vec::new(),
                                    fields: Vec::new(),
                                    methods: Vec::new(),
                                };
                                let method = crate::native_dex::DexMethod {
                                    declaring_type: class.descriptor.clone(),
                                    name: Arc::from(name),
                                    return_type: candidate.return_type.clone(),
                                    parameters: candidate.parameters.clone(),
                                    thrown_types: Vec::new(),
                                    access_flags: candidate.access_flags,
                                    code: None,
                                };
                                let types =
                                    source_inherited_exceptions(&class, &method, Some(self));
                                if !self.raw_future_header(&class, &method).is_empty()
                                    && !candidate.future_checked_effects_closed
                                {
                                    return Err(());
                                }
                                if !types.is_empty() {
                                    return Ok(Some(types.into_iter().map(Arc::from).collect()));
                                }
                            }
                        }
                        return Ok(Some(Vec::new()));
                    }
                }
                return Ok(None);
            }
            Ok(platform_exceptions()
                .methods
                .get(&key)
                .and_then(|(is_static, types)| {
                    (*is_static == static_call).then(|| {
                        types
                            .iter()
                            .map(|ty| Arc::<str>::from(ty.as_str()))
                            .collect()
                    })
                }))
        };
        let mut owner = original_owner;
        let mut seen = HashSet::new();
        let mut interfaces = Vec::new();
        let mut ancestry_complete = false;
        for _ in 0..64 {
            if !seen.insert(owner) {
                return unknown();
            }
            match local(owner) {
                Ok(Some(types)) => return Some(types),
                Err(()) => return unknown(),
                Ok(None) => {}
            }
            if !self.loaded_owners.contains(owner) {
                if owner != OBJECT {
                    return if self.loaded_owners.contains(original_owner) {
                        unknown()
                    } else {
                        None
                    };
                }
                if interfaces.is_empty() {
                    return None;
                }
                ancestry_complete = true;
                break;
            }
            if name == "<init>" {
                if static_call || ret != "V" {
                    return unknown();
                }
                let mut recovered = self
                    .recovered_constructors(owner)
                    .iter()
                    .filter(|constructor| constructor.parameters == args);
                let constructor = recovered.next()?;
                if recovered.next().is_some() {
                    return unknown();
                }
                return Some(constructor.thrown_types.clone());
            }
            let Some(entry) = self.entries.get(owner) else {
                return unknown();
            };
            if !static_call {
                interfaces.extend(entry.interfaces.iter().map(AsRef::as_ref));
            }
            let Some(parent) = entry.superclass.as_deref() else {
                ancestry_complete = true;
                break;
            };
            owner = parent;
        }
        if !ancestry_complete {
            return unknown();
        }
        let mut contracts = Vec::new();
        for _ in 0..64 {
            let Some(owner) = interfaces.pop() else {
                let first: Vec<Arc<str>> = contracts.pop()?;
                return if contracts.iter().all(|c| *c == first) {
                    Some(first)
                } else {
                    unknown()
                };
            };
            if !seen.insert(owner) {
                continue;
            }
            match local(owner) {
                Ok(Some(mut types)) => {
                    types.sort();
                    contracts.push(types);
                }
                Err(()) => return unknown(),
                Ok(None) => {
                    let Some(entry) = self.entries.get(owner) else {
                        return unknown();
                    };
                    interfaces.extend(entry.interfaces.iter().map(AsRef::as_ref));
                }
            }
        }
        unknown()
    }

    /// Exact loaded static declaration only; missing targets and overloads are not guessed.
    pub fn static_call_declares(
        &self,
        owner: &str,
        name: &str,
        args: &[Arc<str>],
        ret: &str,
        caught: &str,
    ) -> bool {
        self.call_declares(owner, name, args, ret, caught, true)
    }

    /// Exact declared exceptions from loaded methods or pinned Android API facts.
    /// Dispatch mode and descriptor must match; project definitions override SDK facts.
    pub fn call_declares(
        &self,
        owner: &str,
        name: &str,
        args: &[Arc<str>],
        ret: &str,
        caught: &str,
        static_call: bool,
    ) -> bool {
        if self.ambiguous.contains(owner) {
            return false;
        }
        let key = format!(
            "{owner}->{name}({}){ret}",
            args.iter().map(AsRef::as_ref).collect::<String>()
        );
        let types: Vec<&str> = if let Some((is_static, types)) = self.static_throws.get(&key) {
            if *is_static != static_call {
                return false;
            }
            types.iter().map(AsRef::as_ref).collect()
        } else if !self.loaded_owners.contains(owner) {
            let Some((is_static, types)) = platform_exceptions().methods.get(&key) else {
                return false;
            };
            if *is_static != static_call {
                return false;
            }
            types.iter().map(String::as_str).collect()
        } else {
            return false;
        };
        types.iter().any(|ty| {
            self.assignable(ty, "Ljava/lang/Throwable;") == Relation::Proven
                && (self.assignable(ty, caught) == Relation::Proven
                    || self.assignable(caught, ty) == Relation::Proven)
        })
    }

    /// Follow an exact pinned platform exception contract through loaded parents.
    /// An intervening declaration, including one without Throws metadata,
    /// stops the search rather than widening a narrowed Java override.
    pub fn inherited_file_exception_contract(
        &self,
        parent: &str,
        method: &crate::native_dex::DexMethod,
        exception: &str,
    ) -> bool {
        if method.name.as_ref() != "openFile"
            || method.return_type.as_ref() != "Landroid/os/ParcelFileDescriptor;"
            || !method
                .parameters
                .iter()
                .map(AsRef::as_ref)
                .eq(["Landroid/net/Uri;", "Ljava/lang/String;"])
        {
            return false;
        }
        self.inherited_platform_exception_contract(
            parent,
            method,
            exception,
            "Landroid/content/ContentProvider;",
        )
    }

    pub fn inherited_document_exception_contract(
        &self,
        parent: &str,
        method: &crate::native_dex::DexMethod,
        exception: &str,
    ) -> bool {
        if !pinned_document_exception_contract(method, exception) {
            return false;
        }
        self.inherited_platform_exception_contract(
            parent,
            method,
            exception,
            "Landroid/provider/DocumentsProvider;",
        )
    }

    fn inherited_platform_exception_contract(
        &self,
        parent: &str,
        method: &crate::native_dex::DexMethod,
        exception: &str,
        platform_owner: &str,
    ) -> bool {
        let mut owner = parent;
        let mut seen = HashSet::new();
        for _ in 0..256 {
            if self.ambiguous.contains(owner)
                || self.override_ambiguous_owners.contains(owner)
                || !seen.insert(owner)
            {
                return false;
            }
            let key = format!(
                "{owner}->{}({}){}",
                method.name,
                method
                    .parameters
                    .iter()
                    .map(AsRef::as_ref)
                    .collect::<String>(),
                method.return_type
            );
            if let Some((is_static, types)) = self.static_throws.get(&key) {
                return !is_static && types.iter().any(|ty| ty.as_ref() == exception);
            }
            if self.override_declarations.contains(&key) {
                return false;
            }
            if !self.loaded_owners.contains(owner) {
                if owner != platform_owner {
                    return false;
                }
                return platform_exceptions().methods.get(&key).is_some_and(
                    |(is_static, types)| !is_static && types.iter().any(|ty| ty == exception),
                );
            }
            let Some(parent) = self
                .entries
                .get(owner)
                .and_then(|entry| entry.superclass.as_deref())
            else {
                return false;
            };
            owner = parent;
        }
        false
    }

    /// Exact static linked target, proven varargs with no competing local or inherited
    /// method name. Unknown ancestry conservatively disables the display rewrite.
    pub fn is_unambiguous_object_call(&self, signature: &str) -> bool {
        self.object_calls.contains(signature)
    }

    pub fn is_unambiguous_object_varargs(&self, signature: &str) -> bool {
        self.object_varargs.contains(signature)
    }

    /// Optimizers can inline an empty constructor at an allocation site. Retarget
    /// only for an exact forwarding body, or a proven implicit default
    /// constructor calling the accessible direct parent's no-arg constructor.
    pub fn equivalent_noarg_constructor(&self, allocated: &str, invoked: &str) -> bool {
        if self.noninstantiable_owners.contains(allocated) {
            return false;
        }
        self.transparent_noarg_constructor_path(allocated, invoked)
    }

    /// These bodies are equivalent only on a fresh, untouched allocation.
    /// They must never authorize a super call or a general constructor rewrite.
    pub(crate) fn fresh_null_constructor(&self, allocated: &str) -> bool {
        !self.loaded_owners.contains("Ljava/lang/Object;")
            && !self.ambiguous.contains(allocated)
            && !self.override_ambiguous_owners.contains(allocated)
            && !self.noninstantiable_owners.contains(allocated)
            && self.fresh_null_constructors.contains(allocated)
    }

    /// Initialize a fresh Object-root instance early only when its constructor
    /// has no observable work and it cannot register a finalizer. Allocation
    /// stays at its original position; the instance must remain private until
    /// the original Object.<init> site.
    pub(crate) fn early_fresh_noarg_constructor(&self, allocated: &str) -> bool {
        !self.loaded_owners.contains("Ljava/lang/Object;")
            && !self.ambiguous.contains(allocated)
            && !self.override_ambiguous_owners.contains(allocated)
            && self
                .entries
                .get(allocated)
                .is_some_and(|entry| entry.superclass.as_deref() == Some("Ljava/lang/Object;"))
            && self
                .loaded_call_families
                .get(allocated)
                .is_some_and(|summary| !summary.methods.contains_key("finalize"))
            && (self.fresh_null_constructor(allocated)
                || self.equivalent_constructor(allocated, "Ljava/lang/Object;", &[]))
    }

    /// A Java super call can target an abstract parent, but the skipped
    /// constructors must still be exact, accessible no-arg forwarders.
    pub fn equivalent_noarg_super_constructor(&self, child: &str, invoked: &str) -> bool {
        if !self.has_accessible_noarg_super(child) || self.ambiguous.contains(child) {
            return false;
        }
        let Some(parent) = self
            .entries
            .get(child)
            .and_then(|entry| entry.superclass.as_deref())
        else {
            return false;
        };
        self.transparent_noarg_constructor_path(parent, invoked)
    }

    pub(crate) fn exact_public_sneaky_readable_field(
        &self,
        caller: &str,
        owner: &str,
        name: &str,
        descriptor: &str,
    ) -> bool {
        !self.ambiguous.contains(owner)
            && !self.override_ambiguous_owners.contains(owner)
            && (owner.contains('/') || !caller.contains('/'))
            && self
                .sneaky_readable_fields
                .get(owner)
                .and_then(|fields| fields.get(name))
                .is_some_and(|ty| ty.as_ref() == descriptor)
    }

    pub(crate) fn exact_public_writable_field(
        &self,
        owner: &str,
        name: &str,
        descriptor: &str,
    ) -> bool {
        !self.ambiguous.contains(owner)
            && !self.override_ambiguous_owners.contains(owner)
            && self
                .writable_fields
                .get(owner)
                .and_then(|fields| fields.get(name))
                .is_some_and(|ty| ty.as_ref() == descriptor)
    }

    pub fn equivalent_super_constructor(
        &self,
        child: &str,
        invoked: &str,
        args: &[Arc<str>],
    ) -> bool {
        if self.ambiguous.contains(child) || self.ambiguous.contains(invoked) {
            return false;
        }
        let Some(parent) = self
            .entries
            .get(child)
            .and_then(|e| e.superclass.as_deref())
        else {
            return false;
        };
        self.strict_superclass(parent, invoked) == Relation::Proven
            && self.recovered_constructors(parent).iter().any(|ctor| {
                ctor.observed_super
                    && ctor.invoked_owner.as_ref() == invoked
                    && ctor.parameters == args
            })
    }

    fn transparent_noarg_constructor_path(&self, allocated: &str, invoked: &str) -> bool {
        if self.strict_superclass(allocated, invoked) != Relation::Proven {
            return false;
        }
        let mut current = allocated;
        let mut visited = HashSet::new();
        for _ in 0..256 {
            if self.ambiguous.contains(current) || !visited.insert(current) {
                return false;
            }
            let Some(parent) = self.trivial_constructors.get(current) else {
                return false;
            };
            if parent.as_ref() == invoked {
                return true;
            }
            current = parent;
        }
        false
    }

    pub fn has_accessible_noarg_super(&self, owner: &str) -> bool {
        self.accessible_noarg_superclasses.contains(owner)
    }

    pub fn recovered_constructors(&self, owner: &str) -> &[RecoveredConstructor] {
        self.recovered_constructors
            .get(owner)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub fn equivalent_constructor(
        &self,
        allocated: &str,
        invoked: &str,
        args: &[Arc<str>],
    ) -> bool {
        (args.is_empty() && self.equivalent_noarg_constructor(allocated, invoked))
            || (!self.ambiguous.contains(allocated)
                && !self.ambiguous.contains(invoked)
                && self
                    .recovered_constructors(allocated)
                    .iter()
                    .any(|constructor| {
                        constructor.observed_allocation
                            && constructor.invoked_owner.as_ref() == invoked
                            && constructor.parameters == args
                    }))
    }

    /// Newly recovered unloaded-platform forwarders must not enter the
    /// historical allocation-staging path that moves allocation timing.
    pub(crate) fn strict_forwarding_constructor(
        &self,
        allocated: &str,
        invoked: &str,
        args: &[Arc<str>],
    ) -> bool {
        self.recovered_constructors(allocated)
            .iter()
            .any(|constructor| {
                constructor.observed_allocation
                    && constructor.strict_allocation_order
                    && constructor.invoked_owner.as_ref() == invoked
                    && constructor.parameters == args
            })
    }

    /// Prove a Java direct-interface qualifier for DEX nonvirtual default dispatch.
    /// The raw owner may be inherited by the qualifier, but no intervening or
    /// competing branch may redeclare its Java name/arity. Missing summaries
    /// retain the DEX body rather than guessing interface resolution.
    pub(crate) fn interface_super_qualifier(
        &self,
        class: &DexClass,
        owner: &str,
        name: &str,
        args: &[Arc<str>],
        ret: &str,
    ) -> Option<Arc<str>> {
        let signature = format!(
            "{name}({})",
            args.iter().map(AsRef::as_ref).collect::<String>()
        );
        if name.starts_with('<')
            || object_method_signature(&signature)
            || self.ambiguous.contains(owner)
        {
            return None;
        }
        let summary = self.loaded_call_families.get(owner)?;
        if !summary.public
            || summary.access_flags & 0x200 == 0
            || !summary.erased_interface_source_header
            || summary.varargs_names.contains(name)
        {
            return None;
        }
        let method = summary.methods.get(name)?.get(&args.len())?.as_ref()?;
        if method.parameters != args
            || method.return_type.as_ref() != ret
            || method.access_flags & 7 != 1
            || method.access_flags & (8 | 0x10 | 0x20 | 0x400 | 0x100 | 0x40 | 0x80 | 0x20000) != 0
            || !method.legal_source_declaration
            || method.generic_method
            || !method.declared_throws_empty
            || (method.may_infer_checked && !method.no_new_checked_header)
        {
            return None;
        }
        let mut selected = None;
        for direct in &class.interfaces {
            if self.assignable(direct, owner) != Relation::Proven {
                continue;
            }
            // Java rejects redundant qualifiers inherited through a superclass
            // or a more-specific direct interface, even for identical bodies.
            if class
                .superclass
                .as_deref()
                .is_some_and(|parent| self.assignable(parent, direct) != Relation::Disproven)
                || class.interfaces.iter().any(|other| {
                    other != direct && self.assignable(other, direct) != Relation::Disproven
                })
            {
                continue;
            }
            let mut pending = vec![(direct.clone(), false)];
            let mut seen = HashSet::new();
            let mut active = HashSet::new();
            let mut reached_owner = false;
            while let Some((current, leaving)) = pending.pop() {
                if leaving {
                    active.remove(&current);
                    continue;
                }
                if active.contains(&current) {
                    return None;
                }
                if !seen.insert(current.clone()) {
                    continue;
                }
                if seen.len() > 128 || self.ambiguous.contains(&current) {
                    return None;
                }
                // A loaded default shadows its platform ancestors. For an
                // unloaded leaf above that exact owner, the pinned complete
                // platform graph proves structural validity without needing a
                // loaded method summary. Never use this for a competing branch.
                if !self.loaded_owners.contains(&current)
                    && self.assignable(owner, &current) == Relation::Proven
                    && platform_interface_leaf(&current)
                {
                    continue;
                }
                let entry = self.entries.get(&current)?;
                let current_summary = self.loaded_call_families.get(&current)?;
                if !current_summary.public
                    || current_summary.access_flags & 0x200 == 0
                    || !current_summary.erased_interface_source_header
                {
                    return None;
                }
                active.insert(current.clone());
                pending.push((current.clone(), true));
                if current.as_ref() == owner {
                    reached_owner = true;
                }
                // The exact concrete declaration shadows its ancestors, but
                // their graph must still be complete and acyclic for Java.
                if self.assignable(owner, &current) != Relation::Proven
                    && (current_summary.varargs_names.contains(name)
                        || current_summary
                            .methods
                            .get(name)
                            .is_some_and(|family| family.contains_key(&args.len())))
                {
                    return None;
                }
                pending.extend(
                    entry
                        .interfaces
                        .iter()
                        .cloned()
                        .map(|parent| (parent, false)),
                );
            }
            if reached_owner && selected.replace(direct.clone()).is_some() {
                return None;
            }
        }
        selected
    }

    /// Strict superclass ancestry only: implemented interfaces must never
    /// authorize ordinary Java `super.method()` dispatch.
    pub fn strict_superclass(&self, source: &str, target: &str) -> Relation {
        if source == target {
            return Relation::Disproven;
        }
        let mut current = source;
        let mut seen = HashSet::new();
        for _ in 0..MAX_QUERY_NODES {
            if self.ambiguous.contains(current) || !seen.insert(current) {
                return Relation::Unknown;
            }
            let Some(entry) = self.entries.get(current) else {
                return Relation::Unknown;
            };
            let Some(parent) = entry.superclass.as_deref() else {
                return Relation::Disproven;
            };
            if self.ambiguous.contains(parent) {
                return Relation::Unknown;
            }
            if parent == target {
                return Relation::Proven;
            }
            current = parent;
        }
        Relation::Unknown
    }

    pub fn from_classes<'a>(classes: impl IntoIterator<Item = &'a DexClass>) -> Result<Self> {
        let classes: Vec<_> = classes.into_iter().take(MAX_CLASSES + 1).collect();
        ensure!(
            classes.len() <= MAX_CLASSES,
            "native class hierarchy exceeds class limit"
        );
        let mut static_throws = HashMap::new();
        let mut seen_owners = HashSet::new();
        let mut duplicate_owners = HashSet::new();
        for class in &classes {
            if !seen_owners.insert(&class.descriptor) {
                duplicate_owners.insert(&class.descriptor);
            }
        }
        // Index only declarations in the ancestry of a method that may infer
        // an explicit throw or an exact static-call exception. The raw word
        // scan is overinclusive but cannot miss a matching opcode; budgeted
        // families not fully indexed are rejected by the query.
        let classes_by_owner: HashMap<&str, &DexClass> = classes
            .iter()
            .map(|class| (class.descriptor.as_ref(), *class))
            .collect();
        let mut required_contracts = HashSet::new();
        let mut indexed_candidate_contracts = HashSet::new();
        for class in &classes {
            for method in &class.methods {
                let signature = format!(
                    "{}({})",
                    method.name,
                    method
                        .parameters
                        .iter()
                        .map(AsRef::as_ref)
                        .collect::<String>()
                );
                let pinned_io_candidate = PINNED_IO_CONTRACT_OWNERS.iter().any(|owner| {
                    matches!(
                        pinned_io_member(owner, &signature, &method.return_type),
                        Some(PinnedIoMember::Throws(_))
                    )
                });
                if method.access_flags & 2 != 0
                    || matches!(method.name.as_ref(), "<init>" | "<clinit>")
                    || !method.thrown_types.is_empty()
                    || !method.code.as_ref().is_some_and(|code| {
                        pinned_io_candidate
                            || future_headers::signature(method)
                            || code
                                .instructions
                                .iter()
                                .any(|word| matches!(*word as u8, 0x27 | 0x71 | 0x77))
                    })
                {
                    continue;
                }
                let static_method = method.access_flags & 8 != 0;
                let mut pending: Vec<&str> = class
                    .superclass
                    .iter()
                    .chain(class.interfaces.iter().filter(|_| !static_method))
                    .map(AsRef::as_ref)
                    .collect();
                let mut seen = HashSet::from([class.descriptor.as_ref()]);
                let mut complete = true;
                while let Some(owner) = pending.pop() {
                    if !seen.insert(owner) {
                        continue;
                    }
                    if seen.len() > MAX_QUERY_NODES {
                        complete = false;
                        break;
                    }
                    if owner == OBJECT {
                        continue;
                    }
                    let Some(parent) = classes_by_owner.get(owner) else {
                        continue;
                    };
                    if duplicate_owners.contains(&parent.descriptor) {
                        continue;
                    }
                    required_contracts.insert(format!("{owner}->{signature}"));
                    if required_contracts.len() > MAX_EDGES {
                        complete = false;
                        break;
                    }
                    pending.extend(
                        parent
                            .superclass
                            .iter()
                            .chain(parent.interfaces.iter().filter(|_| !static_method))
                            .map(AsRef::as_ref),
                    );
                }
                if complete {
                    indexed_candidate_contracts
                        .insert(format!("{}->{signature}", class.descriptor));
                }
            }
        }
        let mut method_contracts = HashMap::new();
        let mut ambiguous_method_contracts = HashSet::new();
        for class in &classes {
            if duplicate_owners.contains(&class.descriptor) {
                continue;
            }
            for method in &class.methods {
                if method.declaring_type != class.descriptor {
                    continue;
                }
                let signature = format!(
                    "{}({})",
                    method.name,
                    method
                        .parameters
                        .iter()
                        .map(AsRef::as_ref)
                        .collect::<String>()
                );
                let key = format!("{}->{signature}", class.descriptor);
                if !required_contracts.contains(&key) {
                    continue;
                }
                let inferred = if method.thrown_types.is_empty() {
                    inferred_root_static_call_throws(class, method, &signature, &classes_by_owner)
                } else {
                    Vec::new()
                };
                if method_contracts
                    .insert(
                        key.clone(),
                        MethodContract {
                            access_flags: method.access_flags,
                            return_type: method.return_type.clone(),
                            thrown_types: Arc::from(if method.thrown_types.is_empty() {
                                inferred.clone()
                            } else {
                                method.thrown_types.clone()
                            }),
                            inferred_throws: !inferred.is_empty(),
                            owner_final: class.access_flags & 0x10 != 0,
                        },
                    )
                    .is_some()
                {
                    ambiguous_method_contracts.insert(key);
                }
                ensure!(
                    method_contracts.len() <= MAX_EDGES,
                    "method contract index exceeds limit"
                );
            }
        }
        for class in &classes {
            if duplicate_owners.contains(&class.descriptor)
                || !class
                    .methods
                    .iter()
                    .any(|method| !method.thrown_types.is_empty())
            {
                continue;
            }
            let mut seen = HashSet::new();
            let mut duplicates = HashSet::new();
            for method in &class.methods {
                let key = format!(
                    "{}->{}({}){}",
                    class.descriptor,
                    method.name,
                    method
                        .parameters
                        .iter()
                        .map(AsRef::as_ref)
                        .collect::<String>(),
                    method.return_type
                );
                if !seen.insert(key.clone()) {
                    duplicates.insert(key.clone());
                }
                if !method.thrown_types.is_empty() && method.declaring_type == class.descriptor {
                    static_throws.insert(
                        key,
                        (
                            method.access_flags & 8 != 0,
                            Arc::from(method.thrown_types.clone()),
                        ),
                    );
                    ensure!(
                        static_throws.len() <= MAX_EDGES,
                        "declared exception index exceeds limit"
                    );
                }
            }
            for key in duplicates {
                static_throws.remove(&key);
            }
        }
        let override_declarations: HashSet<_> = classes
            .iter()
            .flat_map(|class| {
                class
                    .methods
                    .iter()
                    .filter(|method| {
                        method.declaring_type == class.descriptor
                            && (pinned_document_exception_contract(
                                method,
                                "Ljava/io/FileNotFoundException;",
                            ) || (method.name.as_ref() == "openFile"
                                && method.return_type.as_ref()
                                    == "Landroid/os/ParcelFileDescriptor;"
                                && method
                                    .parameters
                                    .iter()
                                    .map(AsRef::as_ref)
                                    .eq(["Landroid/net/Uri;", "Ljava/lang/String;"])))
                    })
                    .map(|method| {
                        format!(
                            "{}->{}({}){}",
                            class.descriptor,
                            method.name,
                            method
                                .parameters
                                .iter()
                                .map(AsRef::as_ref)
                                .collect::<String>(),
                            method.return_type,
                        )
                    })
            })
            .collect();
        ensure!(
            override_declarations.len() <= MAX_EDGES,
            "override declaration index exceeds limit"
        );
        let object_varargs = verified_object_calls(&classes, true);
        let object_calls = verified_object_calls(&classes, false);
        let implicit_constructors = verified_implicit_constructors(&classes);
        let mut recovered_constructors =
            constructor_recovery::recover(&classes, &implicit_constructors);
        let mut accessible_noarg_superclasses =
            constructor_recovery::accessible_noarg_superclasses(&classes);
        // A loaded parent with a proven implicit constructor is also a legal
        // super() target. Check class access from this particular child.
        for class in &classes {
            let Some(parent_type) = class.superclass.as_deref() else {
                continue;
            };
            if !implicit_constructors.contains_key(parent_type)
                || duplicate_owners.contains(&class.descriptor)
                || duplicate_owners.contains(&Arc::<str>::from(parent_type))
            {
                continue;
            }
            let Some(parent) = classes_by_owner.get(parent_type) else {
                continue;
            };
            let child_package = class
                .descriptor
                .rsplit_once('/')
                .map_or("", |(prefix, _)| prefix);
            let parent_package = parent_type
                .rsplit_once('/')
                .map_or("", |(prefix, _)| prefix);
            if parent.access_flags & 1 != 0 || child_package == parent_package {
                accessible_noarg_superclasses.insert(class.descriptor.clone());
            }
        }
        // Explicit parameter constructors suppress Java's implicit default.
        // Preserve an already-proven accessible default when adding overloads.
        for (owner, constructors) in &mut recovered_constructors {
            if let Some(parent) = implicit_constructors.get(owner)
                && !constructors
                    .iter()
                    .any(|constructor| constructor.parameters.is_empty())
            {
                constructors.insert(
                    0,
                    RecoveredConstructor {
                        access_flags: 1,
                        parent: parent.clone(),
                        invoked_owner: parent.clone(),
                        parameters: vec![],
                        thrown_types: vec![],
                        strict_allocation_order: false,
                        observed_allocation: true,
                        observed_super: false,
                    },
                );
            }
        }
        let mut entries = platform_entries();
        let mut ambiguous = HashSet::new();
        let mut trivial_constructors = HashMap::new();
        let mut fresh_null_constructors = HashSet::new();
        let mut constructor_conflicts = HashSet::new();
        let mut constructor_seen = HashSet::new();
        let mut count = 0usize;
        let mut edges = entries
            .values()
            .map(|entry| usize::from(entry.superclass.is_some()) + entry.interfaces.len())
            .sum::<usize>();

        let loaded_owners: HashSet<Arc<str>> =
            classes.iter().map(|c| c.descriptor.clone()).collect();
        let mut loaded_call_families = HashMap::new();
        let mut readability_families = 0usize;
        let mut future_words_left = 1_000_000usize;
        for class in &classes {
            if duplicate_owners.contains(&class.descriptor) {
                continue;
            }
            let Some(summary) = summarize_loaded_call_owner(class, &mut future_words_left) else {
                continue;
            };
            let added = summary.methods.values().fold(0usize, |count, families| {
                count.saturating_add(families.len())
            });
            readability_families = readability_families.saturating_add(added);
            if readability_families > MAX_READABILITY_FAMILIES {
                // A missing certificate keeps the original cast. Do not add
                // unbounded readability metadata to a large project.
                loaded_call_families.clear();
                break;
            }
            loaded_call_families.insert(class.descriptor.clone(), summary);
        }
        // Retain metadata only for original field-store references in bounded
        // constructor catchall bodies. These seeds grant no field authority.
        let mut field_queries: HashMap<Arc<str>, HashSet<Arc<str>>> = HashMap::new();
        let mut seed_words = 0usize;
        let mut seed_queries = 0usize;
        for class in &classes {
            for method in &class.methods {
                let Some(code) = method.code.as_ref() else {
                    continue;
                };
                if method.name.as_ref() != "<init>"
                    || code.instructions.len() > 4096
                    || code.registers > 256
                    || code.try_regions.len() > 16
                    || code
                        .try_regions
                        .iter()
                        .map(|r| r.catches.len())
                        .sum::<usize>()
                        > 64
                    || !code
                        .try_regions
                        .iter()
                        .any(|r| r.catches.iter().any(|(ty, _)| ty.is_none()))
                {
                    continue;
                }
                if seed_words.saturating_add(code.instructions.len()) > MAX_QUERY_NODES {
                    continue;
                }
                seed_words += code.instructions.len();
                let mut pc = 0;
                while pc < code.instructions.len() {
                    let Ok((width, payload)) =
                        crate::native_cfg::instruction_width(&code.instructions, pc)
                    else {
                        break;
                    };
                    if width == 0 {
                        break;
                    }
                    if !payload
                        && matches!(code.instructions[pc] as u8, 0x59..=0x5f)
                        && seed_queries < MAX_CACHE_ENTRIES
                        && let Some(&(owner, _, name)) = code
                            .instructions
                            .get(pc + 1)
                            .and_then(|index| class.symbols.fields.get(*index as usize))
                        && let (Some(owner), Some(name)) = (
                            class.symbols.types.get(owner as usize),
                            class.symbols.strings.get(name as usize),
                        )
                        && owner != &class.descriptor
                        && owner.len() <= MAX_CACHE_KEY_BYTES
                        && name.len() <= MAX_CACHE_KEY_BYTES
                    {
                        let names = field_queries.entry(owner.clone()).or_default();
                        if names.len() < 256 && names.insert(Arc::from(name.as_str())) {
                            seed_queries += 1;
                        }
                    }
                    pc += width;
                }
            }
        }
        seed_typed_constructor_field_queries(
            &classes,
            &mut field_queries,
            &mut seed_words,
            &mut seed_queries,
        );
        let mut writable_fields = HashMap::new();
        for class in &classes {
            let Some(requested) = field_queries.get(&class.descriptor) else {
                continue;
            };
            if duplicate_owners.contains(&class.descriptor)
                || class.access_flags & 7 != 1
                || class.access_flags & !0x1411 != 0
                || class.descriptor.contains('$')
                || class.fields.len() > 256
            {
                continue;
            }
            let mut seen = HashSet::new();
            let mut fields = HashMap::new();
            let mut unambiguous = true;
            for field in &class.fields {
                if field.declaring_type != class.descriptor || !seen.insert(field.name.clone()) {
                    unambiguous = false;
                    break;
                }
                if requested.contains(field.name.as_ref())
                    && !field.is_static
                    && field.access_flags & (1 | 2 | 4 | 8 | 16) == 1
                    && field.access_flags & !0x10df == 0
                {
                    fields.insert(field.name.clone(), field.field_type.clone());
                }
            }
            if unambiguous {
                writable_fields.insert(class.descriptor.clone(), fields);
            }
        }
        // Retain exact source authority only for the instance field read in the
        // bounded static sneaky-rethrow shape. The fixed-width prefilter is
        // constant work per method; exhausted query capacity clears all grants.
        let mut sneaky_field_queries: HashMap<Arc<str>, HashSet<Arc<str>>> = HashMap::new();
        let mut sneaky_source_call_demands = HashSet::new();
        let mut sneaky_query_count = 0usize;
        let mut sneaky_scanned_methods = 0usize;
        let mut sneaky_key_bytes = 0usize;
        let mut sneaky_complete = true;
        for candidate in &classes {
            for method in &candidate.methods {
                sneaky_scanned_methods = sneaky_scanned_methods.saturating_add(1);
                if sneaky_scanned_methods > 1_000_000 {
                    sneaky_complete = false;
                    break;
                }
                let Some(code) = method.code.as_ref() else {
                    continue;
                };
                if method.access_flags != 9
                    || method.return_type.as_ref() != "V"
                    || method.parameters.len() != 1
                    || method.parameters[0].as_ref() != "Ljava/lang/Object;"
                    || code.registers != 2
                    || code.ins != 1
                    || code.outs != 2
                    || code.tries != 0
                    || !code.try_regions.is_empty()
                    || code.instructions.len() != 15
                    || code.instructions[7] != 0x1154
                {
                    continue;
                }
                let Some(&(owner_index, ty_index, name_index)) =
                    candidate.symbols.fields.get(code.instructions[8] as usize)
                else {
                    sneaky_complete = false;
                    break;
                };
                let (Some(owner), Some(ty), Some(name)) = (
                    candidate.symbols.types.get(owner_index as usize).cloned(),
                    candidate.symbols.types.get(ty_index as usize).cloned(),
                    candidate
                        .symbols
                        .strings
                        .get(name_index as usize)
                        .map(|value| Arc::<str>::from(value.as_str())),
                ) else {
                    sneaky_complete = false;
                    break;
                };
                if ty.as_ref() != "Ljava/lang/Throwable;"
                    || owner.len() > MAX_CACHE_KEY_BYTES
                    || name.len() > MAX_CACHE_KEY_BYTES
                {
                    continue;
                }
                sneaky_key_bytes = sneaky_key_bytes
                    .saturating_add(owner.len())
                    .saturating_add(name.len());
                if sneaky_key_bytes > 4 * 1024 * 1024 {
                    sneaky_complete = false;
                    break;
                }
                if code.instructions[11] as u8 == 0x71 {
                    let Some(&(call_owner_index, call_proto_index, call_name_index)) = candidate
                        .symbols
                        .methods
                        .get(code.instructions[12] as usize)
                    else {
                        sneaky_complete = false;
                        break;
                    };
                    let (Some(call_owner), Some((call_ret, call_args)), Some(call_name)) = (
                        candidate.symbols.types.get(call_owner_index as usize),
                        candidate.symbols.protos.get(call_proto_index as usize),
                        candidate.symbols.strings.get(call_name_index as usize),
                    ) else {
                        sneaky_complete = false;
                        break;
                    };
                    let key = format!(
                        "{call_owner}->{call_name}({}){call_ret}",
                        call_args.iter().map(AsRef::as_ref).collect::<String>()
                    );
                    sneaky_key_bytes = sneaky_key_bytes.saturating_add(key.len());
                    if key.len() > MAX_CACHE_KEY_BYTES
                        || sneaky_key_bytes > 4 * 1024 * 1024
                        || (sneaky_source_call_demands.insert(key)
                            && sneaky_source_call_demands.len() > MAX_CACHE_ENTRIES)
                    {
                        sneaky_complete = false;
                        break;
                    }
                }
                let names = sneaky_field_queries.entry(owner).or_default();
                if names.insert(name) {
                    sneaky_query_count = sneaky_query_count.saturating_add(1);
                    if sneaky_query_count > MAX_CACHE_ENTRIES || names.len() > 256 {
                        sneaky_complete = false;
                        break;
                    }
                }
            }
            if !sneaky_complete {
                break;
            }
        }
        if !sneaky_complete {
            sneaky_field_queries.clear();
            sneaky_source_call_demands.clear();
        }
        let mut sneaky_readable_fields = HashMap::new();
        for declaration in &classes {
            let Some(requested) = sneaky_field_queries.get(&declaration.descriptor) else {
                continue;
            };
            if duplicate_owners.contains(&declaration.descriptor)
                || declaration.access_flags & 7 != 1
                || declaration.access_flags & !0x7631 != 0
                || declaration.access_flags & (0x200 | 0x2000 | 0x4000) != 0
                || declaration.descriptor.contains('$')
                || declaration.fields.len() > 256
            {
                continue;
            }
            let mut seen = HashSet::new();
            let mut fields = HashMap::new();
            let mut complete = true;
            for field in &declaration.fields {
                if field.declaring_type != declaration.descriptor
                    || !seen.insert(field.name.clone())
                {
                    complete = false;
                    break;
                }
                if requested.contains(field.name.as_ref())
                    && !field.is_static
                    && field.access_flags & 7 == 1
                    && field.access_flags & 8 == 0
                    && field.access_flags & !0x10df == 0
                    && field.access_flags & 0x50 != 0x50
                {
                    fields.insert(field.name.clone(), field.field_type.clone());
                }
            }
            if complete {
                sneaky_readable_fields.insert(declaration.descriptor.clone(), fields);
            }
        }
        let noninstantiable_owners = classes
            .iter()
            .filter(|class| class.access_flags & (0x200 | 0x400 | 0x4000) != 0)
            .map(|class| class.descriptor.clone())
            .collect();
        for class in &classes {
            count = count.saturating_add(1);
            ensure!(
                count <= MAX_CLASSES,
                "native class hierarchy exceeds class limit"
            );
            let descriptor = Arc::clone(&class.descriptor);
            if null_writing_object_constructor(class) {
                fresh_null_constructors.insert(descriptor.clone());
            }
            let constructor = trivial_noarg_constructor(class)
                .or_else(|| implicit_constructors.get(&class.descriptor).cloned());
            if !constructor_seen.insert(Arc::clone(&descriptor)) {
                if trivial_constructors.get(&descriptor) != constructor.as_ref() {
                    constructor_conflicts.insert(Arc::clone(&descriptor));
                }
            } else if let Some(owner) = constructor {
                trivial_constructors.insert(Arc::clone(&descriptor), owner);
            }

            let entry = TypeEntry {
                superclass: class.superclass.clone(),
                interfaces: class.interfaces.clone().into(),
            };
            edges = edges
                .saturating_add(usize::from(entry.superclass.is_some()) + entry.interfaces.len());
            ensure!(
                edges <= MAX_EDGES,
                "native class hierarchy exceeds edge limit"
            );
            match entries.get(descriptor.as_ref()) {
                Some(existing) if existing == &entry => continue,
                Some(_) => {
                    ambiguous.insert(Arc::clone(&descriptor));
                }
                None => {}
            }
            entries.insert(descriptor, entry);
        }

        trivial_constructors.retain(|descriptor, _| !constructor_conflicts.contains(descriptor));
        let mut hierarchy = Self {
            static_throws: Arc::new(static_throws),
            method_contracts: Arc::new(method_contracts),
            ambiguous_method_contracts: Arc::new(ambiguous_method_contracts),
            indexed_candidate_contracts: Arc::new(indexed_candidate_contracts),
            checked_callers: Arc::new(CheckedCallerIndex::default()),
            override_declarations: Arc::new(override_declarations),
            override_ambiguous_owners: Arc::new(duplicate_owners.into_iter().cloned().collect()),
            loaded_owners: Arc::new(loaded_owners),
            writable_fields: Arc::new(writable_fields),
            sneaky_readable_fields: Arc::new(sneaky_readable_fields),
            loaded_call_families: Arc::new(loaded_call_families),
            static_sneaky_source_headers: Arc::new(HashSet::new()),
            enum_array_retainers: Arc::new(enum_array_retainer::collect(&classes)),
            checked_call_certificates: Arc::new(Default::default()),
            noninstantiable_owners: Arc::new(noninstantiable_owners),
            trivial_constructors: Arc::new(trivial_constructors),
            fresh_null_constructors: Arc::new(fresh_null_constructors),
            recovered_constructors: Arc::new(recovered_constructors),
            accessible_noarg_superclasses: Arc::new(accessible_noarg_superclasses),
            object_varargs: Arc::new(object_varargs),
            object_calls: Arc::new(object_calls),
            entries: Arc::new(entries),
            ambiguous: Arc::new(ambiguous),
            cache: Arc::new(Mutex::new(RelationCache::default())),
        };
        hierarchy.checked_callers = Arc::new(checked_caller_index(&hierarchy, &classes));
        hierarchy.checked_call_certificates =
            Arc::new(checked_call_certificates::build(&hierarchy, &classes));
        // Prove only the declaration writers that can append a generated header.
        // The transient walk retains no bodies, calls, or runtime-effect claims.
        let mut closed_headers = Vec::new();
        let mut header_throw_budget = 256_000_000usize;
        if hierarchy.checked_callers.ready && hierarchy.checked_callers.complete {
            // Existing exact protected/declaration callsites are the only seeds.
            // No extra method walk or retained body/caller index is needed.
            let mut header_keys: Vec<_> = hierarchy.checked_callers.conditional.keys().collect();
            header_keys.sort();
            for key in header_keys {
                if !hierarchy.checked_callers.candidates.contains(key)
                    || hierarchy.checked_callers.blocked.contains(key)
                {
                    continue;
                }
                let Some((owner, signature)) = key.split_once("->") else {
                    continue;
                };
                let Some(class) = classes_by_owner.get(owner) else {
                    continue;
                };
                let Some((name, prototype)) = signature.split_once('(') else {
                    continue;
                };
                let Some((args, ret)) = prototype.split_once(')') else {
                    continue;
                };
                let Some(summary) = hierarchy.loaded_call_families.get(owner) else {
                    continue;
                };
                let mut matches = class.methods.iter().filter(|method| {
                    method.name.as_ref() == name
                        && method.return_type.as_ref() == ret
                        && method
                            .parameters
                            .iter()
                            .map(AsRef::as_ref)
                            .collect::<String>()
                            == args
                });
                let Some(method) = matches.next() else {
                    continue;
                };
                if matches.next().is_some() || method.access_flags & 2 != 0 {
                    continue;
                }
                let candidate = summary
                    .methods
                    .get(name)
                    .and_then(|family| family.get(&method.parameters.len()))
                    .and_then(|candidate| {
                        candidate.as_ref().or_else(|| {
                            summary
                                .exact_overloads
                                .as_ref()
                                .and_then(|exact| exact.get(name))
                                .and_then(|family| family.get(&method.parameters))
                                .and_then(Option::as_ref)
                        })
                    });
                if candidate.is_some_and(|candidate| {
                    candidate.may_infer_checked
                        && candidate.parameters == method.parameters
                        && candidate.return_type == method.return_type
                }) && complete_no_checked_header(
                    class,
                    method,
                    &hierarchy,
                    &mut header_throw_budget,
                ) {
                    closed_headers.push((
                        class.descriptor.clone(),
                        method.name.clone(),
                        method.parameters.clone(),
                    ));
                }
            }
        }

        let mut static_sneaky_source_headers = HashSet::new();
        if hierarchy.checked_callers.ready
            && hierarchy.checked_callers.complete
            && !sneaky_source_call_demands.is_empty()
        {
            const LOCAL_WORK: usize = 1_000_000;
            const AGGREGATE_WORK: usize = 64_000_000;
            const AGGREGATE_KEY_BYTES: usize = 4 * 1024 * 1024;
            let root_sneaky_demands = sneaky_source_call_demands;
            let mut pending: BTreeSet<String> = root_sneaky_demands.iter().cloned().collect();
            let mut summaries = BTreeMap::<String, SneakyHeaderSummary>::new();
            let mut seen = HashSet::new();
            let mut dependency_words = 0usize;
            let mut reserved_work = 0usize;
            let mut key_bytes = pending.iter().map(String::len).sum::<usize>();
            let mut method_indexes = HashMap::<String, BTreeMap<String, Option<usize>>>::new();
            let mut indexed_methods = 0usize;
            let mut indexed_descriptor_bytes = 0usize;
            let mut dependency_edges = 0usize;
            let mut dependency_edge_bytes = 0usize;
            let mut dependency_complete =
                pending.len() <= MAX_CACHE_ENTRIES && key_bytes <= AGGREGATE_KEY_BYTES;
            while dependency_complete {
                let Some(key) = pending.pop_first() else {
                    break;
                };
                if !seen.insert(key.clone()) {
                    continue;
                }
                if seen.len() > MAX_CACHE_ENTRIES {
                    dependency_complete = false;
                    break;
                }
                let Some((owner, signature)) = key.split_once("->") else {
                    dependency_complete = false;
                    break;
                };
                let Some(class) = classes_by_owner.get(owner) else {
                    dependency_complete = false;
                    break;
                };
                if !method_indexes.contains_key(owner) {
                    let mut index = BTreeMap::new();
                    for (method_index, method) in class.methods.iter().enumerate() {
                        indexed_methods = indexed_methods.saturating_add(1);
                        let bytes = method
                            .name
                            .len()
                            .saturating_add(method.return_type.len())
                            .saturating_add(
                                method.parameters.iter().map(|ty| ty.len()).sum::<usize>(),
                            )
                            .saturating_add(2);
                        indexed_descriptor_bytes = indexed_descriptor_bytes.saturating_add(bytes);
                        if indexed_methods > 1_000_000
                            || indexed_descriptor_bytes > 32 * 1024 * 1024
                        {
                            dependency_complete = false;
                            break;
                        }
                        let signature = format!(
                            "{}({}){}",
                            method.name,
                            method
                                .parameters
                                .iter()
                                .map(AsRef::as_ref)
                                .collect::<String>(),
                            method.return_type,
                        );
                        match index.entry(signature) {
                            std::collections::btree_map::Entry::Vacant(entry) => {
                                entry.insert(Some(method_index));
                            }
                            std::collections::btree_map::Entry::Occupied(mut entry) => {
                                entry.insert(None);
                            }
                        }
                    }
                    if !dependency_complete {
                        break;
                    }
                    method_indexes.insert(owner.to_owned(), index);
                }
                let Some((name, prototype)) = signature.split_once('(') else {
                    dependency_complete = false;
                    break;
                };
                let Some((args, ret)) = prototype.split_once(')') else {
                    dependency_complete = false;
                    break;
                };
                let Some(summary) = hierarchy.loaded_call_families.get(owner) else {
                    dependency_complete = false;
                    break;
                };
                let Some(Some(method_index)) = method_indexes
                    .get(owner)
                    .and_then(|index| index.get(signature))
                else {
                    dependency_complete = false;
                    break;
                };
                let Some(method) = class.methods.get(*method_index) else {
                    dependency_complete = false;
                    break;
                };
                if method.name.as_ref() != name
                    || method.return_type.as_ref() != ret
                    || method
                        .parameters
                        .iter()
                        .map(AsRef::as_ref)
                        .collect::<String>()
                        != args
                    || method.access_flags & 2 != 0
                {
                    dependency_complete = false;
                    break;
                }
                let candidate = summary
                    .methods
                    .get(name)
                    .and_then(|family| family.get(&method.parameters.len()))
                    .and_then(|candidate| {
                        candidate.as_ref().or_else(|| {
                            summary
                                .exact_overloads
                                .as_ref()
                                .and_then(|exact| exact.get(name))
                                .and_then(|family| family.get(&method.parameters))
                                .and_then(Option::as_ref)
                        })
                    });
                let Some(candidate) = candidate else {
                    dependency_complete = false;
                    break;
                };
                if candidate.parameters != method.parameters
                    || candidate.return_type != method.return_type
                {
                    dependency_complete = false;
                    break;
                }
                if !candidate.may_infer_checked || candidate.no_new_checked_header {
                    continue;
                }
                let Some(code) = method.code.as_ref() else {
                    dependency_complete = false;
                    break;
                };
                if code.instructions.is_empty()
                    || code.instructions.len() > 4096
                    || code.registers > 1024
                    || code.ins > code.registers
                    || code.tries != 0
                    || !code.try_regions.is_empty()
                {
                    continue;
                }
                dependency_words = dependency_words.saturating_add(code.instructions.len());
                if dependency_words > 1_000_000 {
                    dependency_complete = false;
                    break;
                }
                let decoded = match crate::native_ir::DecodedMethod::decode(code) {
                    Ok(decoded) => decoded,
                    Err(_) => {
                        dependency_complete = false;
                        break;
                    }
                };
                let needs_analysis = decoded
                    .instructions
                    .iter()
                    .any(|instruction| instruction.opcode == 0x27);
                if needs_analysis
                    && !reserve_sneaky_analysis_work(&mut reserved_work, LOCAL_WORK, AGGREGATE_WORK)
                {
                    dependency_complete = false;
                    break;
                }
                let prepared =
                    match prepare_sneaky_no_checked_header(class, method, &hierarchy, LOCAL_WORK) {
                        Ok(Some(prepared)) => prepared,
                        Ok(None) => continue,
                        Err(_) => {
                            dependency_complete = false;
                            break;
                        }
                    };
                for dependency in &prepared.dependencies {
                    dependency_edges = dependency_edges.saturating_add(1);
                    dependency_edge_bytes = dependency_edge_bytes.saturating_add(dependency.len());
                    if dependency_edges > 65_536 || dependency_edge_bytes > 4 * 1024 * 1024 {
                        dependency_complete = false;
                        break;
                    }
                    if !seen.contains(dependency) && pending.insert(dependency.clone()) {
                        key_bytes = key_bytes.saturating_add(dependency.len());
                        if pending.len().saturating_add(seen.len()) > MAX_CACHE_ENTRIES
                            || key_bytes > AGGREGATE_KEY_BYTES
                        {
                            dependency_complete = false;
                            break;
                        }
                    }
                }
                if dependency_complete {
                    summaries.insert(key, prepared);
                }
            }
            if dependency_complete {
                let proven = close_sneaky_header_summaries(&summaries);
                static_sneaky_source_headers.extend(
                    root_sneaky_demands
                        .into_iter()
                        .filter(|key| proven.contains(key)),
                );
            }
        }
        if let Some(families) = Arc::get_mut(&mut hierarchy.loaded_call_families) {
            for (owner, name, parameters) in closed_headers {
                let Some(summary) = families.get_mut(&owner) else {
                    continue;
                };
                let Some(family) = summary
                    .methods
                    .get_mut(&name)
                    .and_then(|family| family.get_mut(&parameters.len()))
                else {
                    continue;
                };
                let candidate = if let Some(candidate) = family {
                    Some(candidate)
                } else {
                    summary
                        .exact_overloads
                        .as_mut()
                        .and_then(|exact| exact.get_mut(&name))
                        .and_then(|family| family.get_mut(&parameters))
                        .and_then(Option::as_mut)
                };
                if let Some(candidate) = candidate {
                    candidate.no_new_checked_header = true;
                    candidate.may_infer_checked = false;
                }
            }
        }
        hierarchy.static_sneaky_source_headers = Arc::new(static_sneaky_source_headers);

        Ok(hierarchy)
    }

    /// Determines whether a value of `source` can be assigned to `target`.
    /// `Unknown` means at least one required external edge is unavailable or a
    /// malformed cycle prevents a safe negative conclusion.
    pub fn assignable(&self, source: &str, target: &str) -> Relation {
        if (source.starts_with('[') && !valid_array(source))
            || (target.starts_with('[') && !valid_array(target))
        {
            return Relation::Unknown;
        }
        if source == target {
            return Relation::Proven;
        }
        if source.len() <= MAX_CACHE_KEY_BYTES && target.len() <= MAX_CACHE_KEY_BYTES {
            let cache = self
                .cache
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            if let Some(relation) = cache.get(source, target) {
                return relation;
            }
        }
        let relation = self.assignable_uncached(source, target, MAX_QUERY_NODES, MAX_QUERY_EDGES);
        self.cache
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .insert(source, target, relation);
        relation
    }

    /// An APK definition with this descriptor replaces pinned SDK facts.
    /// Presence here proves SDK ancestry metadata, not arbitrary method behavior.
    pub(crate) fn unshadowed_sdk_type(&self, descriptor: &str) -> bool {
        platform_exceptions().types.contains_key(descriptor)
            && !self.loaded_owners.contains(descriptor)
            && !self.ambiguous.contains(descriptor)
    }

    /// The SDK's six numeric wrappers directly extend its Number class.
    /// An APK definition with the same descriptor replaces that SDK fact.
    pub(crate) fn sdk_numeric_wrapper_to_number(&self, source: &str) -> bool {
        const NUMBER: &str = "Ljava/lang/Number;";
        matches!(
            source,
            "Ljava/lang/Byte;"
                | "Ljava/lang/Short;"
                | "Ljava/lang/Integer;"
                | "Ljava/lang/Long;"
                | "Ljava/lang/Float;"
                | "Ljava/lang/Double;"
        ) && self.unshadowed_sdk_type(source)
            && self.unshadowed_sdk_type(NUMBER)
            && platform_exceptions()
                .types
                .get(source)
                .is_some_and(|(parent, _)| parent.as_deref() == Some(NUMBER))
            && self.assignable(source, NUMBER) == Relation::Proven
    }

    /// An exact loaded declaration whose Java name/arity has no competitor.
    /// Static calls need a root-Object owner so inherited overloads cannot
    /// change source-level selection. Constructors do not inherit overloads.
    /// Exact static dispatch plus a complete loaded-body proof. A name or
    /// annotation alone never authorizes removing a normal continuation.
    pub(crate) fn loaded_static_void_never_returns(
        &self,
        owner: &str,
        name: &str,
        args: &[Arc<str>],
    ) -> bool {
        if self.ambiguous.contains(owner) {
            return false;
        }
        let Some(summary) = self.loaded_call_families.get(owner) else {
            return false;
        };
        let method = summary
            .methods
            .get(name)
            .and_then(|family| family.get(&args.len()))
            .and_then(Option::as_ref)
            .or_else(|| {
                summary
                    .exact_overloads
                    .as_ref()
                    .and_then(|overloads| overloads.get(name))
                    .and_then(|overloads| overloads.get(args))
                    .and_then(Option::as_ref)
            });
        method.is_some_and(|method| {
            method.parameters.as_slice() == args
                && method.return_type.as_ref() == "V"
                && method.access_flags & 8 != 0
                && method.static_no_return
        })
    }

    pub(crate) fn unambiguous_loaded_call(
        &self,
        owner: &str,
        name: &str,
        args: &[Arc<str>],
        ret: &str,
        opcode: u8,
    ) -> bool {
        let Some(summary) = self.loaded_call_families.get(owner) else {
            return false;
        };
        if !summary.public || self.ambiguous.contains(owner) || summary.varargs_names.contains(name)
        {
            return false;
        }
        let Some(method) = summary
            .methods
            .get(name)
            .and_then(|families| families.get(&args.len()))
            .and_then(Option::as_ref)
        else {
            return false;
        };
        if method.access_flags & 1 == 0
            || method.access_flags & (0x40 | 0x80) != 0
            || method.annotated
            || method.return_type.as_ref() != ret
            || method.parameters.as_slice() != args
        {
            return false;
        }
        match opcode {
            0x71 | 0x77 => {
                summary.root_object
                    && self.unshadowed_sdk_type(OBJECT)
                    && method.access_flags & 8 != 0
            }
            0x70 | 0x76 => {
                name == "<init>"
                    && ret == "V"
                    && method.access_flags & 8 == 0
                    && !summary.generic_class
            }
            0x6e | 0x74 => {
                name != "<init>"
                    && method.access_flags & (8 | 0x1000) == 0
                    && !summary.generic_class
            }
            _ => false,
        }
    }

    /// Argument type changes cannot expose an overload inherited from an
    /// unaudited superclass. The receiver-only noarg-void proof is separate.
    pub(crate) fn unambiguous_loaded_argument_call(
        &self,
        owner: &str,
        name: &str,
        args: &[Arc<str>],
        ret: &str,
        opcode: u8,
    ) -> bool {
        self.unambiguous_loaded_call(owner, name, args, ret, opcode)
            && (!matches!(opcode, 0x6e | 0x74)
                || self
                    .loaded_call_families
                    .get(owner)
                    .is_some_and(|summary| summary.root_object)
                    && self.unshadowed_sdk_type(OBJECT))
    }

    /// Audit every loaded superclass between a more specific receiver and
    /// its invocation owner. An unaudited external step or an incompatible
    /// zero-argument declaration retains the explicit owner cast.
    pub(crate) fn noarg_void_upcast_path(&self, source: &str, owner: &str, name: &str) -> bool {
        if self.assignable(source, owner) != Relation::Proven {
            return false;
        }
        let mut current = source;
        let mut seen = HashSet::new();
        for _ in 0..256 {
            if current == owner {
                return true;
            }
            if !seen.insert(current) || self.ambiguous.contains(current) {
                return false;
            }
            let (Some(entry), Some(summary)) = (
                self.entries.get(current),
                self.loaded_call_families.get(current),
            ) else {
                return false;
            };
            if summary.varargs_names.contains(name) {
                return false;
            }
            if let Some(family) = summary
                .methods
                .get(name)
                .and_then(|methods| methods.get(&0))
            {
                let Some(method) = family else {
                    return false;
                };
                if !method.parameters.is_empty()
                    || method.return_type.as_ref() != "V"
                    || method.access_flags & 1 == 0
                    || method.access_flags & (8 | 0x40 | 0x80 | 0x1000) != 0
                    || method.annotated
                {
                    return false;
                }
            }
            let Some(parent) = entry.superclass.as_deref() else {
                return false;
            };
            current = parent;
        }
        false
    }

    fn assignable_uncached(
        &self,
        source: &str,
        target: &str,
        max_nodes: usize,
        max_edges: usize,
    ) -> Relation {
        // JLS 4.10.3: array covariance applies to reference components only.
        // Strip dimensions iteratively so adversarial descriptors cannot recurse.
        let (mut source, mut target) = (source, target);
        if (source.starts_with('[') && !valid_array(source))
            || (target.starts_with('[') && !valid_array(target))
        {
            return Relation::Unknown;
        }
        loop {
            match (source.strip_prefix('['), target.strip_prefix('[')) {
                (Some(left), Some(right)) => {
                    if primitive(left) || primitive(right) {
                        return if left == right {
                            Relation::Proven
                        } else {
                            Relation::Disproven
                        };
                    }
                    source = left;
                    target = right;
                }
                (Some(_), None) => {
                    return if matches!(
                        target,
                        "Ljava/lang/Object;" | "Ljava/lang/Cloneable;" | "Ljava/io/Serializable;"
                    ) {
                        Relation::Proven
                    } else {
                        Relation::Disproven
                    };
                }
                (None, Some(_)) => return Relation::Disproven,
                (None, None) => break,
            }
        }
        // Every well-formed class/interface reference is assignable to Object,
        // including classes whose external definition is unavailable.
        if target == OBJECT && valid_class(source) {
            return Relation::Proven;
        }
        let mut queue = VecDeque::from([Arc::<str>::from(source)]);
        let mut reachable = HashSet::new();
        let mut unknown = false;
        let mut edges = 0usize;
        while let Some(current) = queue.pop_front() {
            if !reachable.insert(Arc::clone(&current)) {
                continue;
            }
            if reachable.len() > max_nodes {
                return Relation::Unknown;
            }
            if current.as_ref() == target {
                return Relation::Proven;
            }
            if self.ambiguous.contains(current.as_ref()) {
                unknown = true;
                continue;
            }
            let Some(entry) = self.entries.get(current.as_ref()) else {
                unknown = true;
                continue;
            };
            if current.as_ref() != OBJECT
                && entry.superclass.is_none()
                && entry.interfaces.is_empty()
            {
                unknown = true;
            }
            for parent in entry.superclass.iter().chain(entry.interfaces.iter()) {
                edges += 1;
                if edges > max_edges {
                    return Relation::Unknown;
                }
                queue.push_back(Arc::clone(parent));
            }
        }

        // A valid Java hierarchy is acyclic. Kahn's algorithm distinguishes a
        // malformed cycle from harmless repeated paths in a diamond graph.
        let mut indegree: HashMap<Arc<str>, usize> = reachable
            .iter()
            .filter(|node| self.entries.contains_key(node.as_ref()))
            .map(|node| (Arc::clone(node), 0))
            .collect();
        let mut children: HashMap<Arc<str>, Vec<Arc<str>>> = HashMap::new();
        for node in indegree.keys().cloned().collect::<Vec<_>>() {
            let entry = &self.entries[node.as_ref()];
            for parent in entry.superclass.iter().chain(entry.interfaces.iter()) {
                if let Some(degree) = indegree.get_mut(parent.as_ref()) {
                    *degree += 1;
                    children
                        .entry(Arc::clone(&node))
                        .or_default()
                        .push(Arc::clone(parent));
                }
            }
        }
        let mut roots: VecDeque<_> = indegree
            .iter()
            .filter(|(_, degree)| **degree == 0)
            .map(|(node, _)| Arc::clone(node))
            .collect();
        let mut processed = 0usize;
        while let Some(node) = roots.pop_front() {
            processed += 1;
            for parent in children.get(node.as_ref()).into_iter().flatten() {
                let degree = indegree.get_mut(parent.as_ref()).expect("known parent");
                *degree -= 1;
                if *degree == 0 {
                    roots.push_back(Arc::clone(parent));
                }
            }
        }
        if unknown || processed != indegree.len() {
            Relation::Unknown
        } else {
            Relation::Disproven
        }
    }

    #[cfg(test)]
    pub fn cached_relations(&self) -> usize {
        self.cache
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .len
    }

    #[cfg(test)]
    pub fn assignable_with_budget(
        &self,
        source: &str,
        target: &str,
        max_nodes: usize,
        max_edges: usize,
    ) -> Relation {
        self.assignable_uncached(source, target, max_nodes, max_edges)
    }
}

#[derive(serde::Deserialize)]
struct PlatformExceptions {
    sha256: String,
    exception_parents: HashMap<String, Option<String>>,
    types: HashMap<String, (Option<String>, Vec<String>)>,
    methods: HashMap<String, (bool, Vec<String>)>,
}
fn platform_exceptions() -> &'static PlatformExceptions {
    static FACTS: std::sync::OnceLock<PlatformExceptions> = std::sync::OnceLock::new();
    FACTS.get_or_init(|| {
        serde_json::from_str(include_str!("../data/android-35-exceptions.json"))
            .expect("embedded Android exception metadata is valid")
    })
}

#[derive(serde::Deserialize)]
struct PlatformSourceDeclarations {
    schema: u32,
    complete: bool,
    android_jar_sha256: String,
    class_count: usize,
    method_count: usize,
    ambiguous_source_signature_count: usize,
    ambiguous_source_signatures: HashSet<String>,
    classes: HashMap<String, u32>,
    methods: HashMap<String, PlatformSourceMethod>,
}

#[derive(serde::Deserialize)]
struct PlatformSourceMethod {
    access_flags: u32,
    exceptions: Vec<String>,
}

fn platform_source_declarations() -> &'static PlatformSourceDeclarations {
    static FACTS: std::sync::OnceLock<PlatformSourceDeclarations> = std::sync::OnceLock::new();
    FACTS.get_or_init(|| {
        serde_json::from_str(include_str!("../data/android-35-source-declarations.json"))
            .expect("embedded Android source declaration metadata is valid")
    })
}

// Structural leaf authority only; loaded overrides always use loaded facts.
fn platform_interface_leaf(owner: &str) -> bool {
    let facts = platform_source_declarations();
    facts.complete
        && facts.schema == 1
        && facts.android_jar_sha256 == platform_exceptions().sha256
        && facts.class_count == facts.classes.len()
        && facts.method_count == facts.methods.len()
        && facts.ambiguous_source_signature_count == facts.ambiguous_source_signatures.len()
        && facts
            .classes
            .get(owner)
            .is_some_and(|flags| flags & 0x601 == 0x601)
        && matches!(platform_exceptions().types.get(owner),
            Some((Some(parent), interfaces))
                if parent == "Ljava/lang/Object;" && interfaces.is_empty())
}

/// Complete pinned API-35 declaration set for the one unloaded interface used
/// by the bounded ctorless-flat cohort. All direct methods are checked so its
/// inherited defaults cannot hide a conflict. The exceptions hierarchy proves
/// that the interface has no inherited interface obligations.
pub(crate) fn exact_platform_interface_obligations(
    owner: &str,
) -> Option<(usize, &'static [ExactAbstractObligation])> {
    if owner != "Ljava/lang/CharSequence;" {
        return None;
    }
    static OBLIGATIONS: std::sync::OnceLock<Option<Vec<ExactAbstractObligation>>> =
        std::sync::OnceLock::new();
    let facts = platform_source_declarations();
    let obligations = OBLIGATIONS.get_or_init(|| {
        if !facts.complete
            || facts.schema != 1
            || facts.android_jar_sha256 != platform_exceptions().sha256
            || facts.class_count != facts.classes.len()
            || facts.method_count != facts.methods.len()
            || facts.ambiguous_source_signature_count != facts.ambiguous_source_signatures.len()
            || facts.class_count > 100_000
            || facts.method_count > 2_000_000
            || facts.classes.get(owner).copied() != Some(0x601)
            || !matches!(
                platform_exceptions().types.get(owner),
                Some((Some(parent), interfaces))
                    if parent == "Ljava/lang/Object;" && interfaces.is_empty()
            )
        {
            return None;
        }
        let expected = [
            ("charAt", vec!["I"], "C", 0x401),
            ("chars", vec![], "Ljava/util/stream/IntStream;", 1),
            ("codePoints", vec![], "Ljava/util/stream/IntStream;", 1),
            (
                "compare",
                vec!["Ljava/lang/CharSequence;", "Ljava/lang/CharSequence;"],
                "I",
                9,
            ),
            ("isEmpty", vec![], "Z", 1),
            ("length", vec![], "I", 0x401),
            (
                "subSequence",
                vec!["I", "I"],
                "Ljava/lang/CharSequence;",
                0x401,
            ),
            ("toString", vec![], "Ljava/lang/String;", 0x401),
        ];
        let expected_keys: HashSet<String> = expected
            .iter()
            .map(|(name, parameters, ret, _)| {
                format!("{owner}->{name}({}){ret}", parameters.concat())
            })
            .collect();
        let actual_direct: HashSet<String> = facts
            .methods
            .iter()
            .filter(|(key, _)| key.starts_with("Ljava/lang/CharSequence;->"))
            .map(|(key, _)| key.clone())
            .collect();
        if actual_direct != expected_keys {
            return None;
        }
        let mut result = Vec::with_capacity(4);
        for (name, parameters, ret, access_flags) in expected {
            let key = format!("{owner}->{name}({}){ret}", parameters.concat());
            let source_key = format!("{owner}->{name}({})", parameters.concat());
            let method = facts.methods.get(&key)?;
            if method.access_flags != access_flags
                || !method.exceptions.is_empty()
                || facts.ambiguous_source_signatures.contains(&source_key)
            {
                return None;
            }
            if access_flags == 0x401 {
                result.push(ExactAbstractObligation {
                    name: Arc::from(name),
                    parameters: parameters.into_iter().map(Arc::from).collect(),
                    return_type: Arc::from(ret),
                });
            }
        }
        Some(result)
    });
    Some((facts.method_count, obligations.as_deref()?))
}

fn valid_visibility(flags: u32) -> bool {
    matches!(flags & 7, 0 | 1 | 2 | 4)
}

fn platform_source_accessible(
    caller: &str,
    owner: &str,
    owner_flags: u32,
    member_flags: u32,
) -> bool {
    let Some(caller_package) = descriptor_package(caller) else {
        return false;
    };
    let Some(owner_package) = descriptor_package(owner) else {
        return false;
    };
    let same_package = caller_package == owner_package;
    if owner_flags & 1 == 0 && !same_package {
        return false;
    }
    let visibility = member_flags & 7;
    visibility == 1 || visibility == 0 && same_package
}

#[cfg(test)]
pub(crate) fn platform_source_accessible_for_test(
    caller: &str,
    owner: &str,
    owner_flags: u32,
    member_flags: u32,
) -> bool {
    platform_source_accessible(caller, owner, owner_flags, member_flags)
}

fn valid_reference_descriptor(descriptor: &str) -> bool {
    descriptor.starts_with('L')
        && descriptor.ends_with(';')
        && descriptor.len() > 2
        && !descriptor[1..descriptor.len() - 1]
            .bytes()
            .any(|byte| matches!(byte, b'.' | b';' | b'[' | 0))
}

fn descriptor_package(descriptor: &str) -> Option<&str> {
    if !valid_reference_descriptor(descriptor) {
        return None;
    }
    let name = &descriptor[1..descriptor.len() - 1];
    Some(name.rsplit_once('/').map_or("", |(package, _)| package))
}

fn unloaded_platform_superclass(owner: &str) -> Option<&'static str> {
    platform_exceptions()
        .types
        .get(owner)
        .and_then(|(parent, _)| parent.as_deref())
}

/// Exact Android API-35 DocumentsProvider instance Throws fact.
pub(crate) fn pinned_document_exception_contract(
    method: &crate::native_dex::DexMethod,
    exception: &str,
) -> bool {
    let key = format!(
        "Landroid/provider/DocumentsProvider;->{}({}){}",
        method.name,
        method
            .parameters
            .iter()
            .map(AsRef::as_ref)
            .collect::<String>(),
        method.return_type,
    );
    platform_exceptions()
        .methods
        .get(&key)
        .is_some_and(|(is_static, types)| !is_static && types.iter().any(|ty| ty == exception))
}

fn platform_entries() -> HashMap<Arc<str>, TypeEntry> {
    let mut entries = HashMap::new();
    for (name, parent) in &platform_exceptions().exception_parents {
        entries.insert(
            Arc::from(name.as_str()),
            TypeEntry {
                superclass: parent.as_deref().map(Arc::from),
                interfaces: Arc::from([]),
            },
        );
    }
    entries.insert(
        Arc::from(OBJECT),
        TypeEntry {
            superclass: None,
            interfaces: Arc::from([]),
        },
    );
    // Stable Java collection interface edges required for constructor arguments.
    for (child, parents) in [
        ("Ljava/lang/Iterable;", vec![]),
        (
            "Ljava/util/Collection;",
            vec![Arc::from("Ljava/lang/Iterable;")],
        ),
        (
            "Ljava/util/List;",
            vec![Arc::from("Ljava/util/Collection;")],
        ),
    ] {
        entries.insert(
            Arc::from(child),
            TypeEntry {
                superclass: Some(Arc::from(OBJECT)),
                interfaces: parents.into(),
            },
        );
    }
    for (child, parent) in [
        ("Ljava/lang/Throwable;", OBJECT),
        ("Ljava/lang/Exception;", "Ljava/lang/Throwable;"),
        ("Ljava/lang/RuntimeException;", "Ljava/lang/Exception;"),
        ("Ljava/lang/Error;", "Ljava/lang/Throwable;"),
        ("Ljava/io/IOException;", "Ljava/lang/Exception;"),
        ("Ljava/io/FileNotFoundException;", "Ljava/io/IOException;"),
        ("Ljava/io/EOFException;", "Ljava/io/IOException;"),
        ("Ljava/io/InterruptedIOException;", "Ljava/io/IOException;"),
        (
            "Ljava/io/UnsupportedEncodingException;",
            "Ljava/io/IOException;",
        ),
        ("Ljava/io/UTFDataFormatException;", "Ljava/io/IOException;"),
        ("Ljava/net/SocketException;", "Ljava/io/IOException;"),
        (
            "Ljava/net/SocketTimeoutException;",
            "Ljava/io/InterruptedIOException;",
        ),
        ("Ljava/net/UnknownHostException;", "Ljava/io/IOException;"),
        ("Ljava/net/MalformedURLException;", "Ljava/io/IOException;"),
        (
            "Ljava/lang/ReflectiveOperationException;",
            "Ljava/lang/Exception;",
        ),
        (
            "Ljava/lang/ClassNotFoundException;",
            "Ljava/lang/ReflectiveOperationException;",
        ),
        (
            "Ljava/lang/NoSuchMethodException;",
            "Ljava/lang/ReflectiveOperationException;",
        ),
        (
            "Ljava/lang/NoSuchFieldException;",
            "Ljava/lang/ReflectiveOperationException;",
        ),
        (
            "Ljava/lang/IllegalAccessException;",
            "Ljava/lang/ReflectiveOperationException;",
        ),
        (
            "Ljava/lang/InstantiationException;",
            "Ljava/lang/ReflectiveOperationException;",
        ),
        ("Ljava/lang/InterruptedException;", "Ljava/lang/Exception;"),
        (
            "Ljava/lang/CloneNotSupportedException;",
            "Ljava/lang/Exception;",
        ),
        (
            "Ljava/lang/reflect/InvocationTargetException;",
            "Ljava/lang/ReflectiveOperationException;",
        ),
        ("Ljava/sql/SQLException;", "Ljava/lang/Exception;"),
        ("Ljava/text/ParseException;", "Ljava/lang/Exception;"),
        (
            "Ljava/util/concurrent/ExecutionException;",
            "Ljava/lang/Exception;",
        ),
        (
            "Ljava/util/concurrent/TimeoutException;",
            "Ljava/lang/Exception;",
        ),
        (
            "Ljava/lang/IllegalArgumentException;",
            "Ljava/lang/RuntimeException;",
        ),
        (
            "Ljava/lang/IllegalStateException;",
            "Ljava/lang/RuntimeException;",
        ),
        (
            "Ljava/lang/NullPointerException;",
            "Ljava/lang/RuntimeException;",
        ),
        (
            "Ljava/lang/UnsupportedOperationException;",
            "Ljava/lang/RuntimeException;",
        ),
        (
            "Ljava/lang/IndexOutOfBoundsException;",
            "Ljava/lang/RuntimeException;",
        ),
        (
            "Ljava/lang/ArrayIndexOutOfBoundsException;",
            "Ljava/lang/IndexOutOfBoundsException;",
        ),
        (
            "Ljava/lang/StringIndexOutOfBoundsException;",
            "Ljava/lang/IndexOutOfBoundsException;",
        ),
        (
            "Ljava/lang/ClassCastException;",
            "Ljava/lang/RuntimeException;",
        ),
        (
            "Ljava/lang/ArithmeticException;",
            "Ljava/lang/RuntimeException;",
        ),
        (
            "Ljava/lang/SecurityException;",
            "Ljava/lang/RuntimeException;",
        ),
        (
            "Ljava/lang/NumberFormatException;",
            "Ljava/lang/IllegalArgumentException;",
        ),
        (
            "Ljava/lang/NegativeArraySizeException;",
            "Ljava/lang/RuntimeException;",
        ),
        (
            "Ljava/lang/ArrayStoreException;",
            "Ljava/lang/RuntimeException;",
        ),
        (
            "Ljava/util/ConcurrentModificationException;",
            "Ljava/lang/RuntimeException;",
        ),
        (
            "Ljava/util/NoSuchElementException;",
            "Ljava/lang/RuntimeException;",
        ),
        ("Ljava/lang/AssertionError;", "Ljava/lang/Error;"),
        ("Ljava/lang/LinkageError;", "Ljava/lang/Error;"),
        (
            "Ljava/lang/ExceptionInInitializerError;",
            "Ljava/lang/LinkageError;",
        ),
        (
            "Ljava/lang/NoClassDefFoundError;",
            "Ljava/lang/LinkageError;",
        ),
        ("Ljava/lang/VirtualMachineError;", "Ljava/lang/Error;"),
        (
            "Ljava/lang/OutOfMemoryError;",
            "Ljava/lang/VirtualMachineError;",
        ),
        (
            "Ljava/lang/StackOverflowError;",
            "Ljava/lang/VirtualMachineError;",
        ),
    ] {
        entries.insert(
            Arc::from(child),
            TypeEntry {
                superclass: Some(Arc::from(parent)),
                interfaces: Arc::from([]),
            },
        );
    }
    // Keep guaranteed platform interfaces: omitting them produces false
    // negative assignability answers once the graph is used beyond exceptions.
    for interface in ["Ljava/io/Serializable;", "Ljava/lang/Iterable;"] {
        entries.insert(
            Arc::from(interface),
            TypeEntry {
                superclass: Some(Arc::from(OBJECT)),
                interfaces: Arc::from([]),
            },
        );
    }
    entries.get_mut("Ljava/lang/Throwable;").unwrap().interfaces =
        Arc::from([Arc::from("Ljava/io/Serializable;")]);
    entries
        .get_mut("Ljava/sql/SQLException;")
        .unwrap()
        .interfaces = Arc::from([Arc::from("Ljava/lang/Iterable;")]);
    // Pinned SDK declarations provide complete parent/interface edges. Project
    // definitions replace these entries during hierarchy construction.
    for (name, (parent, interfaces)) in &platform_exceptions().types {
        entries.insert(
            Arc::from(name.as_str()),
            TypeEntry {
                superclass: parent.as_deref().map(Arc::from),
                interfaces: interfaces.iter().map(|s| Arc::from(s.as_str())).collect(),
            },
        );
    }
    entries
}

fn primitive(descriptor: &str) -> bool {
    matches!(descriptor, "Z" | "B" | "S" | "C" | "I" | "J" | "F" | "D")
}
fn valid_class(descriptor: &str) -> bool {
    descriptor
        .strip_prefix('L')
        .and_then(|s| s.strip_suffix(';'))
        .is_some_and(|name| {
            !name.is_empty()
                && !name.contains(['.', ';', '['])
                && name.split('/').all(|part| !part.is_empty())
        })
}
fn valid_array(descriptor: &str) -> bool {
    let dimensions = descriptor
        .bytes()
        .take(256)
        .take_while(|&b| b == b'[')
        .count();
    dimensions > 0
        && dimensions <= 255
        && (primitive(&descriptor[dimensions..]) || valid_class(&descriptor[dimensions..]))
}

// A bounded fresh-allocation-only certificate. Object's constructor is followed
// exclusively by redundant null stores to owned ordinary reference fields.
// Keeping this separate from transparent constructors prevents pre-super field
// writes from being overwritten by a newly introduced constructor body.
fn null_writing_object_constructor(class: &DexClass) -> bool {
    let prove = || -> Option<()> {
        if class.fields.len() > 256
            || class.methods.len() > 4096
            || class.superclass.as_deref() != Some("Ljava/lang/Object;")
            || class.access_flags & 1 == 0
            || class
                .fields
                .iter()
                .any(|field| !field.is_static && field.access_flags & (0x10 | 0x40) != 0)
        {
            return None;
        }
        let mut candidates = class
            .methods
            .iter()
            .filter(|method| method.name.as_ref() == "<init>" && method.parameters.is_empty());
        let method = candidates.next()?;
        if candidates.next().is_some()
            || method.declaring_type != class.descriptor
            || method.return_type.as_ref() != "V"
            || !method.thrown_types.is_empty()
            || method.access_flags & 1 == 0
            || method.access_flags & 0x10000 == 0
            || method.access_flags & !0x11001 != 0
        {
            return None;
        }
        let code = method.code.as_ref()?;
        let words = &code.instructions;
        if code.registers != 2
            || code.ins != 1
            || code.outs != 1
            || code.tries != 0
            || !code.try_regions.is_empty()
            || !(7..=133).contains(&words.len())
            || words.len() % 2 == 0
            || words[0] != 0x1070
            || words[2] != 1
            || words[3] != 0x0012
            || words.last() != Some(&0x000e)
        {
            return None;
        }
        let &(owner, proto, name) = class.symbols.methods.get(words[1] as usize)?;
        let (ret, args) = class.symbols.protos.get(proto as usize)?;
        if class.symbols.types.get(owner as usize)?.as_ref() != "Ljava/lang/Object;"
            || class.symbols.strings.get(name as usize)? != "<init>"
            || ret.as_ref() != "V"
            || !args.is_empty()
        {
            return None;
        }
        let mut written = HashSet::new();
        for store in words[4..words.len() - 1].as_chunks::<2>().0 {
            if store[0] != 0x105b {
                return None;
            }
            let &(owner, ty, name) = class.symbols.fields.get(store[1] as usize)?;
            let owner = class.symbols.types.get(owner as usize)?;
            let ty = class.symbols.types.get(ty as usize)?;
            let name = class.symbols.strings.get(name as usize)?;
            if owner != &class.descriptor
                || !(valid_class(ty) || valid_array(ty))
                || !written.insert(name)
            {
                return None;
            }
            let mut fields = class
                .fields
                .iter()
                .filter(|field| field.name.as_ref() == name);
            let field = fields.next()?;
            if fields.next().is_some()
                || field.declaring_type != class.descriptor
                || field.field_type != *ty
                || field.is_static
                || field.access_flags & (0x8 | 0x10 | 0x40) != 0
            {
                return None;
            }
        }
        Some(())
    };
    prove().is_some()
}

// Deliberately exact: no fields, calls, branches, handlers, or arguments may be
// silently added by replacing the optimized owner with the allocated class.
fn trivial_noarg_constructor(class: &DexClass) -> Option<Arc<str>> {
    let mut constructors = class
        .methods
        .iter()
        .filter(|method| method.name.as_ref() == "<init>" && method.parameters.is_empty());
    let method = constructors.next()?;
    if constructors.next().is_some()
        || method.return_type.as_ref() != "V"
        || method.access_flags & 0x8 != 0
    {
        return None;
    }
    let code = method.code.as_ref()?;
    if code.registers != 1 || code.ins != 1 || code.tries != 0 || !code.try_regions.is_empty() {
        return None;
    }
    let words = &code.instructions;
    if words.len() != 4 || words[0] != 0x1070 || words[2] != 0 || words[3] != 0x000e {
        return None;
    }
    let &(owner, proto, name) = class.symbols.methods.get(words[1] as usize)?;
    let (ret, args) = class.symbols.protos.get(proto as usize)?;
    if ret.as_ref() != "V"
        || !args.is_empty()
        || class.symbols.strings.get(name as usize)?.as_str() != "<init>"
    {
        return None;
    }
    class.symbols.types.get(owner as usize).cloned()
}

// A class with no declared constructors receives Java's implicit no-arg
// constructor. Reconstruct that only when it calls the exact DEX owner with no
// hidden enclosing-instance argument and without inventing access or throws.
fn verified_implicit_constructors(classes: &[&DexClass]) -> HashMap<Arc<str>, Arc<str>> {
    let mut owners = HashMap::new();
    let mut duplicates = HashSet::new();
    let mut noarg = HashMap::new();
    for &class in classes {
        if owners.insert(class.descriptor.as_ref(), class).is_some() {
            duplicates.insert(class.descriptor.as_ref());
        }
        let mut methods = class
            .methods
            .iter()
            .filter(|method| method.name.as_ref() == "<init>" && method.parameters.is_empty());
        let first = methods.next();
        noarg.insert(
            class.descriptor.as_ref(),
            if methods.next().is_none() {
                first
            } else {
                None
            },
        );
    }
    let package = |descriptor: &str| {
        descriptor
            .rsplit_once('/')
            .map_or("", |(package, _)| package)
            .to_string()
    };
    let mut result = HashMap::new();
    let mut dependencies: HashMap<Arc<str>, Vec<Arc<str>>> = HashMap::new();
    for &class in classes {
        if duplicates.contains(class.descriptor.as_ref())
            || class.access_flags & (0x200 | 0x4000) != 0
            || !matches!(class.access_flags & 7, 0 | 1)
            || !constructor_recovery::independent_constructor_owner(class)
            || class
                .fields
                .iter()
                .any(|field| !field.is_static && field.access_flags & 0x10 != 0)
            || class
                .methods
                .iter()
                .any(|method| method.name.as_ref() == "<init>")
        {
            continue;
        }
        let Some(parent_type) = &class.superclass else {
            continue;
        };
        if duplicates.contains(parent_type.as_ref()) {
            continue;
        }
        // java.lang.Object has an accessible, no-arg constructor even when
        // platform classes are not bundled in the APK.
        if parent_type.as_ref() == "Ljava/lang/Object;"
            && !owners.contains_key(parent_type.as_ref())
        {
            result.insert(Arc::clone(&class.descriptor), Arc::clone(parent_type));
            continue;
        }
        let Some(parent) = owners.get(parent_type.as_ref()) else {
            if constructor_recovery::accessible_platform_noarg_constructor(
                parent_type,
                &owners,
                &duplicates,
            ) {
                result.insert(Arc::clone(&class.descriptor), Arc::clone(parent_type));
            }
            continue;
        };
        if parent.access_flags & 0x200 != 0 {
            continue;
        }
        let same_package = package(&class.descriptor) == package(parent_type);
        if parent.access_flags & 1 == 0 && !same_package {
            continue;
        }
        let Some(constructor) = noarg.get(parent_type.as_ref()).copied().flatten() else {
            if !parent
                .methods
                .iter()
                .any(|method| method.name.as_ref() == "<init>")
            {
                dependencies
                    .entry(parent_type.clone())
                    .or_default()
                    .push(class.descriptor.clone());
            }
            continue;
        };
        if constructor.declaring_type != *parent_type
            || constructor.return_type.as_ref() != "V"
            || constructor.access_flags & (0x8 | 0x100 | 0x400) != 0
            || constructor.code.is_none()
            || !constructor.thrown_types.is_empty()
            || !match constructor.access_flags & 7 {
                1 | 4 => true,
                0 => same_package,
                _ => false,
            }
        {
            continue;
        }
        result.insert(Arc::clone(&class.descriptor), Arc::clone(parent_type));
    }
    let mut queue: VecDeque<Arc<str>> = result.keys().cloned().collect();
    while let Some(parent) = queue.pop_front() {
        for child in dependencies.remove(&parent).unwrap_or_default() {
            if result.insert(child.clone(), parent.clone()).is_none() {
                queue.push_back(child);
            }
        }
    }
    result
}

// Retain only proven candidates, not a second project-wide method inventory.
fn verified_object_calls(classes: &[&DexClass], varargs_only: bool) -> HashSet<String> {
    let mut owners = HashMap::new();
    let mut duplicates = HashSet::new();
    for &class in classes {
        if owners.insert(class.descriptor.as_ref(), class).is_some() {
            duplicates.insert(class.descriptor.as_ref());
        }
    }
    let mut ordered = classes.to_vec();
    ordered.sort_by(|left, right| left.descriptor.cmp(&right.descriptor));
    let mut result = HashSet::new();
    let mut retained_bytes = 0usize;
    let mut work = 0usize;
    for class in ordered {
        if duplicates.contains(class.descriptor.as_ref()) {
            continue;
        }
        for method in &class.methods {
            let candidate = if varargs_only {
                method.access_flags & (0x80 | 0x8) == (0x80 | 0x8)
                    && method.parameters.last().map(AsRef::as_ref) == Some("[Ljava/lang/Object;")
            } else {
                method.access_flags & (0x80 | 0x8) == 0x8
                    && method.parameters.iter().any(|ty| ty.as_ref() == OBJECT)
            };
            if !candidate || method.name.starts_with('<') {
                continue;
            }
            let mut queue = VecDeque::from([class.descriptor.as_ref()]);
            let mut seen = HashSet::new();
            let mut declarations = 0usize;
            let mut proven = true;
            while let Some(owner) = queue.pop_front() {
                if !seen.insert(owner) {
                    continue;
                }
                // Object's guaranteed method set cannot supply these names.
                if owner == OBJECT && !owners.contains_key(owner) {
                    if matches!(
                        method.name.as_ref(),
                        "clone"
                            | "equals"
                            | "finalize"
                            | "getClass"
                            | "hashCode"
                            | "notify"
                            | "notifyAll"
                            | "toString"
                            | "wait"
                    ) {
                        proven = false;
                    }
                    continue;
                }
                let Some(parent) = owners.get(owner) else {
                    proven = false;
                    break;
                };
                if duplicates.contains(owner) {
                    proven = false;
                    break;
                }
                work = work.saturating_add(parent.methods.len() + 1);
                if work > 20_000_000 {
                    return result;
                }
                declarations += parent
                    .methods
                    .iter()
                    .filter(|m| m.name == method.name)
                    .count();
                if declarations != 1 {
                    proven = false;
                    break;
                }
                queue.extend(
                    parent
                        .superclass
                        .iter()
                        .chain(parent.interfaces.iter())
                        .map(AsRef::as_ref),
                );
            }
            if proven && declarations == 1 {
                let Some(owner) = class
                    .descriptor
                    .strip_prefix('L')
                    .and_then(|s| s.strip_suffix(';'))
                else {
                    continue;
                };
                let signature = format!(
                    "{}.{}({}){}",
                    owner.replace('/', "."),
                    method.name,
                    method.parameters.join(""),
                    method.return_type
                );
                retained_bytes = retained_bytes.saturating_add(signature.len());
                if retained_bytes > 16 * 1024 * 1024 || result.len() >= 65_536 {
                    return result;
                }
                result.insert(signature);
            }
        }
    }
    result
}

// A throw-expression bridge cannot repair a declaration which conflicts with
// an exact public/protected member of a directly named, unloaded SDK parent.
// Unknown parents and loaded SDK shadows retain their own declaration route.
pub(crate) fn valid_pinned_override_declaration(
    class: &DexClass,
    method: &crate::native_dex::DexMethod,
    hierarchy: Option<&TypeHierarchy>,
) -> bool {
    let Some(parent) = class.superclass.as_deref() else {
        return true;
    };
    if !matches!(
        parent,
        "Landroid/app/backup/BackupAgent;"
            | "Landroid/app/backup/BackupAgentHelper;"
            | "Landroid/content/ContentProvider;"
            | "Landroid/provider/DocumentsProvider;"
    ) || method.name.starts_with('<')
        || hierarchy.is_some_and(|h| h.loaded_owners.contains(parent))
    {
        return true;
    }
    type Members = HashMap<String, Option<(String, u32)>>;
    static MEMBERS: std::sync::OnceLock<Option<Members>> = std::sync::OnceLock::new();
    let members = MEMBERS.get_or_init(|| {
        let facts = platform_source_declarations();
        if !facts.complete
            || facts.schema != 1
            || facts.android_jar_sha256 != platform_exceptions().sha256
            || facts.class_count != facts.classes.len()
            || facts.method_count != facts.methods.len()
            || facts.ambiguous_source_signature_count != facts.ambiguous_source_signatures.len()
        {
            return None;
        }
        let mut members = Members::new();
        for (key, member) in &facts.methods {
            let (owner, _) = key.split_once("->")?;
            if !matches!(
                owner,
                "Landroid/app/backup/BackupAgent;"
                    | "Landroid/app/backup/BackupAgentHelper;"
                    | "Landroid/content/ContentProvider;"
                    | "Landroid/provider/DocumentsProvider;"
            ) || member.access_flags & 5 == 0
            {
                continue;
            }
            let end = key.find(')')? + 1;
            let signature = &key[..end];
            let entry = if facts.ambiguous_source_signatures.contains(signature) {
                None
            } else {
                Some((key[end..].to_owned(), member.access_flags))
            };
            if members.contains_key(signature) {
                members.insert(signature.to_owned(), None);
            } else {
                members.insert(signature.to_owned(), entry);
            }
            if members.len() > 2048 {
                return None;
            }
        }
        Some(members)
    });
    let signature = format!(
        "{parent}->{}({})",
        method.name,
        method
            .parameters
            .iter()
            .map(AsRef::as_ref)
            .collect::<String>()
    );
    let Some(Some((returns, flags))) = members.as_ref().and_then(|members| members.get(&signature))
    else {
        return true;
    };
    let visibility = if flags & 1 != 0 {
        method.access_flags & 7 == 1
    } else {
        matches!(method.access_flags & 7, 1 | 4)
    };
    visibility
        && flags & 0x10 == 0
        && (flags & 8 == method.access_flags & 8)
        && (method.return_type.as_ref() == returns
            || ((returns.starts_with('L') || returns.starts_with('['))
                && (method.return_type.starts_with('L') || method.return_type.starts_with('['))
                && hierarchy.is_some_and(|h| {
                    h.assignable(&method.return_type, returns) == Relation::Proven
                })))
}

pub(crate) fn source_inherited_exceptions(
    class: &DexClass,
    method: &crate::native_dex::DexMethod,
    hierarchy: Option<&TypeHierarchy>,
) -> Vec<&'static str> {
    let mut result: Vec<_> = source_inherited_exception(class, method, hierarchy)
        .into_iter()
        .collect();
    if let Some(hierarchy) = hierarchy {
        result.extend(hierarchy.source_future_header(class, method));
    }
    result
}

pub(crate) fn source_inherited_exception(
    class: &DexClass,
    method: &crate::native_dex::DexMethod,
    hierarchy: Option<&TypeHierarchy>,
) -> Option<&'static str> {
    if method.thrown_types.is_empty()
        && method.declaring_type == class.descriptor
        && method.access_flags & 7 == 1
        && method.access_flags & 8 == 0
        && hierarchy.is_some_and(|hierarchy| hierarchy.inherited_io_exception(class, method))
    {
        return Some("Ljava/io/IOException;");
    }
    if method.thrown_types.is_empty()
        && method.declaring_type == class.descriptor
        && method.access_flags & 7 == 1
        && method.access_flags & 8 == 0
        && class.superclass.as_deref().is_some_and(|parent| {
            hierarchy.map_or_else(
                || {
                    parent == "Landroid/provider/DocumentsProvider;"
                        && crate::native_hierarchy::pinned_document_exception_contract(
                            method,
                            "Ljava/io/FileNotFoundException;",
                        )
                },
                |hierarchy| {
                    hierarchy.inherited_document_exception_contract(
                        parent,
                        method,
                        "Ljava/io/FileNotFoundException;",
                    )
                },
            )
        })
    {
        return Some("Ljava/io/FileNotFoundException;");
    }
    if method.thrown_types.is_empty()
        && method.declaring_type == class.descriptor
        && method.access_flags & 15 == 1
        && method.name.as_ref() == "openFile"
        && method.return_type.as_ref() == "Landroid/os/ParcelFileDescriptor;"
        && method
            .parameters
            .iter()
            .map(AsRef::as_ref)
            .eq(["Landroid/net/Uri;", "Ljava/lang/String;"])
        && class.superclass.as_deref().is_some_and(|parent| {
            hierarchy.map_or_else(
                || parent == "Landroid/content/ContentProvider;",
                |hierarchy| {
                    hierarchy.inherited_file_exception_contract(
                        parent,
                        method,
                        "Ljava/io/FileNotFoundException;",
                    )
                },
            )
        })
    {
        return Some("Ljava/io/FileNotFoundException;");
    }
    if !method.thrown_types.is_empty()
        || method.declaring_type != class.descriptor
        || method.access_flags & 7 != 1
        || method.access_flags & 8 != 0
        || method.return_type.as_ref() != "V"
        || !matches!(
            class.superclass.as_deref(),
            Some("Landroid/app/backup/BackupAgent;" | "Landroid/app/backup/BackupAgentHelper;")
        )
    {
        return None;
    }
    let expected: &[&str] = match method.name.as_ref() {
        "onRestore" => &[
            "Landroid/app/backup/BackupDataInput;",
            "I",
            "Landroid/os/ParcelFileDescriptor;",
        ],
        "onBackup" => &[
            "Landroid/os/ParcelFileDescriptor;",
            "Landroid/app/backup/BackupDataOutput;",
            "Landroid/os/ParcelFileDescriptor;",
        ],
        _ => return None,
    };
    (method.parameters.len() == expected.len()
        && method
            .parameters
            .iter()
            .zip(expected)
            .all(|(actual, expected)| actual.as_ref() == *expected))
    .then_some("Ljava/io/IOException;")
}

#[cfg(test)]
mod checked_caller_budget_tests {
    use super::*;

    fn sneaky_dependency(
        instructions: Vec<u16>,
        registers: u16,
        ins: u16,
        parameters: Vec<Arc<str>>,
    ) -> DexClass {
        let owner: Arc<str> = "Lsample/Dependency;".into();
        DexClass {
            descriptor: owner.clone(),
            superclass: Some("Ljava/lang/Object;".into()),
            interfaces: vec![],
            access_flags: 0x11,
            annotations_offset: 0,
            static_values_offset: 0,
            static_values: vec![],
            fields: vec![],
            methods: vec![crate::native_dex::DexMethod {
                declaring_type: owner.clone(),
                name: "probe".into(),
                parameters,
                return_type: "V".into(),
                thrown_types: vec![],
                access_flags: 9,
                code: Some(crate::native_dex::DexCode {
                    registers,
                    ins,
                    outs: 1,
                    tries: 0,
                    try_regions: vec![],
                    instructions,
                    offset: 73,
                }),
            }],
            symbols: Arc::new(crate::native_dex::DexSymbols {
                strings: vec!["probe".into(), "<init>".into(), "requireNonNull".into()],
                types: vec![
                    owner,
                    "Ljava/lang/NullPointerException;".into(),
                    "Ljava/lang/Object;".into(),
                    "Ljava/util/Objects;".into(),
                    "Z".into(),
                ],
                protos: vec![
                    ("V".into(), vec![]),
                    (
                        "Ljava/lang/Object;".into(),
                        vec!["Ljava/lang/Object;".into()],
                    ),
                ],
                methods: vec![(1, 0, 1), (3, 1, 2)],
                ..Default::default()
            }),
        }
    }

    fn sneaky_dependency_summary(class: &DexClass) -> Option<SneakyHeaderSummary> {
        let hierarchy = TypeHierarchy::from_classes([class]).unwrap();
        prepare_sneaky_no_checked_header(class, &class.methods[0], &hierarchy, 100_000).unwrap()
    }

    #[test]
    fn no_trigger_walk_uses_original_widths_and_fails_closed() {
        for literal in [0x27, 0x71, 0x77] {
            assert!(complete_no_inference_trigger(&[0x0013, literal, 0x000e]));
            assert!(complete_no_inference_trigger(&[
                0x0018, literal, 0, 0, 0, 0x000e
            ]));
        }
        for words in [
            vec![],
            vec![0x0013],
            vec![0x003e],
            vec![0x0100, 0, 0, 0],
            vec![0x0027],
            vec![0x0071, 0, 0],
            vec![0x0077, 0, 0],
            vec![0x000e; 4097],
        ] {
            assert!(!complete_no_inference_trigger(&words), "{words:?}");
        }
    }

    #[test]
    fn inferred_caller_cycle_and_deep_chain_fail_closed() {
        let hierarchy = TypeHierarchy::from_classes(std::iter::empty::<&DexClass>()).unwrap();
        let io = "Ljava/io/IOException;";
        let mut index = CheckedCallerIndex {
            ready: true,
            complete: true,
            ..Default::default()
        };
        for step in 0..=64 {
            let key = format!("method{step}");
            index.candidates.insert(key.clone());
            if step < 64 {
                index.conditional.insert(
                    key,
                    vec![CheckedCallerSite {
                        types: vec![],
                        inherited_io_caller: Some(format!("method{}", step + 1)),
                        future_caller: None,
                    }],
                );
            }
        }
        assert!(!index.permits("method0", io, &hierarchy));

        index.conditional.insert(
            "method1".into(),
            vec![CheckedCallerSite {
                types: vec![],
                inherited_io_caller: Some("method0".into()),
                future_caller: None,
            }],
        );
        assert!(!index.permits("method0", io, &hierarchy));
    }

    #[test]
    fn sneaky_header_work_and_dependency_closure_are_bounded() {
        let mut reserved = 0;
        assert!(reserve_sneaky_analysis_work(&mut reserved, 10, 60));
        assert_eq!(reserved, 30);
        assert!(reserve_sneaky_analysis_work(&mut reserved, 10, 60));
        assert!(!reserve_sneaky_analysis_work(&mut reserved, 1, 60));

        let mut chain = BTreeMap::new();
        for step in 0..64 {
            chain.insert(
                format!("m{step:02}"),
                SneakyHeaderSummary {
                    dependencies: if step != 63 {
                        vec![format!("m{:02}", step + 1)]
                    } else {
                        Vec::new()
                    },
                },
            );
        }
        assert_eq!(close_sneaky_header_summaries(&chain).len(), 64);
        chain.get_mut("m63").unwrap().dependencies = vec!["m00".into()];
        assert!(close_sneaky_header_summaries(&chain).is_empty());
    }

    #[test]
    fn sneaky_allocation_requires_one_dominating_initializer_before_all_reads() {
        let initialized_then_sanitized = sneaky_dependency(
            vec![0x0022, 1, 0x1070, 0, 0, 0x1071, 1, 0, 0x0027],
            1,
            0,
            vec![],
        );
        assert!(sneaky_dependency_summary(&initialized_then_sanitized).is_some());

        let uninitialized_throw = sneaky_dependency(vec![0x0022, 1, 0x0027], 1, 0, vec![]);
        assert!(sneaky_dependency_summary(&uninitialized_throw).is_none());

        let branch_bypasses_initializer = sneaky_dependency(
            vec![0x0022, 1, 0x0138, 5, 0x1070, 0, 0, 0x0027],
            2,
            1,
            vec!["Z".into()],
        );
        assert!(sneaky_dependency_summary(&branch_bypasses_initializer).is_none());

        let duplicate_initializer = sneaky_dependency(
            vec![0x0022, 1, 0x1070, 0, 0, 0x1070, 0, 0, 0x0027],
            1,
            0,
            vec![],
        );
        assert!(sneaky_dependency_summary(&duplicate_initializer).is_none());

        let sanitizer_before_initializer = sneaky_dependency(
            vec![0x0022, 1, 0x1071, 1, 0, 0x1070, 0, 0, 0x0027],
            1,
            0,
            vec![],
        );
        assert!(sneaky_dependency_summary(&sanitizer_before_initializer).is_none());
    }

    #[test]
    fn platform_source_invocation_category_is_exact() {
        let hierarchy = TypeHierarchy::from_classes(std::iter::empty::<&DexClass>()).unwrap();
        let no_args = Vec::<Arc<str>>::new();
        assert!(
            hierarchy
                .exact_platform_source_declaration_for_invoke(
                    "Lsample/Caller;",
                    "Ljava/lang/NullPointerException;",
                    "<init>",
                    &no_args,
                    "V",
                    PlatformInvokeKind::Direct,
                )
                .is_some()
        );
        assert!(
            hierarchy
                .exact_platform_source_declaration_for_invoke(
                    "Lsample/Caller;",
                    "Ljava/lang/NullPointerException;",
                    "<init>",
                    &no_args,
                    "V",
                    PlatformInvokeKind::Virtual,
                )
                .is_none()
        );
        assert!(
            hierarchy
                .exact_platform_source_declaration_for_invoke(
                    "Lsample/Caller;",
                    "Ljava/lang/Object;",
                    "toString",
                    &no_args,
                    "Ljava/lang/String;",
                    PlatformInvokeKind::Virtual,
                )
                .is_some()
        );
        assert!(
            hierarchy
                .exact_platform_source_declaration_for_invoke(
                    "Lsample/Caller;",
                    "Ljava/lang/Object;",
                    "toString",
                    &no_args,
                    "Ljava/lang/String;",
                    PlatformInvokeKind::Interface,
                )
                .is_none()
        );
    }
}

#[cfg(test)]
mod typed_constructor_query_budget_tests {
    use super::*;
    use crate::native_dex::{DexCode, DexField, DexMethod, DexSymbols, DexTryRegion};
    fn fixture(owner: &str, external: &str) -> DexClass {
        DexClass {
            descriptor: owner.into(),
            superclass: Some("Ljava/lang/Object;".into()),
            interfaces: vec![],
            access_flags: 1,
            annotations_offset: 0,
            static_values_offset: 0,
            static_values: vec![],
            fields: vec![DexField {
                declaring_type: owner.into(),
                name: "f".into(),
                field_type: "I".into(),
                access_flags: 17,
                is_static: false,
            }],
            methods: vec![DexMethod {
                declaring_type: owner.into(),
                name: "<init>".into(),
                parameters: vec!["I".into(), external.into()],
                return_type: "V".into(),
                thrown_types: vec![],
                access_flags: 0x10001,
                code: Some(DexCode {
                    registers: 4,
                    ins: 3,
                    outs: 1,
                    tries: 1,
                    try_regions: vec![DexTryRegion {
                        start: 5,
                        end: 7,
                        catches: vec![(Some("Ljava/lang/RuntimeException;".into()), 8)].into(),
                    }],
                    instructions: vec![
                        0x1070, 0, 1, 0x3259, 0, 0x1259, 1, 0x0328, 0x000d, 0x0027, 0x000e,
                    ],
                    offset: 0,
                }),
            }],
            symbols: Arc::new(DexSymbols {
                types: vec![
                    "Ljava/lang/Object;".into(),
                    owner.into(),
                    "I".into(),
                    external.into(),
                ],
                strings: vec!["<init>".into(), "x".into(), "f".into()],
                protos: vec![("V".into(), vec![])],
                methods: vec![(0, 0, 0)],
                fields: vec![(3, 2, 1), (1, 2, 2)],
                ..Default::default()
            }),
        }
    }
    fn existing() -> WritableFieldQueries {
        HashMap::from([(
            Arc::from("Lsample/Legacy;"),
            HashSet::from([Arc::from("x")]),
        )])
    }
    #[test]
    fn added_word_budget_is_transactional_in_both_orders() {
        let first = fixture("Lsample/TypedA;", "Lsample/AddedA;");
        let second = fixture("Lsample/TypedB;", "Lsample/AddedB;");
        for reversed in [false, true] {
            for exhausted in [false, true] {
                let classes = if reversed {
                    vec![&second, &first]
                } else {
                    vec![&first, &second]
                };
                let mut queries = existing();
                let old = queries.clone();
                let mut words = if exhausted { MAX_QUERY_NODES - 11 } else { 0 };
                let old_words = words;
                let mut entries = 1;
                seed_typed_constructor_field_queries(
                    &classes,
                    &mut queries,
                    &mut words,
                    &mut entries,
                );
                if exhausted {
                    assert_eq!(queries, old);
                    assert_eq!(words, old_words);
                    assert_eq!(entries, 1);
                } else {
                    assert_eq!(queries.len(), 3);
                    assert_eq!(words, 22);
                    assert_eq!(entries, 3);
                }
            }
        }
    }
    #[test]
    fn added_entry_budget_retains_every_existing_query_in_both_orders() {
        let first = fixture("Lsample/TypedA;", "Lsample/AddedA;");
        let second = fixture("Lsample/TypedB;", "Lsample/AddedB;");
        for reversed in [false, true] {
            let mut queries: WritableFieldQueries = (0..MAX_CACHE_ENTRIES - 1)
                .map(|i| {
                    (
                        Arc::from(format!("Lsample/Legacy{i};")),
                        HashSet::from([Arc::from("x")]),
                    )
                })
                .collect();
            let old = queries.clone();
            let mut words = 0;
            let mut entries = MAX_CACHE_ENTRIES - 1;
            let classes = if reversed {
                vec![&second, &first]
            } else {
                vec![&first, &second]
            };
            seed_typed_constructor_field_queries(&classes, &mut queries, &mut words, &mut entries);
            assert_eq!(queries, old);
            assert_eq!(words, 0);
            assert_eq!(entries, MAX_CACHE_ENTRIES - 1);
        }
    }
    #[test]
    fn malformed_or_unprotected_store_does_not_add_typed_queries() {
        for mode in 0..3 {
            let mut class = fixture("Lsample/Typed;", "Lsample/Added;");
            let code = class.methods[0].code.as_mut().unwrap();
            match mode {
                0 => code.instructions.push(0x0014),
                1 => {
                    code.try_regions[0].start = 3;
                    code.try_regions[0].end = 5;
                }
                2 => code.try_regions[0].catches = vec![(None, 8)].into(),
                _ => unreachable!(),
            }
            let mut queries = existing();
            let old = queries.clone();
            let mut words = 0;
            let mut entries = 1;
            seed_typed_constructor_field_queries(&[&class], &mut queries, &mut words, &mut entries);
            assert_eq!(queries, old);
            assert_eq!(entries, 1);
        }
    }
    #[test]
    fn raw_prefilter_budget_discards_all_additions_in_both_orders() {
        let mut classes: Vec<_> = (0..257)
            .map(|i| {
                let mut class = fixture(&format!("Lsample/Unprotected{i};"), "Lsample/Added;");
                let code = class.methods[0].code.as_mut().unwrap();
                code.try_regions[0].start = 3;
                code.try_regions[0].end = 5;
                code.instructions.resize(4096, 0);
                class
            })
            .collect();
        classes.push(fixture("Lsample/Typed;", "Lsample/Added;"));
        for reversed in [false, true] {
            let mut references: Vec<_> = classes.iter().collect();
            if reversed {
                references.reverse();
            }
            let mut queries = existing();
            let old = queries.clone();
            let mut words = 0;
            let mut entries = 1;
            seed_typed_constructor_field_queries(
                &references,
                &mut queries,
                &mut words,
                &mut entries,
            );
            assert_eq!(queries, old);
            assert_eq!(words, 0);
            assert_eq!(entries, 1);
        }
    }
}
