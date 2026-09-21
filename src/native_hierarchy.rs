//! Immutable, bounded type relationships assembled from all parsed DEX classes.
//!
//! A missing external definition is never guessed from its name. The small
//! platform graph below only records relationships guaranteed by the Java type
//! hierarchy and needed to anchor exception ancestry.

use crate::native_dex::DexClass;
use anyhow::{Result, ensure};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};

const MAX_CLASSES: usize = 1_000_000;
const MAX_EDGES: usize = 4_000_000;
const MAX_QUERY_NODES: usize = 131_072;
const MAX_QUERY_EDGES: usize = 524_288;
const MAX_CACHE_ENTRIES: usize = 8_192;
const MAX_CACHE_KEY_BYTES: usize = 512;
const OBJECT: &str = "Ljava/lang/Object;";

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

/// Read-only project-wide class hierarchy. It is cheap to share behind an
/// `Arc<TypeHierarchy>` from each DEX symbol table.
#[derive(Debug, Clone)]
pub struct TypeHierarchy {
    entries: Arc<HashMap<Arc<str>, TypeEntry>>,
    ambiguous: Arc<HashSet<Arc<str>>>,
    cache: Arc<Mutex<RelationCache>>,
    trivial_constructors: Arc<HashMap<Arc<str>, Arc<str>>>,
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
    /// Optimizers can inline an empty constructor at an allocation site. Retarget
    /// only when the allocated class still declares the exact same no-arg body.
    pub fn equivalent_noarg_constructor(&self, allocated: &str, invoked: &str) -> bool {
        !self.ambiguous.contains(allocated)
            && self
                .trivial_constructors
                .get(allocated)
                .is_some_and(|owner| owner.as_ref() == invoked)
            && self.strict_superclass(allocated, invoked) == Relation::Proven
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

        for class in classes {
            count = count.saturating_add(1);
            ensure!(
                count <= MAX_CLASSES,
                "native class hierarchy exceeds class limit"
            );
            let descriptor = Arc::clone(&class.descriptor);
            let constructor = trivial_noarg_constructor(class);
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
        Ok(Self {
            trivial_constructors: Arc::new(trivial_constructors),
            entries: Arc::new(entries),
            ambiguous: Arc::new(ambiguous),
            cache: Arc::new(Mutex::new(RelationCache::default())),
        })
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

fn platform_entries() -> HashMap<Arc<str>, TypeEntry> {
    let mut entries = HashMap::new();
    entries.insert(
        Arc::from(OBJECT),
        TypeEntry {
            superclass: None,
            interfaces: Arc::from([]),
        },
    );
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
