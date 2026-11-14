//! Build a DEX file from interned pools and class definitions.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::dex::write::fix_checksums;
use crate::dex::NO_INDEX;
use crate::error::{DexError, Result};
use crate::leb128::{write_sleb128, write_uleb128, write_uleb128p1};

/// A method body ready for emission.
#[derive(Clone, Debug, Default)]
pub struct BuiltCode {
    pub registers_size: u16,
    pub ins_size: u16,
    pub outs_size: u16,
    pub insns: Vec<u8>,
    pub tries: Vec<BuiltTry>,
    pub debug_info_off: u32, // filled during finish; leave 0 to omit
    pub debug_ops: Vec<u8>,  // raw debug_info_item bytes (optional)
}

#[derive(Clone, Debug)]
pub struct BuiltTry {
    pub start_unit: u32,
    pub insn_count: u16,
    /// Typed handlers: (type_idx, handler_unit).
    pub handlers: Vec<(u32, u32)>,
    /// Catch-all handler address in code units, if any.
    pub catch_all: Option<u32>,
}

#[derive(Clone, Debug)]
pub struct BuiltField {
    pub class: String,
    pub name: String,
    pub typ: String,
    pub access_flags: u32,
    pub static_field: bool,
    /// Optional encoded static initializer (only for static fields; ordered with static_fields).
    pub static_value: Option<crate::dex::encoded_value::EncodedValue>,
}

#[derive(Clone, Debug)]
pub struct BuiltMethod {
    pub class: String,
    pub name: String,
    pub proto: String, // "(I)V"
    pub access_flags: u32,
    pub code: Option<BuiltCode>,
    pub direct: bool,
}

#[derive(Clone, Debug)]
pub struct BuiltClass {
    pub descriptor: String,
    pub access_flags: u32,
    pub superclass: Option<String>,
    pub interfaces: Vec<String>,
    pub source_file: Option<String>,
    pub static_fields: Vec<BuiltField>,
    pub instance_fields: Vec<BuiltField>,
    pub direct_methods: Vec<BuiltMethod>,
    pub virtual_methods: Vec<BuiltMethod>,
    /// Annotations with pool indices already resolved (or empty).
    pub annotations: crate::dex::annotations::AnnotationsDirectory,
}

/// Incrementally builds a DEX file.
#[derive(Default)]
pub struct DexBuilder {
    strings: BTreeSet<String>,
    types: BTreeSet<String>,
    /// shorty -> (return_type, params)
    protos: BTreeMap<String, (String, Vec<String>)>,
    fields: BTreeSet<(String, String, String)>, // class, name, type
    methods: BTreeSet<(String, String, String)>, // class, name, proto
    classes: Vec<BuiltClass>,
}

impl DexBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn intern_string(&mut self, s: impl Into<String>) {
        self.strings.insert(s.into());
    }

    pub fn intern_type(&mut self, descriptor: impl Into<String>) {
        let d = descriptor.into();
        self.intern_string(d.clone());
        self.types.insert(d);
    }

    pub fn intern_proto(&mut self, proto: &str) {
        let (params, ret) = parse_proto(proto);
        for p in &params {
            self.intern_type(p.clone());
        }
        self.intern_type(ret.clone());
        let shorty = make_shorty(&params, &ret);
        self.intern_string(shorty.clone());
        self.protos.insert(proto.to_string(), (ret, params));
        // also key by proto string itself for lookup
        let _ = shorty;
    }

    pub fn intern_field_ref(&mut self, class: &str, name: &str, typ: &str) {
        self.intern_type(class);
        self.intern_string(name);
        self.intern_type(typ);
        self.fields
            .insert((class.to_string(), name.to_string(), typ.to_string()));
    }

    pub fn intern_method_ref(&mut self, class: &str, name: &str, proto: &str) {
        self.intern_type(class);
        self.intern_string(name);
        self.intern_proto(proto);
        self.methods
            .insert((class.to_string(), name.to_string(), proto.to_string()));
    }

    pub fn add_class(&mut self, mut class: BuiltClass) {
        self.intern_type(class.descriptor.clone());
        if let Some(ref s) = class.superclass {
            self.intern_type(s.clone());
        }
        for iface in &class.interfaces {
            self.intern_type(iface.clone());
        }
        if let Some(ref sf) = class.source_file {
            self.intern_string(sf.clone());
        }
        for f in class
            .static_fields
            .iter()
            .chain(class.instance_fields.iter())
        {
            self.intern_field_ref(&f.class, &f.name, &f.typ);
        }
        for m in class
            .direct_methods
            .iter_mut()
            .chain(class.virtual_methods.iter_mut())
        {
            self.intern_method_ref(&m.class, &m.name, &m.proto);
            if let Some(ref code) = m.code {
                // Ensure try handler types are interned
                for t in &code.tries {
                    for (ty, _) in &t.handlers {
                        let _ = ty; // type indices assigned later; descriptors already interned by assembler
                    }
                }
            }
        }
        self.classes.push(class);
    }

    /// Snapshot sorted pool indices (for encoding before `finish`).
    pub fn pool_maps(&self) -> PoolMaps {
        build_pool_maps(self)
    }

    /// Resolve string → index after pools are sorted (call during finish).
    pub fn finish(self) -> Result<Vec<u8>> {
        finish_build(self)
    }
}

