//! Immutable, bounded type relationships assembled from all parsed DEX classes.
//!
//! A missing external definition is never guessed from its name. The small
//! platform graph below only records relationships guaranteed by the Java type
//! hierarchy and needed to anchor exception ancestry.

#[path = "native_constructor_recovery.rs"]
mod constructor_recovery;
use crate::native_dex::DexClass;
use anyhow::{Result, ensure};
pub use constructor_recovery::RecoveredConstructor;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};

const MAX_CLASSES: usize = 1_000_000;
const MAX_EDGES: usize = 4_000_000;
const MAX_QUERY_NODES: usize = 131_072;
const MAX_QUERY_EDGES: usize = 524_288;
const MAX_CACHE_ENTRIES: usize = 8_192;
const MAX_CACHE_KEY_BYTES: usize = 512;
const MAX_READABILITY_FAMILIES: usize = 256_000;
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
    let members = match owner {
        "Ljava/io/DataInput;" => DATA_INPUT,
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
    const MAX_CANDIDATES: usize = 65_536;
    const MAX_TABLES: usize = 256;
    const MAX_SYMBOLS: usize = 2_000_000;
    const MAX_WORDS: usize = 64_000_000;
    const MAX_DECODED: usize = 100_000;
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
            let pinned_io = [
                "Ljava/io/DataInput;",
                "Ljava/io/InputStream;",
                "Ljava/io/FilterInputStream;",
                "Ljava/io/Closeable;",
            ]
            .iter()
            .any(|owner| {
                matches!(
                    pinned_io_member(owner, &signature, &method.return_type),
                    Some(PinnedIoMember::Throws(IO))
                ) && hierarchy.assignable(&class.descriptor, owner) == Relation::Proven
            });
            let raw_inference = code
                .instructions
                .iter()
                .any(|word| matches!((*word & 0xff) as u8, 0x27 | 0x71 | 0x77));
            if pinned_io || raw_inference {
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
                            types.extend(region.catches.iter().filter_map(|(ty, _)| ty.clone()));
                        }
                    }
                }
                let inherited_io_caller =
                    if types.is_empty() && hierarchy.inherited_io_exception(class, method) {
                        Some(io_method_key(class, method))
                    } else {
                        None
                    };
                if types.is_empty() && inherited_io_caller.is_none() {
                    index.blocked.insert(callee.to_owned());
                } else {
                    index
                        .conditional
                        .entry(callee.to_owned())
                        .or_default()
                        .push(CheckedCallerSite {
                            types,
                            inherited_io_caller,
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
    loaded_call_families: Arc<HashMap<Arc<str>, LoadedCallOwner>>,
    noninstantiable_owners: Arc<HashSet<Arc<str>>>,
}

#[derive(Debug)]
struct LoadedCallOwner {
    public: bool,
    root_object: bool,
    generic_class: bool,
    varargs_names: HashSet<Arc<str>>,
    // None means at least two declarations share a Java name and arity.
    methods: HashMap<Arc<str>, HashMap<usize, Option<LoadedCallMethod>>>,
}

#[derive(Debug)]
struct LoadedCallMethod {
    parameters: Vec<Arc<str>>,
    return_type: Arc<str>,
    access_flags: u32,
    annotated: bool,
}

fn summarize_loaded_call_owner(class: &DexClass) -> Option<LoadedCallOwner> {
    let directory = class.symbols.annotations.get(&class.annotations_offset);
    if class.annotations_offset != 0 && directory.is_none() {
        return None;
    }
    let mut summary = LoadedCallOwner {
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
        let method = LoadedCallMethod {
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
                entry.insert(None);
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
        let pinned_io_signature = [
            "Ljava/io/DataInput;",
            "Ljava/io/InputStream;",
            "Ljava/io/FilterInputStream;",
            "Ljava/io/Closeable;",
        ]
        .iter()
        .any(|owner| {
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
                continue;
            }
            if !self.loaded_owners.contains(owner.as_ref())
                && let Some(contract) = pinned_io_member(&owner, &signature, &method.return_type)
            {
                pinned_io_seen = true;
                match contract {
                    PinnedIoMember::Absent => continue,
                    PinnedIoMember::Throws(declared)
                        if !static_method
                            && method.access_flags & 7 == 1
                            && self.assignable(exception, declared) == Relation::Proven =>
                    {
                        pinned_io_contract = true;
                        continue;
                    }
                    _ => return false,
                }
            }
            if !self.loaded_owners.contains(owner.as_ref())
                && known_platform_interface_excludes_signature(&owner, &signature)
            {
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
        [
            "Ljava/io/DataInput;",
            "Ljava/io/InputStream;",
            "Ljava/io/FilterInputStream;",
            "Ljava/io/Closeable;",
        ]
        .iter()
        .any(|owner| {
            matches!(
                pinned_io_member(owner, &signature, &method.return_type),
                Some(PinnedIoMember::Throws("Ljava/io/IOException;"))
            ) && self.assignable(&class.descriptor, owner) == Relation::Proven
        }) && self.permits_inferred_checked_throw(class, method, "Ljava/io/IOException;")
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
        if args.is_empty() {
            return self.equivalent_noarg_constructor(allocated, invoked);
        }
        !self.ambiguous.contains(allocated)
            && !self.ambiguous.contains(invoked)
            && self
                .recovered_constructors(allocated)
                .iter()
                .any(|constructor| {
                    constructor.invoked_owner.as_ref() == invoked && constructor.parameters == args
                })
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
                let pinned_io_candidate = [
                    "Ljava/io/DataInput;",
                    "Ljava/io/InputStream;",
                    "Ljava/io/FilterInputStream;",
                    "Ljava/io/Closeable;",
                ]
                .iter()
                .any(|owner| {
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
        let mut recovered_constructors = constructor_recovery::recover(&classes);
        let accessible_noarg_superclasses =
            constructor_recovery::accessible_noarg_superclasses(&classes);
        // Explicit parameter constructors suppress Java's implicit default.
        // Preserve an already-proven accessible default when adding overloads.
        for (owner, constructors) in &mut recovered_constructors {
            if let Some(parent) = implicit_constructors.get(owner) {
                constructors.insert(
                    0,
                    RecoveredConstructor {
                        parent: parent.clone(),
                        invoked_owner: parent.clone(),
                        parameters: vec![],
                        thrown_types: vec![],
                    },
                );
            }
        }
        let mut entries = platform_entries();
        let mut ambiguous = HashSet::new();
        let mut trivial_constructors = HashMap::new();
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
        for class in &classes {
            if duplicate_owners.contains(&class.descriptor) {
                continue;
            }
            let Some(summary) = summarize_loaded_call_owner(class) else {
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
            loaded_call_families: Arc::new(loaded_call_families),
            noninstantiable_owners: Arc::new(noninstantiable_owners),
            trivial_constructors: Arc::new(trivial_constructors),
            recovered_constructors: Arc::new(recovered_constructors),
            accessible_noarg_superclasses: Arc::new(accessible_noarg_superclasses),
            object_varargs: Arc::new(object_varargs),
            object_calls: Arc::new(object_calls),
            entries: Arc::new(entries),
            ambiguous: Arc::new(ambiguous),
            cache: Arc::new(Mutex::new(RelationCache::default())),
        };
        hierarchy.checked_callers = Arc::new(checked_caller_index(&hierarchy, &classes));
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
            || class.descriptor.contains('$')
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

#[cfg(test)]
mod checked_caller_budget_tests {
    use super::*;

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
            }],
        );
        assert!(!index.permits("method0", io, &hierarchy));
    }
}
