//! Project-local identifier postings, built from original DEX without Java rendering.
use crate::{
    native_cfg::instruction_width,
    native_dex::{DexClass, DexSymbols},
    native_engine::disassembly::{method_id, type_name},
};
use anyhow::{Context, Result, ensure};
use regex::{Regex, RegexBuilder};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap},
    sync::{
        Arc, Mutex, TryLockError,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

#[path = "identifier_scan.rs"]
mod scan;

const MAX_INDEX_BYTES: usize = 256 * 1024 * 1024;
const NO_POSITION: u32 = u32::MAX;

#[derive(Debug)]
struct IndexLimit;
impl std::fmt::Display for IndexLimit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("IDENTIFIER_INDEX_LIMIT")
    }
}
impl std::error::Error for IndexLimit {}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum Kind {
    Class,
    Method,
    Field,
}
impl Kind {
    fn name(self) -> &'static str {
        match self {
            Self::Class => "class",
            Self::Method => "method",
            Self::Field => "field",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ContextKind {
    Declaration,
    Instruction,
    Superclass,
    Interface,
    FieldType,
    ReturnType,
    ParameterType,
}
impl ContextKind {
    fn name(self) -> &'static str {
        match self {
            Self::Declaration => "declaration",
            Self::Instruction => "instruction",
            Self::Superclass => "superclass",
            Self::Interface => "interface",
            Self::FieldType => "field_type",
            Self::ReturnType => "return_type",
            Self::ParameterType => "parameter_type",
        }
    }
}
#[derive(Clone, Copy)]
struct Site {
    class: u32,
    member: u32,
    owner: Kind,
    context: ContextKind,
    position: u32,
}
struct Symbol {
    kind: Kind,
    id: Arc<str>,
    name: String,
    sites: Vec<Site>,
}
struct Index {
    classes: Arc<BTreeMap<String, DexClass>>,
    names: Vec<String>,
    symbols: Vec<Symbol>,
    hash: String,
    estimated_bytes: usize,
    code_methods: usize,
    dynamic_sites: usize,
}

/// Clones share one completed index. Opening a project creates a fresh cache.
/// The current request builds outside the interactive engine lock; cancellation
/// drops an incomplete build, and another waiter may retry. Memory overflow
/// remembers streaming mode; other failures aren't cached.
#[derive(Clone, Default)]
pub(crate) struct Cache {
    index: Arc<Mutex<Option<Arc<Index>>>>,
    streaming: Arc<AtomicBool>,
    scan_session: Arc<std::sync::OnceLock<String>>,
}
impl Cache {
    fn get_or_build(
        &self,
        cancel: &AtomicBool,
        build: impl FnOnce() -> Result<Index>,
    ) -> Result<(Arc<Index>, bool)> {
        let mut slot = loop {
            ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
            match self.index.try_lock() {
                Ok(slot) => break slot,
                Err(TryLockError::WouldBlock) => std::thread::sleep(Duration::from_millis(5)),
                Err(TryLockError::Poisoned(_)) => {
                    anyhow::bail!("IDENTIFIER_INDEX_UNAVAILABLE: reopen the project")
                }
            }
        };
        if let Some(index) = slot.as_ref() {
            return Ok((index.clone(), true));
        }
        // Catch worker panics so the cache remains retryable rather than poisoned.
        let index = std::panic::catch_unwind(std::panic::AssertUnwindSafe(build))
            .map_err(|_| anyhow::anyhow!("IDENTIFIER_INDEX_BUILD_FAILED: retry the query"))??;
        ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
        let index = Arc::new(index);
        *slot = Some(index.clone());
        Ok((index, false))
    }
    pub(crate) fn search(
        &self,
        classes: Arc<BTreeMap<String, DexClass>>,
        args: &Value,
        cancel: &AtomicBool,
    ) -> Result<Value> {
        self.search_with_budget(classes, args, cancel, MAX_INDEX_BYTES)
    }
    fn search_with_budget(
        &self,
        classes: Arc<BTreeMap<String, DexClass>>,
        args: &Value,
        cancel: &AtomicBool,
        budget: usize,
    ) -> Result<Value> {
        let query = Query::parse(args)?;
        if args["search_mode"] == "scan"
            || args.get("resume_cursor").is_some()
            || args["projection"].as_str().is_some_and(|p| p != "full")
            || (args["search_mode"] != "index"
                && query.kind == "method"
                && query.occurrence == "reference")
        {
            let session = self.scan_session.get_or_init(|| {
                format!(
                    "{:?}-{:p}",
                    std::time::SystemTime::now(),
                    Arc::as_ptr(&classes)
                )
            });
            return scan::run(classes, args, query, cancel, session);
        }
        let started = Instant::now();
        // A scoped request never waits for or builds unrelated global postings.
        // Once the global build exceeds its budget, remember the execution mode,
        // not a failed result. Every subsequent query remains fully searchable.
        let streaming = !query.package.is_empty() || self.streaming.load(Ordering::Relaxed);
        let built = if streaming {
            None
        } else {
            match self.get_or_build(cancel, || {
                if self.streaming.load(Ordering::Relaxed) {
                    return Err(IndexLimit.into());
                }
                let result = Index::build_with_budget(classes.clone(), cancel, budget);
                if result.as_ref().is_err_and(|e| e.is::<IndexLimit>()) {
                    self.streaming.store(true, Ordering::Relaxed);
                }
                result
            }) {
                Ok(index) => Some(index),
                Err(error) if error.is::<IndexLimit>() => None,
                Err(error) => return Err(error),
            }
        };
        let build_ms = started.elapsed().as_millis();
        let (mut result, cached) = match built {
            Some((index, cached)) => (index.search(&query, cancel)?, cached),
            None => (stream(classes, query, cancel)?, false),
        };
        result["index_cached"] = json!(cached);
        result["index_wait_build_ms"] = json!(build_ms);
        result["elapsed_ms"] = json!(started.elapsed().as_millis());
        Ok(result)
    }
}

struct Query {
    array_components: bool,
    matcher: Regex,
    kind: String,
    occurrence: String,
    package: String,
    offset: usize,
    limit: usize,
}
fn choice<'a>(args: &'a Value, key: &str, default: &'a str, choices: &[&str]) -> Result<&'a str> {
    let value = args.get(key).map_or(Ok(default), |v| {
        v.as_str()
            .with_context(|| format!("{key} must be a string"))
    })?;
    ensure!(choices.contains(&value), "Invalid {key}");
    Ok(value)
}
fn number(args: &Value, key: &str, default: usize) -> Result<usize> {
    args.get(key).map_or(Ok(default), |v| {
        usize::try_from(v.as_u64().with_context(|| format!("Invalid {key}"))?)
            .context("Page value is too large")
    })
}
fn normalized_query(text: &str) -> String {
    if let Some((owner, member)) = text.split_once("->")
        && owner.starts_with('L')
        && owner.ends_with(';')
    {
        format!("{}.{}", type_name(owner), member)
    } else if text.starts_with('L')
        && text.ends_with(';')
        && !text.contains('(')
        && !text.contains(':')
    {
        type_name(text)
    } else {
        text.to_owned()
    }
}
impl Query {
    fn parse(args: &Value) -> Result<Self> {
        choice(args, "search_mode", "auto", &["auto", "index", "scan"])?;
        choice(args, "projection", "full", &["full", "summary", "callers"])?;
        ensure!(
            args.get("resume_cursor")
                .is_none_or(|c| c.as_str().is_some_and(|s| s.len() <= 4096)),
            "Invalid resume_cursor"
        );
        let query = args["query"].as_str().context("Missing query")?;
        ensure!(
            !query.is_empty() && query.len() <= 4096,
            "Query must contain 1..4096 bytes"
        );
        let match_mode = choice(args, "match_mode", "contains", &["exact", "contains"])?;
        ensure!(
            args.get("pagination").is_none() || args["pagination"] == "hits",
            "Identifier mode paginates by hits; omit pagination or use hits"
        );
        ensure!(
            args.get("source_offset").is_none(),
            "source_offset applies to source mode only"
        );
        let sensitive = args.get("case_sensitive").map_or(Ok(true), |v| {
            v.as_bool().context("case_sensitive must be a boolean")
        })?;
        let pattern = regex::escape(&normalized_query(query));
        let pattern = if match_mode == "exact" {
            format!("^(?:{pattern})$")
        } else {
            pattern
        };
        let matcher = RegexBuilder::new(&pattern)
            .case_insensitive(!sensitive)
            .size_limit(2 * 1024 * 1024)
            .build()?;
        let limit = number(args, "limit", 100)?;
        ensure!((1..=500).contains(&limit), "limit must be 1..500");
        let package = args
            .get("package_prefix")
            .map_or(Ok(""), |v| {
                v.as_str().context("package_prefix must be a string")
            })?
            .trim_end_matches('.')
            .to_owned();
        Ok(Self {
            array_components: args["include_array_components"].as_bool().unwrap_or(false),
            matcher,
            kind: choice(
                args,
                "symbol_kind",
                "all",
                &["all", "class", "method", "field"],
            )?
            .into(),
            occurrence: choice(
                args,
                "occurrence",
                "all",
                &["all", "declaration", "reference"],
            )?
            .into(),
            package,
            offset: number(args, "offset", 0)?,
            limit,
        })
    }
}
pub(crate) fn validate(args: &Value) -> Result<()> {
    Query::parse(args).map(|_| ())
}

impl Query {
    fn accepts_class(&self, owner: &str) -> bool {
        self.package.is_empty()
            || owner == self.package
            || owner
                .strip_prefix(&self.package)
                .is_some_and(|s| s.starts_with('.'))
    }
    fn matches_symbol(&self, symbol: &Symbol) -> bool {
        (self.kind == "all" || self.kind == symbol.kind.name())
            && (self.matcher.is_match(&symbol.id)
                || self.matcher.is_match(&symbol.name)
                || (self.array_components
                    && symbol.kind == Kind::Class
                    && self.matcher.is_match(symbol.id.trim_end_matches("[]"))))
    }
    fn matches_site(&self, site: &Site) -> bool {
        match self.occurrence.as_str() {
            "declaration" => site.context == ContextKind::Declaration,
            "reference" => site.context != ContextKind::Declaration,
            _ => true,
        }
    }
    fn matches(&self, symbol: &Symbol, site: &Site) -> bool {
        self.matches_symbol(symbol) && self.matches_site(site)
    }
}

// Retains at most one response page (1 MiB / 500 records), regardless of offset.
// The complete scan computes the exact total and digest without retaining hits.
struct Stream {
    query: Query,
    items: Vec<Value>,
    total: usize,
    bytes: usize,
    full: bool,
    digest: Sha256,
}
impl Stream {
    fn record(&mut self, item: Value) -> Result<()> {
        let encoded = serde_json::to_vec(&item)?;
        self.digest.update(&encoded);
        self.digest.update(b"\n");
        if self.total >= self.query.offset && !self.full {
            ensure!(
                encoded.len() <= 1_000_000,
                "Identifier record exceeds output budget"
            );
            if self.bytes + encoded.len() > 1_000_000 {
                self.full = true;
            } else {
                self.bytes += encoded.len();
                self.items.push(item);
                self.full = self.items.len() == self.query.limit;
            }
        }
        self.total += 1;
        Ok(())
    }
}
fn stream(
    classes: Arc<BTreeMap<String, DexClass>>,
    query: Query,
    cancel: &AtomicBool,
) -> Result<Value> {
    let mut builder = Builder {
        index: Index {
            classes: classes.clone(),
            names: vec![],
            symbols: vec![],
            hash: String::new(),
            estimated_bytes: 0,
            code_methods: 0,
            dynamic_sites: 0,
        },
        ids: BTreeMap::new(),
        operands: HashMap::new(),
        budget: 0,
        stream: Some(Stream {
            query,
            items: vec![],
            total: 0,
            bytes: 0,
            full: false,
            digest: Sha256::new(),
        }),
    };
    let mut scanned = 0;
    for (name, class) in classes.iter() {
        ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
        if !builder.stream.as_ref().unwrap().query.accepts_class(name) {
            continue;
        }
        builder.index.names.clear();
        builder.index.names.push(name.clone());
        builder.class(class, 0, cancel)?;
        scanned += 1;
    }
    ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
    let stream = builder.stream.unwrap();
    ensure!(
        stream.query.offset <= stream.total,
        "Invalid matching-occurrence offset"
    );
    let next = stream.query.offset + stream.items.len();
    Ok(
        json!({"mode":"identifiers","pagination":"hits","execution_mode":"streaming",
        "pagination_order":"class_then_dex_occurrence","items":stream.items,"total":stream.total,
        "offset":stream.query.offset,"offset_unit":"matching identifier occurrences",
        "next_offset":(next<stream.total).then_some(next),"exhausted":next==stream.total,
        "source_hash":format!("{:x}",stream.digest.finalize()),"index_hash":null,"index_complete":false,
        "coverage_complete":true,"scanned_classes":scanned,"skipped_classes":0,
        "out_of_scope_classes":classes.len()-scanned,"package_prefix":stream.query.package,
        "scanned_code_methods":builder.index.code_methods,"indexed_classes":0,
        "dynamic_instruction_sites_excluded":builder.index.dynamic_sites,
        "scope":IDENTIFIER_SCOPE}),
    )
}

const IDENTIFIER_SCOPE: &str = "Original DEX class/method/field declarations, superclass/interfaces, declaration types, and direct type/field/method instruction operands. Method references are declared targets, including unreachable instructions. Generated Java names, locals, comments, string literals, annotations, encoded values, catch metadata, dynamic call sites/handles/prototypes and runtime dispatch are outside this index.";

struct Builder {
    index: Index,
    ids: BTreeMap<(Kind, Arc<str>), u32>,
    operands: HashMap<(usize, Kind, u32), u32>,
    budget: usize,
    stream: Option<Stream>,
}
impl Builder {
    fn charge(&mut self, bytes: usize) -> Result<()> {
        if self.stream.is_some() {
            return Ok(());
        }
        self.index.estimated_bytes = self
            .index
            .estimated_bytes
            .checked_add(bytes)
            .context("Identifier index size overflow")?;
        if self.index.estimated_bytes > self.budget {
            return Err(IndexLimit.into());
        }
        Ok(())
    }
    fn intern(&mut self, kind: Kind, id: String, name: String) -> Result<u32> {
        let id: Arc<str> = id.into();
        if self.stream.is_some() {
            // Only the current symbol exists in streaming mode; no postings or
            // operand cache grow with the number of references or classes.
            self.index.symbols.clear();
            self.index.symbols.push(Symbol {
                kind,
                id,
                name,
                sites: vec![],
            });
            return Ok(0);
        }
        if let Some(index) = self.ids.get(&(kind, id.clone())) {
            return Ok(*index);
        }
        self.charge(id.len() + name.len() + 256)?;
        let index = u32::try_from(self.index.symbols.len())?;
        self.ids.insert((kind, id.clone()), index);
        self.index.symbols.push(Symbol {
            kind,
            id,
            name,
            sites: vec![],
        });
        Ok(index)
    }
    fn add(&mut self, symbol: u32, site: Site) -> Result<()> {
        if let Some(stream) = &mut self.stream {
            let symbol = &self.index.symbols[symbol as usize];
            if stream.query.matches(symbol, &site) {
                let item = self.index.item(symbol, &site)?;
                stream.record(item)?;
            }
            return Ok(());
        }
        // Covers geometric Vec growth and allocator overhead conservatively.
        self.charge(2 * std::mem::size_of::<Site>() + 8)?;
        self.index.symbols[symbol as usize].sites.push(site);
        Ok(())
    }
    fn type_ref(&mut self, descriptor: &str, site: Site) -> Result<()> {
        let base = descriptor.trim_start_matches('[');
        if base.starts_with('L') && base.ends_with(';') {
            let id = type_name(base);
            let name = id.rsplit('.').next().unwrap_or(&id).to_owned();
            let symbol = self.intern(Kind::Class, id, name)?;
            self.add(symbol, site)?;
        }
        Ok(())
    }
    fn operand(&mut self, symbols: &Arc<DexSymbols>, kind: Kind, operand: u32) -> Result<u32> {
        let key = (Arc::as_ptr(symbols) as usize, kind, operand);
        if let Some(id) = self.operands.get(&key) {
            return Ok(*id);
        }
        let i = operand as usize;
        let (id, name) = match kind {
            Kind::Class => {
                let descriptor = symbols.types.get(i).context("Invalid type operand")?;
                let id = type_name(descriptor);
                let name = id.rsplit('.').next().unwrap_or(&id).to_owned();
                (id, name)
            }
            Kind::Method => {
                let &(owner, proto, name) =
                    symbols.methods.get(i).context("Invalid method operand")?;
                let owner = symbols
                    .types
                    .get(owner as usize)
                    .context("Invalid method owner")?;
                let (ret, params) = symbols
                    .protos
                    .get(proto as usize)
                    .context("Invalid method prototype")?;
                let name = symbols
                    .strings
                    .get(name as usize)
                    .context("Invalid method name")?
                    .clone();
                (
                    format!("{}.{}({}){}", type_name(owner), name, params.join(""), ret),
                    name,
                )
            }
            Kind::Field => {
                let &(owner, ty, name) = symbols.fields.get(i).context("Invalid field operand")?;
                let owner = symbols
                    .types
                    .get(owner as usize)
                    .context("Invalid field owner")?;
                let ty = symbols
                    .types
                    .get(ty as usize)
                    .context("Invalid field type")?;
                let name = symbols
                    .strings
                    .get(name as usize)
                    .context("Invalid field name")?
                    .clone();
                (format!("{}.{}:{}", type_name(owner), name, ty), name)
            }
        };
        let id = self.intern(kind, id, name)?;
        if self.stream.is_none() {
            self.charge(96)?;
            self.operands.insert(key, id);
        }
        Ok(id)
    }
    fn class(&mut self, class: &DexClass, ci: u32, cancel: &AtomicBool) -> Result<()> {
        let mut site = Site {
            class: ci,
            member: 0,
            owner: Kind::Class,
            context: ContextKind::Declaration,
            position: NO_POSITION,
        };
        self.type_ref(&class.descriptor, site)?;
        if let Some(parent) = &class.superclass {
            self.type_ref(
                parent,
                Site {
                    context: ContextKind::Superclass,
                    ..site
                },
            )?;
        }
        for ty in &class.interfaces {
            self.type_ref(
                ty,
                Site {
                    context: ContextKind::Interface,
                    ..site
                },
            )?;
        }
        for (fi, field) in class.fields.iter().enumerate() {
            ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
            site = Site {
                class: ci,
                member: u32::try_from(fi)?,
                owner: Kind::Field,
                context: ContextKind::Declaration,
                position: NO_POSITION,
            };
            let id = format!(
                "{}.{}:{}",
                type_name(&field.declaring_type),
                field.name,
                field.field_type
            );
            let symbol = self.intern(Kind::Field, id, field.name.to_string())?;
            self.add(symbol, site)?;
            self.type_ref(
                &field.field_type,
                Site {
                    context: ContextKind::FieldType,
                    ..site
                },
            )?;
        }
        for (mi, method) in class.methods.iter().enumerate() {
            ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
            site = Site {
                class: ci,
                member: u32::try_from(mi)?,
                owner: Kind::Method,
                context: ContextKind::Declaration,
                position: NO_POSITION,
            };
            let symbol = self.intern(Kind::Method, method_id(method), method.name.to_string())?;
            self.add(symbol, site)?;
            self.type_ref(
                &method.return_type,
                Site {
                    context: ContextKind::ReturnType,
                    ..site
                },
            )?;
            for (pi, ty) in method.parameters.iter().enumerate() {
                self.type_ref(
                    ty,
                    Site {
                        context: ContextKind::ParameterType,
                        position: u32::try_from(pi)?,
                        ..site
                    },
                )?;
            }
            let Some(code) = &method.code else { continue };
            self.index.code_methods += 1;
            let mut pc = 0;
            let mut next_check = 0;
            while pc < code.instructions.len() {
                if pc >= next_check {
                    ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
                    next_check = pc.saturating_add(256);
                }
                let (width, payload) = instruction_width(&code.instructions, pc)
                    .with_context(|| format!("{} at {pc:#x}", method_id(method)))?;
                ensure!(
                    pc + width <= code.instructions.len(),
                    "Truncated DEX in {} at {pc:#x}",
                    method_id(method)
                );
                if !payload {
                    let kind = match code.instructions[pc] as u8 {
                        0x1c | 0x1f | 0x20 | 0x22..=0x25 => Some(Kind::Class),
                        0x52..=0x6d => Some(Kind::Field),
                        0x6e..=0x72 | 0x74..=0x78 | 0xfa | 0xfb => Some(Kind::Method),
                        0xfc..=0xff => {
                            self.index.dynamic_sites += 1;
                            None
                        }
                        _ => None,
                    };
                    if let Some(kind) = kind {
                        let symbol = self
                            .operand(&class.symbols, kind, code.instructions[pc + 1] as u32)
                            .with_context(|| format!("{} at {pc:#x}", method_id(method)))?;
                        self.add(
                            symbol,
                            Site {
                                context: ContextKind::Instruction,
                                position: u32::try_from(pc)?,
                                ..site
                            },
                        )?;
                    }
                }
                pc += width;
            }
        }
        Ok(())
    }
}
impl Index {
    #[cfg(test)]
    fn build(classes: Arc<BTreeMap<String, DexClass>>, cancel: &AtomicBool) -> Result<Self> {
        Self::build_with_budget(classes, cancel, MAX_INDEX_BYTES)
    }
    fn build_with_budget(
        classes: Arc<BTreeMap<String, DexClass>>,
        cancel: &AtomicBool,
        budget: usize,
    ) -> Result<Self> {
        let mut builder = Builder {
            index: Self {
                classes: classes.clone(),
                names: vec![],
                symbols: vec![],
                hash: String::new(),
                estimated_bytes: 0,
                code_methods: 0,
                dynamic_sites: 0,
            },
            ids: BTreeMap::new(),
            operands: HashMap::new(),
            budget,
            stream: None,
        };
        for (ci, (name, class)) in classes.iter().enumerate() {
            ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
            builder.charge(name.len() + 64)?;
            builder.index.names.push(name.clone());
            builder.class(class, u32::try_from(ci)?, cancel)?;
        }
        // Stable order is independent of hashmap iteration and pagination.
        builder
            .index
            .symbols
            .sort_by(|a, b| (a.kind, &a.id).cmp(&(b.kind, &b.id)));
        let mut digest = Sha256::new();
        for name in &builder.index.names {
            ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
            digest.update([0]);
            digest.update(name.as_bytes());
            digest.update(
                classes[name]
                    .symbols
                    .origin
                    .get()
                    .map_or("", String::as_str)
                    .as_bytes(),
            );
            for method in &classes[name].methods {
                if let Some(code) = &method.code {
                    digest.update(code.offset.to_le_bytes());
                    for (i, word) in code.instructions.iter().enumerate() {
                        if i % 256 == 0 {
                            ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
                        }
                        digest.update(word.to_le_bytes());
                    }
                }
            }
        }
        for symbol in &builder.index.symbols {
            ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
            digest.update([symbol.kind as u8]);
            digest.update(symbol.id.as_bytes());
            for (i, site) in symbol.sites.iter().enumerate() {
                if i % 256 == 0 {
                    ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
                }
                digest.update(site.class.to_le_bytes());
                digest.update(site.member.to_le_bytes());
                digest.update([site.owner as u8, site.context as u8]);
                digest.update(site.position.to_le_bytes());
            }
        }
        builder.index.hash = format!("{:x}", digest.finalize());
        Ok(builder.index)
    }
    fn item(&self, symbol: &Symbol, site: &Site) -> Result<Value> {
        let name = &self.names[site.class as usize];
        let class = &self.classes[name];
        let method = (site.owner == Kind::Method).then(|| &class.methods[site.member as usize]);
        let field = (site.owner == Kind::Field).then(|| &class.fields[site.member as usize]);
        let mut item = json!({"symbol_id":symbol.id.as_ref(),"symbol_name":symbol.name,"symbol_kind":symbol.kind.name(),
            "occurrence":if site.context==ContextKind::Declaration {"declaration"} else {"reference"},"context":site.context.name(),
            "class_id":name,"method_id":method.map(method_id),"field_id":field.map(|f|format!("{}.{}:{}",type_name(&f.declaring_type),f.name,f.field_type)),
            "dex_entry":class.symbols.origin.get(),"code_unit_offset":Value::Null});
        if site.context == ContextKind::ParameterType {
            item["parameter_index"] = json!(site.position);
        }
        if site.context == ContextKind::Instruction {
            let code = method
                .and_then(|m| m.code.as_ref())
                .context("Indexed instruction has no code")?;
            let pc = site.position as usize;
            let (width, _) = instruction_width(&code.instructions, pc)?;
            item["code_unit_offset"] = json!(pc);
            item["offset_unit"] = json!("16-bit DEX code units");
            item["method_code_offset_bytes"] = json!(code.offset);
            item["opcode_value"] = json!(code.instructions[pc] as u8);
            item["operand_index"] = json!(code.instructions[pc + 1]);
            item["instruction_words"] = json!(&code.instructions[pc..pc + width]);
        }
        Ok(item)
    }
    fn search(&self, query: &Query, cancel: &AtomicBool) -> Result<Value> {
        let mut total = 0usize;
        let mut items = vec![];
        let mut bytes = 0;
        let mut full = false;
        for symbol in &self.symbols {
            ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
            if !query.matches_symbol(symbol) {
                continue;
            }
            for (i, site) in symbol.sites.iter().enumerate() {
                if i % 256 == 0 {
                    ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
                }
                if !query.matches_site(site)
                    || !query.accepts_class(&self.names[site.class as usize])
                {
                    continue;
                }
                if total >= query.offset && !full {
                    let item = self.item(symbol, site)?;
                    let size = serde_json::to_vec(&item)?.len();
                    ensure!(size <= 1_000_000, "Identifier record exceeds output budget");
                    if bytes + size > 1_000_000 {
                        full = true;
                    } else {
                        bytes += size;
                        items.push(item);
                        full = items.len() == query.limit;
                    }
                }
                total += 1;
            }
        }
        ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
        ensure!(query.offset <= total, "Invalid matching-occurrence offset");
        let next = query.offset + items.len();
        Ok(
            json!({"mode":"identifiers","pagination":"hits","items":items,"total":total,"offset":query.offset,
            "offset_unit":"matching identifier occurrences","next_offset":(next<total).then_some(next),"exhausted":next==total,
            "execution_mode":"index","pagination_order":"symbol_then_occurrence","coverage_complete":true,"skipped_classes":0,
            "index_complete":true,"index_hash":self.hash,"indexed_classes":self.names.len(),"indexed_code_methods":self.code_methods,
            "indexed_symbols":self.symbols.len(),"index_estimated_bytes":self.estimated_bytes,"dynamic_instruction_sites_excluded":self.dynamic_sites,
            "scope":IDENTIFIER_SCOPE}),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_dex::{DexCode, DexField};
    fn fixture(name: &str, dex: &str, callee: &str) -> DexClass {
        let mut class = crate::native_dex::parse(include_bytes!("../tests/fixtures/hello.dex"))
            .unwrap()
            .classes
            .remove(0);
        class.descriptor = format!("L{};", name.replace('.', "/")).into();
        class.superclass = Some("Lframework/Api;".into());
        class.interfaces.clear();
        class.fields = vec![DexField {
            declaring_type: class.descriptor.clone(),
            name: "own".into(),
            field_type: "Ljava/lang/String;".into(),
            access_flags: 1,
            is_static: false,
        }];
        class.methods.truncate(1);
        let method = &mut class.methods[0];
        method.declaring_type = class.descriptor.clone();
        method.name = "run".into();
        method.return_type = "V".into();
        method.parameters = vec!["Ljava/lang/String;".into()];
        method.code = Some(DexCode {
            registers: 1,
            ins: 0,
            outs: 1,
            tries: 0,
            try_regions: vec![],
            offset: 12,
            instructions: vec![
                0x1071, 0, 0, 0x60, 0, 0x1c, 0, 0x1a, 0, 0, 0x300, 2, 3, 0, 0x1071, 1, 0,
            ],
        });
        let symbols = DexSymbols {
            strings: vec![callee.into(), "value".into(), "unusedApi".into()],
            types: vec!["Lframework/Api;".into(), "Ljava/lang/String;".into()],
            protos: vec![("V".into(), vec!["Ljava/lang/String;".into()])],
            methods: vec![(0, 0, 0), (0, 0, 2)],
            fields: vec![(0, 1, 1)],
            ..Default::default()
        };
        symbols.origin.set(dex.into()).unwrap();
        class.symbols = Arc::new(symbols);
        class
    }
    pub(super) fn classes() -> Arc<BTreeMap<String, DexClass>> {
        Arc::new(BTreeMap::from([
            (
                "app.One".into(),
                fixture("app.One", "classes.dex", "parseUri"),
            ),
            (
                "other.Two".into(),
                fixture("other.Two", "classes2.dex", "parseOther"),
            ),
        ]))
    }
    fn search(index: &Index, args: Value) -> Value {
        index
            .search(&Query::parse(&args).unwrap(), &AtomicBool::new(false))
            .unwrap()
    }
    #[test]
    fn scoped_search_bypasses_global_budget_and_unrelated_invalid_dex() {
        let mut map = Arc::try_unwrap(classes()).ok().unwrap();
        map.get_mut("other.Two").unwrap().methods[0]
            .code
            .as_mut()
            .unwrap()
            .instructions = vec![0x1071, 0];
        let cache = Cache::default();
        let cancel = AtomicBool::new(false);
        // Even a zero-byte global budget and malformed unrelated code cannot
        // block a scoped request. External targets referenced inside it remain.
        let page = cache
            .search_with_budget(
                Arc::new(map),
                &json!({"query":"parseUri","package_prefix":"app"}),
                &cancel,
                0,
            )
            .unwrap();
        assert_eq!(page["total"], 1);
        assert_eq!(
            page["items"][0]["symbol_id"],
            "framework.Api.parseUri(Ljava/lang/String;)V"
        );
        assert_eq!(page["coverage_complete"], true);
        assert_eq!(page["index_complete"], false);
        assert_eq!(page["scanned_classes"], 1);
        assert_eq!(page["out_of_scope_classes"], 1);
        assert_eq!(page["skipped_classes"], 0);
        assert!(cache.index.lock().unwrap().is_none());
        assert!(!cache.streaming.load(Ordering::Relaxed));
        let empty = cache
            .search_with_budget(
                classes(),
                &json!({"query":"parse","package_prefix":"ap"}),
                &cancel,
                0,
            )
            .unwrap();
        assert_eq!(empty["total"], 0);
        assert_eq!(empty["scanned_classes"], 0);
    }

    #[test]
    fn overflow_streams_complete_stable_pages_and_does_not_retry_global_build() {
        let cache = Cache::default();
        let cancel = AtomicBool::new(false);
        let args = json!({"query":"String","symbol_kind":"class"});
        let all = cache
            .search_with_budget(classes(), &args, &cancel, 1)
            .unwrap();
        assert_eq!(all["total"], 4);
        assert_eq!(all["execution_mode"], "streaming");
        assert_eq!(all["coverage_complete"], true);
        assert_eq!(all["scanned_classes"], 2);
        assert!(cache.streaming.load(Ordering::Relaxed));
        let mut items = vec![];
        for offset in 0..4 {
            let page = cache
                .search_with_budget(
                    classes(),
                    &json!({"query":"String","symbol_kind":"class","limit":1,"offset":offset}),
                    &cancel,
                    usize::MAX,
                )
                .unwrap();
            assert_eq!(page["execution_mode"], "streaming");
            assert_eq!(page["source_hash"], all["source_hash"]);
            assert_eq!(page["total"], 4);
            items.extend(page["items"].as_array().unwrap().iter().cloned());
        }
        assert_eq!(items, all["items"].as_array().unwrap().clone());
        assert!(cache.index.lock().unwrap().is_none());
        let terminal = cache
            .search_with_budget(
                classes(),
                &json!({"query":"String","symbol_kind":"class","offset":4}),
                &cancel,
                1,
            )
            .unwrap();
        assert_eq!(terminal["items"], json!([]));
        assert_eq!(terminal["exhausted"], true);
        assert!(terminal["next_offset"].is_null());
        assert!(
            cache
                .search_with_budget(
                    classes(),
                    &json!({"query":"String","symbol_kind":"class","offset":5}),
                    &cancel,
                    1
                )
                .is_err()
        );
        let fresh = Cache::default()
            .search_with_budget(classes(), &args, &cancel, usize::MAX)
            .unwrap();
        assert_eq!(fresh["execution_mode"], "index");
    }

    #[test]
    fn streaming_and_index_have_identical_occurrences_for_all_filters() {
        let index = Index::build(classes(), &AtomicBool::new(false)).unwrap();
        for args in [
            json!({"query":".","occurrence":"all"}),
            json!({"query":"run","occurrence":"declaration"}),
            json!({"query":"parse","occurrence":"reference"}),
            json!({"query":"value","symbol_kind":"field"}),
            json!({"query":"String","symbol_kind":"class"}),
            json!({"query":"parseuri","case_sensitive":false}),
            json!({"query":"Lframework/Api;->parseUri(Ljava/lang/String;)V","match_mode":"exact"}),
            json!({"query":"parse","package_prefix":"app."}),
            json!({"query":"unusedApi"}),
        ] {
            let expected = search(&index, args.clone());
            let actual = stream(
                classes(),
                Query::parse(&args).unwrap(),
                &AtomicBool::new(false),
            )
            .unwrap();
            let sorted = |v: &Value| {
                let mut items: Vec<_> = v["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|i| serde_json::to_string(i).unwrap())
                    .collect();
                items.sort();
                items
            };
            assert_eq!(sorted(&actual), sorted(&expected), "{args}");
            assert_eq!(actual["total"], expected["total"]);
        }
    }

    #[test]
    fn scoped_queries_do_not_wait_for_a_global_build() {
        let cache = Cache::default();
        let _building = cache.index.lock().unwrap();
        let result = cache
            .search(
                classes(),
                &json!({"query":"parse","package_prefix":"app"}),
                &AtomicBool::new(false),
            )
            .unwrap();
        assert_eq!(result["total"], 1);
    }

    #[test]
    fn streaming_rejects_late_malformed_input_even_after_page_is_full() {
        let mut map = Arc::try_unwrap(classes()).ok().unwrap();
        map.get_mut("other.Two").unwrap().methods[0]
            .code
            .as_mut()
            .unwrap()
            .instructions = vec![0x1071, 0];
        let result = Cache::default().search_with_budget(
            Arc::new(map),
            &json!({"query":"parseUri","limit":1}),
            &AtomicBool::new(false),
            1,
        );
        assert!(result.unwrap_err().to_string().contains("other.Two"));
    }

    #[test]
    fn streaming_cancellation_and_invalid_scoped_code_are_errors() {
        let cache = Cache::default();
        assert!(
            cache
                .search(
                    classes(),
                    &json!({"query":"parse","package_prefix":"app"}),
                    &AtomicBool::new(true)
                )
                .is_err()
        );
        let mut map = Arc::try_unwrap(classes()).ok().unwrap();
        map.get_mut("app.One").unwrap().methods[0]
            .code
            .as_mut()
            .unwrap()
            .instructions = vec![0x1071, 0];
        assert!(
            cache
                .search(
                    Arc::new(map),
                    &json!({"query":"parse","package_prefix":"app"}),
                    &AtomicBool::new(false)
                )
                .is_err()
        );
        assert!(cache.index.lock().unwrap().is_none());
    }

    #[test]
    fn streaming_page_memory_does_not_grow_with_hit_count_or_offset() {
        let mut class = fixture("app.One", "classes.dex", "parseUri");
        class.methods[0].code.as_mut().unwrap().instructions = [0x1071, 0, 0].repeat(20_000);
        let map = Arc::new(BTreeMap::from([("app.One".into(), class)]));
        let args = json!({"query":"parseUri","package_prefix":"app","offset":19_998,"limit":1});
        let page = stream(map, Query::parse(&args).unwrap(), &AtomicBool::new(false)).unwrap();
        assert_eq!(page["total"], 20_000);
        assert_eq!(page["items"].as_array().unwrap().len(), 1);
        assert_eq!(page["items"][0]["code_unit_offset"], 19_998 * 3);
        assert_eq!(page["next_offset"], 19_999);
    }

    #[test]
    fn dex_local_operands_exact_names_case_and_smali_aliases_are_correct() {
        let index = Index::build(classes(), &AtomicBool::new(false)).unwrap();
        let a = search(&index, json!({"query":"parseUri","match_mode":"exact"}));
        assert_eq!(a["total"], 1);
        assert_eq!(a["items"][0]["dex_entry"], "classes.dex");
        assert_eq!(a["items"][0]["code_unit_offset"], 0);
        assert_eq!(a["items"][0]["instruction_words"], json!([0x1071, 0, 0]));
        assert_eq!(
            a["items"][0]["symbol_id"],
            "framework.Api.parseUri(Ljava/lang/String;)V"
        );
        assert_eq!(
            search(
                &index,
                json!({"query":"Lframework/Api;->parseUri(Ljava/lang/String;)V","match_mode":"exact"})
            )["items"],
            a["items"]
        );
        assert_eq!(
            search(&index, json!({"query":"parseuri","match_mode":"exact"}))["total"],
            0
        );
        assert_eq!(
            search(
                &index,
                json!({"query":"parseuri","case_sensitive":false,"match_mode":"exact"})
            )["items"],
            a["items"]
        );
        assert_eq!(search(&index, json!({"query":"unusedApi"}))["total"], 0); // unused table entry / fake payload invoke
        assert_eq!(
            search(&index, json!({"query":"parseOther"}))["items"][0]["dex_entry"],
            "classes2.dex"
        );
        let fields = search(
            &index,
            json!({"query":"value","symbol_kind":"field","match_mode":"exact"}),
        );
        assert_eq!(fields["total"], 2);
        assert_eq!(fields["items"][0]["code_unit_offset"], 3);
    }
    #[test]
    fn declarations_types_package_boundaries_and_occurrence_pages_are_complete() {
        let index = Index::build(classes(), &AtomicBool::new(false)).unwrap();
        let all = search(&index, json!({"query":"String","symbol_kind":"class"}));
        assert_eq!(all["total"], 4); // field and parameter types, no fabricated instruction use
        let mut found = vec![];
        let mut offset = 0;
        loop {
            let page = search(
                &index,
                json!({"query":"String","symbol_kind":"class","limit":1,"offset":offset}),
            );
            assert_eq!(page["index_hash"], all["index_hash"]);
            found.extend(page["items"].as_array().unwrap().iter().cloned());
            if let Some(next) = page["next_offset"].as_u64() {
                offset = next;
            } else {
                break;
            }
        }
        assert_eq!(found, all["items"].as_array().unwrap().clone());
        assert_eq!(
            search(&index, json!({"query":"run","occurrence":"declaration"}))["total"],
            2
        );
        assert_eq!(
            search(&index, json!({"query":"run","occurrence":"reference"}))["total"],
            0
        );
        assert_eq!(
            search(&index, json!({"query":"parse","package_prefix":"app."}))["total"],
            1
        );
        assert_eq!(
            search(&index, json!({"query":"parse","package_prefix":"ap"}))["total"],
            0
        );
        assert_eq!(
            search(
                &index,
                json!({"query":"Lframework/Api;","match_mode":"exact","symbol_kind":"class"})
            )["total"],
            4
        );
        let outside = Query::parse(&json!({"query":"missing","offset":1})).unwrap();
        assert!(index.search(&outside, &AtomicBool::new(false)).is_err());
    }
    #[test]
    fn cancelled_failed_and_panicking_builds_are_retryable_and_completed_cache_is_shared() {
        let cache = Cache::default();
        let cancel = AtomicBool::new(false);
        assert!(
            cache
                .get_or_build(&cancel, || anyhow::bail!("broken input"))
                .is_err()
        );
        assert!(cache.index.lock().unwrap().is_none());
        assert!(
            cache
                .get_or_build(&cancel, || panic!("builder panic"))
                .is_err()
        );
        assert!(cache.index.lock().unwrap().is_none());
        assert!(
            cache
                .get_or_build(&cancel, || {
                    let index = Index::build(classes(), &cancel)?;
                    cancel.store(true, Ordering::Relaxed);
                    Ok(index)
                })
                .is_err()
        );
        assert!(cache.index.lock().unwrap().is_none());
        cancel.store(false, Ordering::Relaxed);
        let (first, hit) = cache
            .get_or_build(&cancel, || Index::build(classes(), &cancel))
            .unwrap();
        assert!(!hit);
        let (second, hit) = cache
            .clone()
            .get_or_build(&cancel, || panic!("must reuse index"))
            .unwrap();
        assert!(hit && Arc::ptr_eq(&first, &second));
    }
    #[test]
    fn waiting_for_index_is_cancellable_and_invalid_requests_do_not_build() {
        let cache = Cache::default();
        let guard = cache.index.lock().unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = cancel.clone();
        let worker_cache = cache.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let result = worker_cache.get_or_build(&worker_cancel, || panic!("blocked builder"));
            tx.send(result.is_err()).unwrap();
        });
        cancel.store(true, Ordering::Relaxed);
        assert!(rx.recv_timeout(Duration::from_secs(2)).unwrap());
        drop(guard);
        worker.join().unwrap();
        for args in [
            json!({"query":""}),
            json!({"query":"x","limit":501}),
            json!({"query":"x","pagination":"classes"}),
            json!({"query":"x","source_offset":0}),
            json!({"query":"x","match_mode":"regex"}),
            json!({"query":"x","symbol_kind":"bogus"}),
        ] {
            assert!(
                cache
                    .search(classes(), &args, &AtomicBool::new(false))
                    .is_err()
            );
            assert!(cache.index.lock().unwrap().is_none());
        }
    }
    #[test]
    fn reopening_resets_index_and_old_snapshot_keeps_its_own_project() {
        use crate::engine::{DecompilerEngine, NativeEngine};
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let mut engine = NativeEngine::default();
        engine.open(&root.join("hello.dex")).unwrap();
        let args = json!({"query":"sample.Hello","match_mode":"exact","symbol_kind":"class"});
        let cancel = AtomicBool::new(false);
        let original = engine.search_identifiers(&args, &cancel).unwrap();
        assert_eq!(original["total"], 1);
        let snapshot = engine.clone();
        engine.open(&root.join("navigation.dex")).unwrap();
        let reopened = engine.search_identifiers(&args, &cancel).unwrap();
        assert_eq!(reopened["total"], 0);
        assert_eq!(reopened["index_cached"], false);
        assert_eq!(
            snapshot.search_identifiers(&args, &cancel).unwrap()["total"],
            1
        );
    }

    #[test]
    fn invalid_reference_and_truncated_code_do_not_publish_partial_results() {
        for words in [
            vec![0x1071, 2, 0],
            vec![0x1071, 0],
            vec![0x60, 1],
            vec![0x1c, 2],
            vec![0x400],
        ] {
            let mut c = fixture("app.One", "classes.dex", "parseUri");
            c.methods[0].code.as_mut().unwrap().instructions = words;
            let map = Arc::new(BTreeMap::from([("app.One".into(), c)]));
            let cache = Cache::default();
            assert!(
                cache
                    .search(map, &json!({"query":"parseUri"}), &AtomicBool::new(false))
                    .is_err()
            );
            assert!(cache.index.lock().unwrap().is_none());
        }
        assert!(Index::build(classes(), &AtomicBool::new(true)).is_err());
    }
}