fn build_pool_maps(builder: &DexBuilder) -> PoolMaps {
    let strings: Vec<String> = builder.strings.iter().cloned().collect();
    let mut string_idx = HashMap::new();
    for (i, s) in strings.iter().enumerate() {
        string_idx.insert(s.clone(), i as u32);
    }

    let mut types: Vec<String> = builder.types.iter().cloned().collect();
    types.sort_by_key(|t| string_idx.get(t).copied().unwrap_or(u32::MAX));
    let mut type_idx = HashMap::new();
    for (i, t) in types.iter().enumerate() {
        type_idx.insert(t.clone(), i as u32);
    }

    let mut proto_list: Vec<(String, String, Vec<String>)> = builder
        .protos
        .iter()
        .map(|(proto, (ret, params))| (proto.clone(), ret.clone(), params.clone()))
        .collect();
    proto_list.sort_by(|a, b| {
        let ra = type_idx.get(&a.1).copied().unwrap_or(0);
        let rb = type_idx.get(&b.1).copied().unwrap_or(0);
        ra.cmp(&rb).then_with(|| {
            let pa: Vec<u32> = a.2.iter().map(|t| type_idx.get(t).copied().unwrap_or(0)).collect();
            let pb: Vec<u32> = b.2.iter().map(|t| type_idx.get(t).copied().unwrap_or(0)).collect();
            pa.cmp(&pb)
        })
    });
    let mut proto_idx = HashMap::new();
    for (i, (proto, _, _)) in proto_list.iter().enumerate() {
        proto_idx.insert(proto.clone(), i as u32);
    }

    let mut field_list: Vec<(String, String, String)> = builder.fields.iter().cloned().collect();
    field_list.sort_by(|a, b| {
        let ca = type_idx.get(&a.0).copied().unwrap_or(0);
        let cb = type_idx.get(&b.0).copied().unwrap_or(0);
        ca.cmp(&cb)
            .then_with(|| {
                let na = string_idx.get(&a.1).copied().unwrap_or(0);
                let nb = string_idx.get(&b.1).copied().unwrap_or(0);
                na.cmp(&nb)
            })
            .then_with(|| {
                let ta = type_idx.get(&a.2).copied().unwrap_or(0);
                let tb = type_idx.get(&b.2).copied().unwrap_or(0);
                ta.cmp(&tb)
            })
    });
    let mut field_idx = HashMap::new();
    for (i, f) in field_list.iter().enumerate() {
        field_idx.insert(f.clone(), i as u32);
    }

    let mut method_list: Vec<(String, String, String)> = builder.methods.iter().cloned().collect();
    method_list.sort_by(|a, b| {
        let ca = type_idx.get(&a.0).copied().unwrap_or(0);
        let cb = type_idx.get(&b.0).copied().unwrap_or(0);
        ca.cmp(&cb)
            .then_with(|| {
                let na = string_idx.get(&a.1).copied().unwrap_or(0);
                let nb = string_idx.get(&b.1).copied().unwrap_or(0);
                na.cmp(&nb)
            })
            .then_with(|| {
                let pa = proto_idx.get(&a.2).copied().unwrap_or(0);
                let pb = proto_idx.get(&b.2).copied().unwrap_or(0);
                pa.cmp(&pb)
            })
    });
    let mut method_idx = HashMap::new();
    for (i, m) in method_list.iter().enumerate() {
        method_idx.insert(m.clone(), i as u32);
    }

    PoolMaps {
        string_idx,
        type_idx,
        proto_idx,
        field_idx,
        method_idx,
    }
}

/// Index maps produced after sorting pools.
pub struct PoolMaps {
    pub string_idx: HashMap<String, u32>,
    pub type_idx: HashMap<String, u32>,
    pub proto_idx: HashMap<String, u32>,
    pub field_idx: HashMap<(String, String, String), u32>,
    pub method_idx: HashMap<(String, String, String), u32>,
}

impl PoolMaps {
    pub fn string(&self, s: &str) -> Result<u32> {
        self.string_idx
            .get(s)
            .copied()
            .ok_or_else(|| DexError::Parse(format!("missing string: {s}")))
    }
    pub fn ty(&self, s: &str) -> Result<u32> {
        self.type_idx
            .get(s)
            .copied()
            .ok_or_else(|| DexError::Parse(format!("missing type: {s}")))
    }
    pub fn proto(&self, s: &str) -> Result<u32> {
        self.proto_idx
            .get(s)
            .copied()
            .ok_or_else(|| DexError::Parse(format!("missing proto: {s}")))
    }
    pub fn field(&self, c: &str, n: &str, t: &str) -> Result<u32> {
        self.field_idx
            .get(&(c.to_string(), n.to_string(), t.to_string()))
            .copied()
            .ok_or_else(|| DexError::Parse(format!("missing field: {c}->{n}:{t}")))
    }
    pub fn method(&self, c: &str, n: &str, p: &str) -> Result<u32> {
        self.method_idx
            .get(&(c.to_string(), n.to_string(), p.to_string()))
            .copied()
            .ok_or_else(|| DexError::Parse(format!("missing method: {c}->{n}{p}")))
    }
}

fn finish_build(builder: DexBuilder) -> Result<Vec<u8>> {
    let maps = build_pool_maps(&builder);
    let strings: Vec<String> = builder.strings.into_iter().collect();
    let mut types: Vec<String> = builder.types.into_iter().collect();
    types.sort_by_key(|t| maps.string_idx.get(t).copied().unwrap_or(u32::MAX));

    let mut proto_list: Vec<(String, String, Vec<String>)> = builder
        .protos
        .into_iter()
        .map(|(proto, (ret, params))| (proto, ret, params))
        .collect();
    proto_list.sort_by(|a, b| {
        let ra = maps.type_idx.get(&a.1).copied().unwrap_or(0);
        let rb = maps.type_idx.get(&b.1).copied().unwrap_or(0);
        ra.cmp(&rb).then_with(|| {
            let pa: Vec<u32> = a.2.iter().map(|t| maps.type_idx.get(t).copied().unwrap_or(0)).collect();
            let pb: Vec<u32> = b.2.iter().map(|t| maps.type_idx.get(t).copied().unwrap_or(0)).collect();
            pa.cmp(&pb)
        })
    });

    let mut field_list: Vec<(String, String, String)> = builder.fields.into_iter().collect();
    field_list.sort_by(|a, b| {
        let ca = maps.type_idx.get(&a.0).copied().unwrap_or(0);
        let cb = maps.type_idx.get(&b.0).copied().unwrap_or(0);
        ca.cmp(&cb)
            .then_with(|| {
                let na = maps.string_idx.get(&a.1).copied().unwrap_or(0);
                let nb = maps.string_idx.get(&b.1).copied().unwrap_or(0);
                na.cmp(&nb)
            })
            .then_with(|| {
                let ta = maps.type_idx.get(&a.2).copied().unwrap_or(0);
                let tb = maps.type_idx.get(&b.2).copied().unwrap_or(0);
                ta.cmp(&tb)
            })
    });

    let mut method_list: Vec<(String, String, String)> = builder.methods.into_iter().collect();
    method_list.sort_by(|a, b| {
        let ca = maps.type_idx.get(&a.0).copied().unwrap_or(0);
        let cb = maps.type_idx.get(&b.0).copied().unwrap_or(0);
        ca.cmp(&cb)
            .then_with(|| {
                let na = maps.string_idx.get(&a.1).copied().unwrap_or(0);
                let nb = maps.string_idx.get(&b.1).copied().unwrap_or(0);
                na.cmp(&nb)
            })
            .then_with(|| {
                let pa = maps.proto_idx.get(&a.2).copied().unwrap_or(0);
                let pb = maps.proto_idx.get(&b.2).copied().unwrap_or(0);
                pa.cmp(&pb)
            })
    });

    // Sort classes by type index
    let mut classes = builder.classes;
    classes.sort_by_key(|c| maps.type_idx.get(&c.descriptor).copied().unwrap_or(0));

    // --- Layout id tables ---
    let header_size = 0x70u32;
    let string_ids_off = header_size;
    let string_ids_size = strings.len() as u32;
    let type_ids_off = string_ids_off + string_ids_size * 4;
    let type_ids_size = types.len() as u32;
    let proto_ids_off = type_ids_off + type_ids_size * 4;
    let proto_ids_size = proto_list.len() as u32;
    let field_ids_off = proto_ids_off + proto_ids_size * 12;
    let field_ids_size = field_list.len() as u32;
    let method_ids_off = field_ids_off + field_ids_size * 8;
    let method_ids_size = method_list.len() as u32;
    let class_defs_off = method_ids_off + method_ids_size * 8;
    let class_defs_size = classes.len() as u32;
    let data_off = class_defs_off + class_defs_size * 32;

    let mut data = Vec::new();
    // We'll build data section separately then stitch.

    // string_data
    let mut string_data_offs = Vec::with_capacity(strings.len());
    for s in &strings {
        align4(&mut data);
        string_data_offs.push(data_off as usize + data.len());
        let utf16_units = s.encode_utf16().count() as u32;
        data.extend(write_uleb128(utf16_units));
        data.extend(encode_mutf8(s));
        data.push(0);
    }

    // type_list for protos and interfaces
    let mut type_list_cache: HashMap<Vec<u32>, u32> = HashMap::new();
    let mut ensure_type_list = |data: &mut Vec<u8>, type_indices: Vec<u32>| -> u32 {
        if type_indices.is_empty() {
            return 0;
        }
        if let Some(&off) = type_list_cache.get(&type_indices) {
            return off;
        }
        align4(data);
        let off = data_off + data.len() as u32;
        data.extend_from_slice(&(type_indices.len() as u32).to_le_bytes());
        for t in &type_indices {
            data.extend_from_slice(&(*t as u16).to_le_bytes());
        }
        type_list_cache.insert(type_indices, off);
        off
    };

    let mut proto_param_offs = Vec::with_capacity(proto_list.len());
    for (_, _, params) in &proto_list {
        let idxs: Vec<u32> = params
            .iter()
            .map(|p| maps.type_idx[p])
            .collect();
        let off = ensure_type_list(&mut data, idxs);
        proto_param_offs.push(off);
    }

    // code_items + optional debug
    let mut method_code_slots: HashMap<(String, String, String), MethodCodeSlot> = HashMap::new();

    for class in &classes {
        for m in class.direct_methods.iter().chain(class.virtual_methods.iter()) {
            let Some(code) = &m.code else { continue };
            let mut debug_off = 0u32;
            if !code.debug_ops.is_empty() {
                debug_off = data_off + data.len() as u32;
                data.extend_from_slice(&code.debug_ops);
            }

            align4(&mut data);
            let code_off = data_off + data.len() as u32;
            write_code_item(&mut data, code, debug_off)?;
            method_code_slots.insert(
                (m.class.clone(), m.name.clone(), m.proto.clone()),
                MethodCodeSlot { code_off },
            );
        }
    }

    // class_data + interfaces type_lists
    let mut class_data_offs = Vec::with_capacity(classes.len());
    let mut class_iface_offs = Vec::with_capacity(classes.len());
    for class in &classes {
        let iface_idxs: Vec<u32> = class
            .interfaces
            .iter()
            .map(|i| maps.type_idx[i])
            .collect();
        let iface_off = ensure_type_list(&mut data, iface_idxs);
        class_iface_offs.push(iface_off);

        let cd_off = if class.static_fields.is_empty()
            && class.instance_fields.is_empty()
            && class.direct_methods.is_empty()
            && class.virtual_methods.is_empty()
        {
            0
        } else {
            let off = data_off + data.len() as u32;
            write_class_data(&mut data, class, &maps, &method_code_slots)?;
            off
        };
        class_data_offs.push(cd_off);
    }

    // static_values (encoded_array per class) + annotations directories
    let mut class_static_value_offs = Vec::with_capacity(classes.len());
    let mut class_annotation_offs = Vec::with_capacity(classes.len());
    for class in &classes {
        let values: Vec<_> = class
            .static_fields
            .iter()
            .filter_map(|f| f.static_value.clone())
            .collect();
        // DEX static_values covers a prefix of static fields; we emit all provided values in order.
        // If some middle fields lack values, pad with Null so indices align with static_fields that have values trailing.
        let sv_off = if values.is_empty() {
            0
        } else {
            // Prefer emitting one value per static field when any is set (pad missing with null)
            let padded: Vec<_> = class
                .static_fields
                .iter()
                .map(|f| {
                    f.static_value
                        .clone()
                        .unwrap_or(crate::dex::encoded_value::EncodedValue::Null)
                })
                .collect();
            // Trim trailing nulls that were never set
            let mut end = padded.len();
            while end > 0 {
                let f = &class.static_fields[end - 1];
                if f.static_value.is_none() {
                    end -= 1;
                } else {
                    break;
                }
            }
            let slice = &padded[..end];
            if slice.is_empty() {
                0
            } else {
                let off = data_off + data.len() as u32;
                data.extend(crate::dex::encoded_value::encode_encoded_array(slice));
                off
            }
        };
        let _ = values;
        class_static_value_offs.push(sv_off);

        let ann_off = crate::dex::annotations::write_annotations_directory(
            &mut data,
            data_off,
            &class.annotations,
        )?;
        class_annotation_offs.push(ann_off);
    }

    // map_list at end of data
    align4(&mut data);
    let map_off = data_off + data.len() as u32;

    // Build map items
    let mut map_items: Vec<(u16, u32, u32)> = Vec::new();
    // TYPE_HEADER_ITEM = 0x0000
    map_items.push((0x0000, 1, 0));
    if string_ids_size > 0 {
        map_items.push((0x0001, string_ids_size, string_ids_off));
    }
    if type_ids_size > 0 {
        map_items.push((0x0002, type_ids_size, type_ids_off));
    }
    if proto_ids_size > 0 {
        map_items.push((0x0003, proto_ids_size, proto_ids_off));
    }
    if field_ids_size > 0 {
        map_items.push((0x0004, field_ids_size, field_ids_off));
    }
    if method_ids_size > 0 {
        map_items.push((0x0005, method_ids_size, method_ids_off));
    }
    if class_defs_size > 0 {
        map_items.push((0x0006, class_defs_size, class_defs_off));
    }
    // TYPE_CODE_ITEM = 0x2001, TYPE_STRING_DATA = 0x2002, TYPE_TYPE_LIST = 0x1001,
    // TYPE_CLASS_DATA = 0x2000, TYPE_MAP_LIST = 0x1000
    // For simplicity emit map_list only for required sections we know offsets for.
    // Add STRING_DATA_ITEM, TYPE_LIST, CODE_ITEM, CLASS_DATA_ITEM, MAP_LIST.
    let string_data_item_off = if string_ids_size > 0 {
        string_data_offs[0] as u32
    } else {
        0
    };
    if string_ids_size > 0 {
        map_items.push((0x2002, string_ids_size, string_data_item_off));
    }
    // Collect code item count
    let code_count = method_code_slots.len() as u32;
    if code_count > 0 {
        let first_code = method_code_slots.values().map(|s| s.code_off).min().unwrap();
        map_items.push((0x2001, code_count, first_code));
    }
    let class_data_count = class_data_offs.iter().filter(|&&o| o != 0).count() as u32;
    if class_data_count > 0 {
        let first = class_data_offs.iter().copied().filter(|&o| o != 0).min().unwrap();
        map_items.push((0x2000, class_data_count, first));
    }
    if !type_list_cache.is_empty() {
        let first = *type_list_cache.values().min().unwrap();
        map_items.push((0x1001, type_list_cache.len() as u32, first));
    }
    map_items.push((0x1000, 1, map_off));
    map_items.sort_by_key(|(ty, _, off)| (*off, *ty));

    data.extend_from_slice(&(map_items.len() as u32).to_le_bytes());
    for &(ty, size, off) in &map_items {
        data.extend_from_slice(&ty.to_le_bytes());
        data.extend_from_slice(&0u16.to_le_bytes()); // unused
        data.extend_from_slice(&size.to_le_bytes());
        data.extend_from_slice(&off.to_le_bytes());
    }

    let data_size = data.len() as u32;
    let file_size = data_off + data_size;

    // --- Assemble full file ---
    let mut out = vec![0u8; file_size as usize];

    // magic + version
    out[0..4].copy_from_slice(b"dex\n");
    out[4..8].copy_from_slice(b"035\0");
    // checksum/signature filled later
    write_u32(&mut out, 32, file_size);
    write_u32(&mut out, 36, header_size);
    write_u32(&mut out, 40, 0x1234_5678);
    write_u32(&mut out, 44, 0); // link_size
    write_u32(&mut out, 48, 0); // link_off
    write_u32(&mut out, 52, map_off);
    write_u32(&mut out, 56, string_ids_size);
    write_u32(&mut out, 60, string_ids_off);
    write_u32(&mut out, 64, type_ids_size);
    write_u32(&mut out, 68, type_ids_off);
    write_u32(&mut out, 72, proto_ids_size);
    write_u32(&mut out, 76, proto_ids_off);
    write_u32(&mut out, 80, field_ids_size);
    write_u32(&mut out, 84, field_ids_off);
    write_u32(&mut out, 88, method_ids_size);
    write_u32(&mut out, 92, method_ids_off);
    write_u32(&mut out, 96, class_defs_size);
    write_u32(&mut out, 100, class_defs_off);
    write_u32(&mut out, 104, data_size);
    write_u32(&mut out, 108, data_off);

    // string_ids
    for (i, &off) in string_data_offs.iter().enumerate() {
        write_u32(&mut out, string_ids_off as usize + i * 4, off as u32);
    }
    // type_ids
    for (i, t) in types.iter().enumerate() {
        let sidx = maps.string_idx[t];
        write_u32(&mut out, type_ids_off as usize + i * 4, sidx);
    }
    // proto_ids
    for (i, (proto, ret, _)) in proto_list.iter().enumerate() {
        let shorty = make_shorty(
            &parse_proto(proto).0,
            ret,
        );
        let shorty_idx = maps.string_idx[&shorty];
        let ret_idx = maps.type_idx[ret];
        let base = proto_ids_off as usize + i * 12;
        write_u32(&mut out, base, shorty_idx);
        write_u32(&mut out, base + 4, ret_idx);
        write_u32(&mut out, base + 8, proto_param_offs[i]);
    }
    // field_ids
    for (i, (c, n, t)) in field_list.iter().enumerate() {
        let base = field_ids_off as usize + i * 8;
        let ci = maps.type_idx[c] as u16;
        let ti = maps.type_idx[t] as u16;
        let ni = maps.string_idx[n];
        out[base..base + 2].copy_from_slice(&ci.to_le_bytes());
        out[base + 2..base + 4].copy_from_slice(&ti.to_le_bytes());
        write_u32(&mut out, base + 4, ni);
    }
    // method_ids
    for (i, (c, n, p)) in method_list.iter().enumerate() {
        let base = method_ids_off as usize + i * 8;
        let ci = maps.type_idx[c] as u16;
        let pi = maps.proto_idx[p] as u16;
        let ni = maps.string_idx[n];
        out[base..base + 2].copy_from_slice(&ci.to_le_bytes());
        out[base + 2..base + 4].copy_from_slice(&pi.to_le_bytes());
        write_u32(&mut out, base + 4, ni);
    }
    // class_defs
    for (i, class) in classes.iter().enumerate() {
        let base = class_defs_off as usize + i * 32;
        write_u32(&mut out, base, maps.type_idx[&class.descriptor]);
        write_u32(&mut out, base + 4, class.access_flags);
        let super_idx = class
            .superclass
            .as_ref()
            .map(|s| maps.type_idx[s])
            .unwrap_or(NO_INDEX);
        write_u32(&mut out, base + 8, super_idx);
        write_u32(&mut out, base + 12, class_iface_offs[i]);
        let sf = class
            .source_file
            .as_ref()
            .map(|s| maps.string_idx[s])
            .unwrap_or(NO_INDEX);
        write_u32(&mut out, base + 16, sf);
        write_u32(&mut out, base + 20, class_annotation_offs[i]);
        write_u32(&mut out, base + 24, class_data_offs[i]);
        write_u32(&mut out, base + 28, class_static_value_offs[i]);
    }

    out[data_off as usize..].copy_from_slice(&data);
    fix_checksums(&mut out)?;
    Ok(out)
}

fn write_code_item(data: &mut Vec<u8>, code: &BuiltCode, debug_off: u32) -> Result<()> {
    if code.insns.len() % 2 != 0 {
        return Err(DexError::Parse("insns length must be even".into()));
    }
    let insns_size = (code.insns.len() / 2) as u32;
    let tries_size = code.tries.len() as u16;
    data.extend_from_slice(&code.registers_size.to_le_bytes());
    data.extend_from_slice(&code.ins_size.to_le_bytes());
    data.extend_from_slice(&code.outs_size.to_le_bytes());
    data.extend_from_slice(&tries_size.to_le_bytes());
    data.extend_from_slice(&debug_off.to_le_bytes());
    data.extend_from_slice(&insns_size.to_le_bytes());
    data.extend_from_slice(&code.insns);
    if tries_size > 0 && insns_size % 2 == 1 {
        data.extend_from_slice(&[0u8; 2]);
    }
    if tries_size > 0 {
        // Reserve try_item slots; fill after handlers are written
        let tries_pos = data.len();
        data.resize(tries_pos + tries_size as usize * 8, 0);

        let handlers_start = data.len();
        data.extend(write_uleb128(tries_size as u32));
        let mut handler_offs = Vec::with_capacity(code.tries.len());
        for t in &code.tries {
            handler_offs.push((data.len() - handlers_start) as u16);
            let has_catch_all = t.catch_all.is_some();
            let typed = t.handlers.len() as i32;
            let size = if has_catch_all { -typed } else { typed };
            // When typed==0 and catch_all, size is 0 and catch-all follows (spec: size <= 0 means catch-all)
            let size = if typed == 0 && has_catch_all { 0 } else { size };
            data.extend(write_sleb128(size));
            for &(ty, addr) in &t.handlers {
                data.extend(write_uleb128(ty));
                data.extend(write_uleb128(addr));
            }
            if has_catch_all {
                data.extend(write_uleb128(t.catch_all.unwrap()));
            }
        }
        for (i, t) in code.tries.iter().enumerate() {
            let base = tries_pos + i * 8;
            data[base..base + 4].copy_from_slice(&t.start_unit.to_le_bytes());
            data[base + 4..base + 6].copy_from_slice(&t.insn_count.to_le_bytes());
            data[base + 6..base + 8].copy_from_slice(&handler_offs[i].to_le_bytes());
        }
    }
    Ok(())
}

fn write_class_data(
    data: &mut Vec<u8>,
    class: &BuiltClass,
    maps: &PoolMaps,
    code_slots: &HashMap<(String, String, String), MethodCodeSlot>,
) -> Result<()> {
    data.extend(write_uleb128(class.static_fields.len() as u32));
    data.extend(write_uleb128(class.instance_fields.len() as u32));
    data.extend(write_uleb128(class.direct_methods.len() as u32));
    data.extend(write_uleb128(class.virtual_methods.len() as u32));

    let write_fields = |data: &mut Vec<u8>, fields: &[BuiltField]| -> Result<()> {
        let mut prev = 0u32;
        for (i, f) in fields.iter().enumerate() {
            let idx = maps.field(&f.class, &f.name, &f.typ)?;
            let diff = if i == 0 { idx } else { idx - prev };
            prev = idx;
            data.extend(write_uleb128(diff));
            data.extend(write_uleb128(f.access_flags));
        }
        Ok(())
    };
    write_fields(data, &class.static_fields)?;
    write_fields(data, &class.instance_fields)?;

    let write_methods = |data: &mut Vec<u8>, methods: &[BuiltMethod]| -> Result<()> {
        let mut prev = 0u32;
        for (i, m) in methods.iter().enumerate() {
            let idx = maps.method(&m.class, &m.name, &m.proto)?;
            let diff = if i == 0 { idx } else { idx - prev };
            prev = idx;
            let code_off = code_slots
                .get(&(m.class.clone(), m.name.clone(), m.proto.clone()))
                .map(|s| s.code_off)
                .unwrap_or(0);
            data.extend(write_uleb128(diff));
            data.extend(write_uleb128(m.access_flags));
            data.extend(write_uleb128(code_off));
        }
        Ok(())
    };
    write_methods(data, &class.direct_methods)?;
    write_methods(data, &class.virtual_methods)?;
    Ok(())
}

struct MethodCodeSlot {
    code_off: u32,
}

fn align4(data: &mut Vec<u8>) {
    while data.len() % 4 != 0 {
        data.push(0);
    }
}

fn write_u32(data: &mut [u8], off: usize, v: u32) {
    data[off..off + 4].copy_from_slice(&v.to_le_bytes());
}

fn parse_proto(proto: &str) -> (Vec<String>, String) {
    let proto = proto.trim();
    let rest = proto.strip_prefix('(').unwrap_or(proto);
    let (params_str, ret) = rest
        .split_once(')')
        .map(|(a, b)| (a, b.to_string()))
        .unwrap_or(("", "V".into()));
    let mut params = Vec::new();
    let mut chars = params_str.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            'L' => {
                let mut s = String::from("L");
                for ch in chars.by_ref() {
                    s.push(ch);
                    if ch == ';' {
                        break;
                    }
                }
                params.push(s);
            }
            '[' => {
                let mut s = String::from("[");
                while let Some('[') = chars.peek().copied() {
                    s.push(chars.next().unwrap());
                }
                if let Some('L') = chars.peek().copied() {
                    s.push(chars.next().unwrap());
                    for ch in chars.by_ref() {
                        s.push(ch);
                        if ch == ';' {
                            break;
                        }
                    }
                } else if let Some(p) = chars.next() {
                    s.push(p);
                }
                params.push(s);
            }
            _ => params.push(c.to_string()),
        }
    }
    (params, ret)
}

fn make_shorty(params: &[String], ret: &str) -> String {
    let mut s = String::new();
    s.push(shorty_char(ret));
    for p in params {
        s.push(shorty_char(p));
    }
    s
}

fn shorty_char(descriptor: &str) -> char {
    let d = descriptor.trim_start_matches('[');
    match d.chars().next().unwrap_or('V') {
        'L' => 'L',
        c => c,
    }
}

fn encode_mutf8(s: &str) -> Vec<u8> {
    let mut out = Vec::new();
    for ch in s.chars() {
        let cp = ch as u32;
        if cp != 0 && cp <= 0x7f {
            out.push(cp as u8);
        } else if cp <= 0x7ff {
            out.push((0xc0 | ((cp >> 6) & 0x1f)) as u8);
            out.push((0x80 | (cp & 0x3f)) as u8);
        } else {
            out.push((0xe0 | ((cp >> 12) & 0x0f)) as u8);
            out.push((0x80 | ((cp >> 6) & 0x3f)) as u8);
            out.push((0x80 | (cp & 0x3f)) as u8);
        }
    }
    out
}

/// Build a minimal debug_info_item with only line numbers (DBG_SET_PROLOGUE_END + DBG_ADVANCE_LINE / DBG_END_SEQUENCE).
pub fn build_simple_debug(line_start: u32, line_ops: &[(u32, u32)]) -> Vec<u8> {
    // line_ops: (addr_units, line)
    let mut out = Vec::new();
    out.extend(write_uleb128(line_start)); // line_start
    out.extend(write_uleb128(0)); // parameters_size
    // Emit address/line advances naively
    let mut addr = 0u32;
    let mut line = line_start as i32;
    for &(a, l) in line_ops {
        if a > addr {
            out.push(0x01); // DBG_ADVANCE_PC
            out.extend(write_uleb128(a - addr));
            addr = a;
        }
        let dl = l as i32 - line;
        if dl != 0 {
            out.push(0x02); // DBG_ADVANCE_LINE
            out.extend(write_sleb128(dl));
            line = l as i32;
        }
        out.push(0x0a); // special opcode for position (approximate: use SET_EPILOGUE? use special 0x0a = line+addr)
    }
    let _ = write_uleb128p1; // silence unused if not used
    out.push(0x00); // DBG_END_SEQUENCE
    out
}

/// One high-level debug directive for [`build_debug_info`].
#[derive(Clone, Debug)]
pub enum DebugBuilderOp {
    /// `.line N` at current address (address stays 0 unless advanced elsewhere).
    Line(u32),
    /// `.local vN "name" Type;`
    StartLocal {
        reg: u32,
        name_idx: u32,
        type_idx: u32,
    },
    Prologue,
    Epilogue,
}

/// Richer debug_info_item: parameter names + locals / prologue / epilogue / lines.
pub fn build_debug_info(
    line_start: u32,
    param_name_idxs: &[Option<u32>],
    ops: &[DebugBuilderOp],
) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend(write_uleb128(line_start.max(1)));
    out.extend(write_uleb128(param_name_idxs.len() as u32));
    for name in param_name_idxs {
        match name {
            Some(idx) => out.extend(write_uleb128p1(*idx as i32)),
            None => out.extend(write_uleb128p1(-1)),
        }
    }
    let mut line = line_start.max(1) as i32;
    for op in ops {
        match op {
            DebugBuilderOp::Line(n) => {
                let dl = *n as i32 - line;
                if dl != 0 {
                    out.push(0x02); // DBG_ADVANCE_LINE
                    out.extend(write_sleb128(dl));
                    line = *n as i32;
                }
                // Emit a position via special opcode 0x0a (addr+0, line+0 after advance).
                out.push(0x0a);
            }
            DebugBuilderOp::StartLocal {
                reg,
                name_idx,
                type_idx,
            } => {
                out.push(0x03); // DBG_START_LOCAL
                out.extend(write_uleb128(*reg));
                out.extend(write_uleb128p1(*name_idx as i32));
                out.extend(write_uleb128p1(*type_idx as i32));
            }
            DebugBuilderOp::Prologue => out.push(0x07),
            DebugBuilderOp::Epilogue => out.push(0x08),
        }
    }
    out.push(0x00); // DBG_END_SEQUENCE
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dex::DexFile;

    #[test]
    fn build_empty_dex() {
        let bytes = DexBuilder::new().finish().unwrap();
        let dex = DexFile::parse(&bytes).unwrap();
        assert_eq!(dex.header.class_defs_size, 0);
    }

    #[test]
    fn build_minimal_class() {
        let mut b = DexBuilder::new();
        b.add_class(BuiltClass {
            descriptor: "LHello;".into(),
            access_flags: 1, // public
            superclass: Some("Ljava/lang/Object;".into()),
            interfaces: vec![],
            source_file: None,
            static_fields: vec![],
            instance_fields: vec![],
            direct_methods: vec![BuiltMethod {
                class: "LHello;".into(),
                name: "<init>".into(),
                proto: "()V".into(),
                access_flags: 0x10001, // public constructor
                direct: true,
                code: Some(BuiltCode {
                    registers_size: 1,
                    ins_size: 1,
                    outs_size: 0,
                    insns: vec![0x0e, 0x00], // return-void
                    tries: vec![],
                    debug_info_off: 0,
                    debug_ops: vec![],
                }),
            }],
            virtual_methods: vec![],
            annotations: Default::default(),
        });
        let bytes = b.finish().unwrap();
        let dex = DexFile::parse(&bytes).unwrap();
        assert_eq!(dex.header.class_defs_size, 1);
        let name = dex.get_type(dex.get_class_def(0).unwrap().class_idx).unwrap();
        assert_eq!(name, "LHello;");
        let cd = dex.get_class_data(&dex.get_class_def(0).unwrap()).unwrap().unwrap();
        assert_eq!(cd.direct_methods.len(), 1);
        let code = dex.get_code_item(cd.direct_methods[0].code_off).unwrap();
        assert_eq!(code.insns_slice(&dex.data), &[0x0e, 0x00]);
    }
}
