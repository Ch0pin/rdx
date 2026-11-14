//! Slice one class into a minimal, spec-valid DEX via [`DexBuilder`].

use dex_bytecode::{format_length, get_opcode_entry, Format, RefKind};

use crate::dex::annotations::{AnnotationItem, AnnotationsDirectory};
use crate::dex::build::{
    build_debug_info, BuiltClass, BuiltCode, BuiltField, BuiltMethod, BuiltTry, DebugBuilderOp,
    DexBuilder, PoolMaps,
};
use crate::dex::encoded_value::{EncodedAnnotation, EncodedValue};
use crate::dex::write::fix_checksums;
use crate::dex::{CodeItem, DexFile, NO_INDEX};
use crate::error::{DexError, Result};
use crate::leb128::{read_sleb128, read_uleb128, read_uleb128p1};

/// Public error alias used by callers that want a distinct unsupported-ref signal.
pub type SliceError = DexError;

/// Harvest one class and emit a minimal DEX through [`DexBuilder`].
pub struct DexSlicer<'a> {
    data: &'a [u8],
    dex: DexFile,
}

impl<'a> DexSlicer<'a> {
    pub fn new(data: &'a [u8]) -> Result<Self> {
        Ok(Self {
            data,
            dex: DexFile::parse(data)?,
        })
    }

    /// `descriptor` in Dalvik form, e.g. `"Lcom/poc/Main;"`.
    pub fn slice_class(&self, descriptor: &str) -> Result<Vec<u8>> {
        let class_def = self
            .find_class_def(descriptor)?
            .ok_or_else(|| DexError::Parse(format!("class not found: {descriptor}")))?;

        self.preflight_unsupported(&class_def)?;

        let mut builder = DexBuilder::new();
        let mut draft = self.harvest(&class_def, &mut builder)?;

        // Catch-handler types (and everything else) must be interned before pool_maps.
        for m in draft
            .direct_methods
            .iter()
            .chain(draft.virtual_methods.iter())
        {
            if let Some(ref code) = m.code {
                for t in &code.tries {
                    for (ty, _) in &t.handlers {
                        // Placeholder: harvest stores descriptor strings via side channel.
                        let _ = ty;
                    }
                }
            }
        }

        // Rebuild tries with remapped type indices after pool_maps.
        let maps = builder.pool_maps();
        self.rewrite_class(&mut draft, &maps)?;
        builder.add_class(draft);
        let mut bytes = builder.finish()?;
        fix_checksums(&mut bytes)?;
        Ok(bytes)
    }

    fn find_class_def(&self, descriptor: &str) -> Result<Option<crate::dex::ClassDef>> {
        for r in self.dex.class_defs() {
            let cd = r?;
            let name = self.dex.get_type(cd.class_idx)?;
            if name == descriptor {
                return Ok(Some(cd));
            }
        }
        Ok(None)
    }

    fn preflight_unsupported(&self, class_def: &crate::dex::ClassDef) -> Result<()> {
        let Some(cdata) = self.dex.get_class_data(class_def)? else {
            return Ok(());
        };
        for m in cdata
            .direct_methods
            .iter()
            .chain(cdata.virtual_methods.iter())
        {
            if m.code_off == 0 {
                continue;
            }
            let code = self.dex.get_code_item(m.code_off)?;
            let insns = code.insns_slice(self.data);
            let mut pc = 0usize;
            while pc + 2 <= insns.len() {
                let unit = u16::from_le_bytes([insns[pc], insns[pc + 1]]);
                if matches!(unit, 0x0100 | 0x0200 | 0x0300) {
                    pc += payload_len(insns, pc)? as usize;
                    continue;
                }
                let op = insns[pc];
                if matches!(op, 0xfc | 0xfd | 0xfe | 0xff) {
                    return Err(DexError::UnsupportedRef);
                }
                let entry = get_opcode_entry(op);
                let len = format_length(entry.format) as usize;
                if len == 0 {
                    pc += 2;
                } else {
                    pc += len;
                }
            }
        }
        Ok(())
    }

    fn harvest(
        &self,
        class_def: &crate::dex::ClassDef,
        builder: &mut DexBuilder,
    ) -> Result<BuiltClass> {
        let descriptor = self.dex.get_type(class_def.class_idx)?;
        builder.intern_type(&descriptor);

        let superclass = if class_def.superclass_idx == NO_INDEX {
            None
        } else {
            let s = self.dex.get_type(class_def.superclass_idx)?;
            builder.intern_type(&s);
            Some(s)
        };

        let interfaces = self.dex.get_interfaces(class_def)?;
        for i in &interfaces {
            builder.intern_type(i);
        }

        let source_file = if class_def.source_file_idx == NO_INDEX {
            None
        } else {
            let s = self.dex.get_string(class_def.source_file_idx)?;
            builder.intern_string(&s);
            Some(s)
        };

        let static_values = self.dex.get_static_values(class_def)?;
        let cdata = self.dex.get_class_data(class_def)?;

        let mut static_fields = Vec::new();
        let mut instance_fields = Vec::new();
        let mut direct_methods = Vec::new();
        let mut virtual_methods = Vec::new();

        // Source field/method idx → identity for annotation remapping.
        let mut field_id_of: std::collections::HashMap<u32, (String, String, String)> =
            std::collections::HashMap::new();
        let mut method_id_of: std::collections::HashMap<u32, (String, String, String)> =
            std::collections::HashMap::new();

        if let Some(ref cd) = cdata {
            for (i, f) in cd.static_fields.iter().enumerate() {
                let info = self.dex.get_field_info(f.field_idx)?;
                builder.intern_field_ref(&info.class, &info.name, &info.typ);
                field_id_of.insert(
                    f.field_idx,
                    (info.class.clone(), info.name.clone(), info.typ.clone()),
                );
                let sv = static_values.get(i).cloned();
                if let Some(ref v) = sv {
                    self.intern_encoded_value(builder, v)?;
                }
                static_fields.push(BuiltField {
                    class: info.class,
                    name: info.name,
                    typ: info.typ,
                    access_flags: f.access_flags,
                    static_field: true,
                    static_value: sv,
                });
            }
            for f in &cd.instance_fields {
                let info = self.dex.get_field_info(f.field_idx)?;
                builder.intern_field_ref(&info.class, &info.name, &info.typ);
                field_id_of.insert(
                    f.field_idx,
                    (info.class.clone(), info.name.clone(), info.typ.clone()),
                );
                instance_fields.push(BuiltField {
                    class: info.class,
                    name: info.name,
                    typ: info.typ,
                    access_flags: f.access_flags,
                    static_field: false,
                    static_value: None,
                });
            }
            for (m, direct) in cd
                .direct_methods
                .iter()
                .map(|m| (m, true))
                .chain(cd.virtual_methods.iter().map(|m| (m, false)))
            {
                let info = self.dex.get_method_info(m.method_idx)?;
                let proto = proto_str(&info);
                builder.intern_method_ref(&info.class, &info.name, &proto);
                method_id_of.insert(
                    m.method_idx,
                    (info.class.clone(), info.name.clone(), proto.clone()),
                );
                let code = if m.code_off != 0 {
                    Some(self.harvest_code(builder, m.code_off)?)
                } else {
                    None
                };
                let bm = BuiltMethod {
                    class: info.class,
                    name: info.name,
                    proto,
                    access_flags: m.access_flags,
                    code,
                    direct,
                };
                if direct {
                    direct_methods.push(bm);
                } else {
                    virtual_methods.push(bm);
                }
            }
        }

        let ann = self.dex.get_annotations(class_def)?;
        self.intern_annotations(builder, &ann)?;

        let _ = (&field_id_of, &method_id_of);

        Ok(BuiltClass {
            descriptor,
            access_flags: class_def.access_flags,
            superclass,
            interfaces,
            source_file,
            static_fields,
            instance_fields,
            direct_methods,
            virtual_methods,
            annotations: ann,
        })
    }

    fn harvest_code(&self, builder: &mut DexBuilder, code_off: u32) -> Result<BuiltCode> {
        let code = self.dex.get_code_item(code_off)?;
        let insns = code.insns_slice(self.data).to_vec();
        self.collect_insn_refs(builder, &insns)?;

        let tries_raw = code.tries(self.data)?;
        let mut tries = Vec::new();
        // Store catch type descriptors as u32::MAX-tagged? We'll use a parallel Vec.
        // BuiltTry.handlers uses type_idx — temporarily store 0 and keep descriptors separately
        // in debug_ops unused. Cleaner: intern types now and leave placeholder 0; rewrite fills.
        let mut try_type_descs: Vec<Vec<Option<String>>> = Vec::new();
        for t in &tries_raw {
            let mut handlers = Vec::new();
            let mut descs = Vec::new();
            let mut catch_all = None;
            for (ty, addr) in &t.handlers {
                match ty {
                    Some(tidx) => {
                        let d = self.dex.get_type(*tidx)?;
                        builder.intern_type(&d);
                        handlers.push((0u32, *addr)); // filled in rewrite
                        descs.push(Some(d));
                    }
                    None => {
                        catch_all = Some(*addr);
                        descs.push(None);
                    }
                }
            }
            // handlers from TryItem may mix typed + catch-all; CodeItem puts catch-all as None.
            // Rebuild properly:
            let mut typed = Vec::new();
            let mut typed_desc = Vec::new();
            let mut ca = None;
            for (ty, addr) in &t.handlers {
                match ty {
                    Some(tidx) => {
                        let d = self.dex.get_type(*tidx)?;
                        builder.intern_type(&d);
                        typed.push((0u32, *addr));
                        typed_desc.push(d);
                    }
                    None => ca = Some(*addr),
                }
            }
            let _ = (handlers, descs, catch_all);
            tries.push(BuiltTry {
                start_unit: t.start_unit,
                insn_count: t.insn_count,
                handlers: typed,
                catch_all: ca,
            });
            try_type_descs.push(typed_desc.into_iter().map(Some).collect());
        }
        let _ = try_type_descs; // rewrite walks tries again from source

        let debug_ops = self.harvest_debug(builder, &code)?;

        Ok(BuiltCode {
            registers_size: code.registers_size,
            ins_size: code.ins_size,
            outs_size: code.outs_size,
            insns, // still source indices; rewritten later
            tries,
            debug_info_off: 0,
            debug_ops,
        })
    }

    fn harvest_debug(&self, builder: &mut DexBuilder, code: &CodeItem) -> Result<Vec<u8>> {
        if code.debug_info_off == 0 {
            return Ok(Vec::new());
        }
        // Collect + rebuild with remapped indices after pool_maps — store raw rebuild inputs
        // as empty for now and rebuild in rewrite using source offset stamped in tries...
        // Simpler approach: intern all strings/types from debug now; rebuild bytes in rewrite
        // by re-parsing source debug_info_off. Stamp debug_info_off temporarily on BuiltCode
        // via unused field — we already zero debug_info_off. Keep source off in debug_ops
        // as a sentinel? Use debug_ops = source off as LE bytes when non-empty marker.
        // Cleanest: store source debug off in the high-level rewrite by re-getting from method.
        // For harvest, just intern referenced strings/types.
        let mut off = code.debug_info_off as usize;
        let data = self.data;
        let (line_start, n) =
            read_uleb128(data, off).ok_or(DexError::Truncated("debug line_start".into()))?;
        off += n;
        let (parameters_size, n) =
            read_uleb128(data, off).ok_or(DexError::Truncated("debug parameters_size".into()))?;
        off += n;
        let mut param_names: Vec<Option<String>> = Vec::new();
        for _ in 0..parameters_size {
            let (name_idx_p1, n) =
                read_uleb128p1(data, off).ok_or(DexError::Truncated("debug param".into()))?;
            off += n;
            if name_idx_p1 >= 0 {
                let s = self.dex.get_string(name_idx_p1 as u32)?;
                builder.intern_string(&s);
                param_names.push(Some(s));
            } else {
                param_names.push(None);
            }
        }
        let mut ops = Vec::new();
        loop {
            if off >= data.len() {
                break;
            }
            let opcode = data[off];
            off += 1;
            match opcode {
                0x00 => break,
                0x01 => {
                    let (_, n) = read_uleb128(data, off).ok_or(DexError::Truncated("dbg pc".into()))?;
                    off += n;
                }
                0x02 => {
                    let (_, n) =
                        read_sleb128(data, off).ok_or(DexError::Truncated("dbg line".into()))?;
                    off += n;
                }
                0x03 | 0x04 => {
                    let (reg, n) =
                        read_uleb128(data, off).ok_or(DexError::Truncated("dbg reg".into()))?;
                    off += n;
                    let (name_idx_p1, n) =
                        read_uleb128p1(data, off).ok_or(DexError::Truncated("dbg name".into()))?;
                    off += n;
                    let (type_idx_p1, n) =
                        read_uleb128p1(data, off).ok_or(DexError::Truncated("dbg type".into()))?;
                    off += n;
                    if opcode == 0x04 {
                        let (sig_p1, n) =
                            read_uleb128p1(data, off).ok_or(DexError::Truncated("dbg sig".into()))?;
                        off += n;
                        if sig_p1 >= 0 {
                            let s = self.dex.get_string(sig_p1 as u32)?;
                            builder.intern_string(&s);
                        }
                    }
                    let mut name_s = None;
                    if name_idx_p1 >= 0 {
                        let s = self.dex.get_string(name_idx_p1 as u32)?;
                        builder.intern_string(&s);
                        name_s = Some(s);
                    }
                    let mut type_s = None;
                    if type_idx_p1 >= 0 {
                        let s = self.dex.get_type(type_idx_p1 as u32)?;
                        builder.intern_type(&s);
                        type_s = Some(s);
                    }
                    if let (Some(n), Some(t)) = (name_s, type_s) {
                        ops.push((reg, n, t));
                    }
                }
                0x05 | 0x06 => {
                    let (_, n) =
                        read_uleb128(data, off).ok_or(DexError::Truncated("dbg end/restart".into()))?;
                    off += n;
                }
                0x07 | 0x08 => {}
                0x09 => {
                    let (name_idx_p1, n) =
                        read_uleb128p1(data, off).ok_or(DexError::Truncated("dbg file".into()))?;
                    off += n;
                    if name_idx_p1 >= 0 {
                        let s = self.dex.get_string(name_idx_p1 as u32)?;
                        builder.intern_string(&s);
                    }
                }
                _ if opcode >= 0x0a => {}
                _ => break,
            }
        }
        // Encode a provisional debug_info with string *names* resolved after maps in rewrite.
        // Store a marker: empty debug_ops means none; non-empty rebuilt in rewrite_class
        // by re-reading code.debug_info_off — so return empty here and rebuild later.
        // We need to pass line_start/params/ops to rewrite. Encode as a simple custom blob:
        let mut blob = Vec::new();
        blob.extend_from_slice(&line_start.to_le_bytes());
        blob.extend_from_slice(&(param_names.len() as u32).to_le_bytes());
        for p in &param_names {
            match p {
                Some(s) => {
                    blob.push(1);
                    let b = s.as_bytes();
                    blob.extend_from_slice(&(b.len() as u32).to_le_bytes());
                    blob.extend_from_slice(b);
                }
                None => blob.push(0),
            }
        }
        blob.extend_from_slice(&(ops.len() as u32).to_le_bytes());
        for (reg, name, typ) in &ops {
            blob.extend_from_slice(&reg.to_le_bytes());
            let nb = name.as_bytes();
            blob.extend_from_slice(&(nb.len() as u32).to_le_bytes());
            blob.extend_from_slice(nb);
            let tb = typ.as_bytes();
            blob.extend_from_slice(&(tb.len() as u32).to_le_bytes());
            blob.extend_from_slice(tb);
        }
        let _ = line_start;
        Ok(blob)
    }

    fn collect_insn_refs(&self, builder: &mut DexBuilder, insns: &[u8]) -> Result<()> {
        let mut pc = 0usize;
        while pc + 2 <= insns.len() {
            let unit = u16::from_le_bytes([insns[pc], insns[pc + 1]]);
            if matches!(unit, 0x0100 | 0x0200 | 0x0300) {
                pc += payload_len(insns, pc)? as usize;
                continue;
            }
            let op = insns[pc];
            let entry = get_opcode_entry(op);
            let len = format_length(entry.format) as usize;
            if len == 0 {
                pc += 2;
                continue;
            }
            if pc + len > insns.len() {
                break;
            }
            match entry.ref_kind {
                RefKind::String => {
                    let idx = read_ref_idx(insns, pc, entry.format)?;
                    let s = self.dex.get_string(idx)?;
                    builder.intern_string(s);
                }
                RefKind::Type => {
                    let idx = read_ref_idx(insns, pc, entry.format)?;
                    let t = self.dex.get_type(idx)?;
                    builder.intern_type(t);
                }
                RefKind::Field => {
                    let idx = read_ref_idx(insns, pc, entry.format)?;
                    let f = self.dex.get_field_info(idx)?;
                    builder.intern_field_ref(&f.class, &f.name, &f.typ);
                }
                RefKind::Method => {
                    let idx = read_ref_idx(insns, pc, entry.format)?;
                    let m = self.dex.get_method_info(idx)?;
                    builder.intern_method_ref(&m.class, &m.name, &proto_str(&m));
                    if matches!(entry.format, Format::F45cc | Format::F4rcc) && len >= 8 {
                        let proto_idx =
                            u16::from_le_bytes([insns[pc + 6], insns[pc + 7]]) as u32;
                        let (ret, params) = self.dex.protos.get_proto(
                            self.data,
                            &self.dex.types,
                            &self.dex.strings,
                            proto_idx,
                        )?;
                        let p = format!("({}){}", params.join(""), ret);
                        builder.intern_proto(&p);
                    }
                }
                RefKind::MethodProto => {
                    let idx = read_ref_idx(insns, pc, entry.format)?;
                    let (ret, params) = self.dex.protos.get_proto(
                        self.data,
                        &self.dex.types,
                        &self.dex.strings,
                        idx,
                    )?;
                    let p = format!("({}){}", params.join(""), ret);
                    builder.intern_proto(&p);
                }
                RefKind::CallSite | RefKind::Varies => {
                    return Err(DexError::UnsupportedRef);
                }
                RefKind::None => {}
            }
            pc += len;
        }
        Ok(())
    }

    fn intern_encoded_value(&self, builder: &mut DexBuilder, v: &EncodedValue) -> Result<()> {
        match v {
            EncodedValue::String(i) => {
                builder.intern_string(self.dex.get_string(*i)?);
            }
            EncodedValue::Type(i) | EncodedValue::MethodType(i) => {
                if matches!(v, EncodedValue::MethodType(_)) {
                    let (ret, params) = self.dex.protos.get_proto(
                        self.data,
                        &self.dex.types,
                        &self.dex.strings,
                        *i,
                    )?;
                    builder.intern_proto(&format!("({}){}", params.join(""), ret));
                } else {
                    builder.intern_type(self.dex.get_type(*i)?);
                }
            }
            EncodedValue::Field(i) | EncodedValue::Enum(i) => {
                let f = self.dex.get_field_info(*i)?;
                builder.intern_field_ref(&f.class, &f.name, &f.typ);
            }
            EncodedValue::Method(i) => {
                let m = self.dex.get_method_info(*i)?;
                builder.intern_method_ref(&m.class, &m.name, &proto_str(&m));
            }
            EncodedValue::MethodHandle(_) => return Err(DexError::UnsupportedRef),
            EncodedValue::Array(arr) => {
                for e in arr {
                    self.intern_encoded_value(builder, e)?;
                }
            }
            EncodedValue::Annotation(a) => {
                self.intern_annotation(builder, a)?;
            }
            _ => {}
        }
        Ok(())
    }

    fn intern_annotation(&self, builder: &mut DexBuilder, a: &EncodedAnnotation) -> Result<()> {
        builder.intern_type(self.dex.get_type(a.type_idx)?);
        for (name_idx, val) in &a.elements {
            builder.intern_string(self.dex.get_string(*name_idx)?);
            self.intern_encoded_value(builder, val)?;
        }
        Ok(())
    }

    fn intern_annotations(
        &self,
        builder: &mut DexBuilder,
        ann: &AnnotationsDirectory,
    ) -> Result<()> {
        for a in &ann.class_annotations {
            self.intern_annotation(builder, &a.annotation)?;
        }
        for (_, set) in &ann.field_annotations {
            for a in set {
                self.intern_annotation(builder, &a.annotation)?;
            }
        }
        for (_, set) in &ann.method_annotations {
            for a in set {
                self.intern_annotation(builder, &a.annotation)?;
            }
        }
        for (_, params) in &ann.parameter_annotations {
            for set in params {
                for a in set {
                    self.intern_annotation(builder, &a.annotation)?;
                }
            }
        }
        Ok(())
    }

    fn rewrite_class(&self, class: &mut BuiltClass, maps: &PoolMaps) -> Result<()> {
        for f in class
            .static_fields
            .iter_mut()
            .chain(class.instance_fields.iter_mut())
        {
            if let Some(ref mut v) = f.static_value {
                *v = self.remap_encoded_value(v, maps)?;
            }
        }
        for m in class
            .direct_methods
            .iter_mut()
            .chain(class.virtual_methods.iter_mut())
        {
            if let Some(ref mut code) = m.code {
                code.insns = self.rewrite_insns(&code.insns, maps)?;
                // Fix try handler type indices by re-parsing is hard without source;
                // harvest left handlers as (0, addr). Re-derive from insn tries stored
                // with descriptors in debug blob? Instead re-intern during harvest into
                // a Vec on BuiltTry via encoding descriptors as high bits — messy.
                // Re-walk: we stored typed handlers with 0; look up from parallel storage
                // embedded after rewrite of tries from descriptors kept in debug_ops? 
                // Simplest fix: during harvest, put type descriptors into a custom
                // extension. Change harvest to store catch types as strings in an
                // unused channel: encode into debug_ops prefix.
            }
        }
        // Fix tries: re-harvest catch types from original class by matching method identity
        self.fill_tries_and_debug(class, maps)?;
        class.annotations = self.remap_annotations(&class.annotations, maps)?;
        Ok(())
    }

    fn fill_tries_and_debug(&self, class: &mut BuiltClass, maps: &PoolMaps) -> Result<()> {
        let class_def = self
            .find_class_def(&class.descriptor)?
            .ok_or_else(|| DexError::Parse("class vanished".into()))?;
        let Some(cdata) = self.dex.get_class_data(&class_def)? else {
            return Ok(());
        };
        let mut all_src = cdata
            .direct_methods
            .iter()
            .chain(cdata.virtual_methods.iter());
        for m in class
            .direct_methods
            .iter_mut()
            .chain(class.virtual_methods.iter_mut())
        {
            let src = all_src
                .next()
                .ok_or_else(|| DexError::Parse("method count mismatch".into()))?;
            let Some(ref mut code) = m.code else { continue };
            if src.code_off == 0 {
                continue;
            }
            let src_code = self.dex.get_code_item(src.code_off)?;
            let tries_raw = src_code.tries(self.data)?;
            let mut tries = Vec::new();
            for t in &tries_raw {
                let mut handlers = Vec::new();
                let mut catch_all = None;
                for (ty, addr) in &t.handlers {
                    match ty {
                        Some(tidx) => {
                            let d = self.dex.get_type(*tidx)?;
                            handlers.push((maps.ty(&d)?, *addr));
                        }
                        None => catch_all = Some(*addr),
                    }
                }
                tries.push(BuiltTry {
                    start_unit: t.start_unit,
                    insn_count: t.insn_count,
                    handlers,
                    catch_all,
                });
            }
            code.tries = tries;

            // Rebuild debug from harvest blob in code.debug_ops
            if !code.debug_ops.is_empty() {
                code.debug_ops = rebuild_debug_from_blob(&code.debug_ops, maps)?;
            }
        }
        Ok(())
    }

    fn rewrite_insns(&self, insns: &[u8], maps: &PoolMaps) -> Result<Vec<u8>> {
        let mut out = insns.to_vec();
        let mut pc = 0usize;
        while pc + 2 <= out.len() {
            let unit = u16::from_le_bytes([out[pc], out[pc + 1]]);
            if matches!(unit, 0x0100 | 0x0200 | 0x0300) {
                pc += payload_len(&out, pc)? as usize;
                continue;
            }
            let op = out[pc];
            let entry = get_opcode_entry(op);
            let len = format_length(entry.format) as usize;
            if len == 0 {
                pc += 2;
                continue;
            }
            if pc + len > out.len() {
                break;
            }
            match entry.ref_kind {
                RefKind::String => {
                    let old = read_ref_idx(&out, pc, entry.format)?;
                    let s = self.dex.get_string(old)?;
                    let new = maps.string(&s)?;
                    write_ref_idx(&mut out, pc, entry.format, new)?;
                }
                RefKind::Type => {
                    let old = read_ref_idx(&out, pc, entry.format)?;
                    let t = self.dex.get_type(old)?;
                    let new = maps.ty(&t)?;
                    write_ref_idx(&mut out, pc, entry.format, new)?;
                }
                RefKind::Field => {
                    let old = read_ref_idx(&out, pc, entry.format)?;
                    let f = self.dex.get_field_info(old)?;
                    let new = maps.field(&f.class, &f.name, &f.typ)?;
                    write_ref_idx(&mut out, pc, entry.format, new)?;
                }
                RefKind::Method => {
                    let old = read_ref_idx(&out, pc, entry.format)?;
                    let m = self.dex.get_method_info(old)?;
                    let new = maps.method(&m.class, &m.name, &proto_str(&m))?;
                    write_ref_idx(&mut out, pc, entry.format, new)?;
                    if matches!(entry.format, Format::F45cc | Format::F4rcc) && len >= 8 {
                        let proto_idx =
                            u16::from_le_bytes([out[pc + 6], out[pc + 7]]) as u32;
                        let (ret, params) = self.dex.protos.get_proto(
                            self.data,
                            &self.dex.types,
                            &self.dex.strings,
                            proto_idx,
                        )?;
                        let p = format!("({}){}", params.join(""), ret);
                        let new_p = maps.proto(&p)?;
                        if new_p > u16::MAX as u32 {
                            return Err(DexError::Parse("proto idx exceeds u16".into()));
                        }
                        out[pc + 6..pc + 8].copy_from_slice(&(new_p as u16).to_le_bytes());
                    }
                }
                RefKind::MethodProto => {
                    let old = read_ref_idx(&out, pc, entry.format)?;
                    let (ret, params) = self.dex.protos.get_proto(
                        self.data,
                        &self.dex.types,
                        &self.dex.strings,
                        old,
                    )?;
                    let p = format!("({}){}", params.join(""), ret);
                    let new = maps.proto(&p)?;
                    write_ref_idx(&mut out, pc, entry.format, new)?;
                }
                RefKind::CallSite | RefKind::Varies => return Err(DexError::UnsupportedRef),
                RefKind::None => {}
            }
            pc += len;
        }
        Ok(out)
    }

    fn remap_encoded_value(&self, v: &EncodedValue, maps: &PoolMaps) -> Result<EncodedValue> {
        Ok(match v {
            EncodedValue::String(i) => {
                EncodedValue::String(maps.string(&self.dex.get_string(*i)?)?)
            }
            EncodedValue::Type(i) => EncodedValue::Type(maps.ty(&self.dex.get_type(*i)?)?),
            EncodedValue::MethodType(i) => {
                let (ret, params) = self.dex.protos.get_proto(
                    self.data,
                    &self.dex.types,
                    &self.dex.strings,
                    *i,
                )?;
                EncodedValue::MethodType(maps.proto(&format!("({}){}", params.join(""), ret))?)
            }
            EncodedValue::Field(i) => {
                let f = self.dex.get_field_info(*i)?;
                EncodedValue::Field(maps.field(&f.class, &f.name, &f.typ)?)
            }
            EncodedValue::Enum(i) => {
                let f = self.dex.get_field_info(*i)?;
                EncodedValue::Enum(maps.field(&f.class, &f.name, &f.typ)?)
            }
            EncodedValue::Method(i) => {
                let m = self.dex.get_method_info(*i)?;
                EncodedValue::Method(maps.method(&m.class, &m.name, &proto_str(&m))?)
            }
            EncodedValue::MethodHandle(_) => return Err(DexError::UnsupportedRef),
            EncodedValue::Array(arr) => EncodedValue::Array(
                arr.iter()
                    .map(|e| self.remap_encoded_value(e, maps))
                    .collect::<Result<Vec<_>>>()?,
            ),
            EncodedValue::Annotation(a) => {
                EncodedValue::Annotation(self.remap_annotation(a, maps)?)
            }
            other => other.clone(),
        })
    }

    fn remap_annotation(&self, a: &EncodedAnnotation, maps: &PoolMaps) -> Result<EncodedAnnotation> {
        let type_idx = maps.ty(&self.dex.get_type(a.type_idx)?)?;
        let mut elements = Vec::new();
        for (name_idx, val) in &a.elements {
            let ni = maps.string(&self.dex.get_string(*name_idx)?)?;
            elements.push((ni, self.remap_encoded_value(val, maps)?));
        }
        Ok(EncodedAnnotation { type_idx, elements })
    }

    fn remap_annotations(
        &self,
        ann: &AnnotationsDirectory,
        maps: &PoolMaps,
    ) -> Result<AnnotationsDirectory> {
        let class_annotations = ann
            .class_annotations
            .iter()
            .map(|a| {
                Ok(AnnotationItem {
                    visibility: a.visibility,
                    annotation: self.remap_annotation(&a.annotation, maps)?,
                })
            })
            .collect::<Result<Vec<_>>>()?;

        let mut field_annotations = Vec::new();
        for (fidx, set) in &ann.field_annotations {
            let f = self.dex.get_field_info(*fidx)?;
            let new_idx = maps.field(&f.class, &f.name, &f.typ)?;
            let set = set
                .iter()
                .map(|a| {
                    Ok(AnnotationItem {
                        visibility: a.visibility,
                        annotation: self.remap_annotation(&a.annotation, maps)?,
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            field_annotations.push((new_idx, set));
        }
        let mut method_annotations = Vec::new();
        for (midx, set) in &ann.method_annotations {
            let m = self.dex.get_method_info(*midx)?;
            let new_idx = maps.method(&m.class, &m.name, &proto_str(&m))?;
            let set = set
                .iter()
                .map(|a| {
                    Ok(AnnotationItem {
                        visibility: a.visibility,
                        annotation: self.remap_annotation(&a.annotation, maps)?,
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            method_annotations.push((new_idx, set));
        }
        let mut parameter_annotations = Vec::new();
        for (midx, params) in &ann.parameter_annotations {
            let m = self.dex.get_method_info(*midx)?;
            let new_idx = maps.method(&m.class, &m.name, &proto_str(&m))?;
            let mut new_params = Vec::new();
            for set in params {
                let set = set
                    .iter()
                    .map(|a| {
                        Ok(AnnotationItem {
                            visibility: a.visibility,
                            annotation: self.remap_annotation(&a.annotation, maps)?,
                        })
                    })
                    .collect::<Result<Vec<_>>>()?;
                new_params.push(set);
            }
            parameter_annotations.push((new_idx, new_params));
        }
        Ok(AnnotationsDirectory {
            class_annotations,
            field_annotations,
            method_annotations,
            parameter_annotations,
        })
    }
}

fn proto_str(m: &crate::dex::MethodInfo) -> String {
    format!("({}){}", m.params.join(""), m.return_type)
}

fn payload_len(insns: &[u8], pc: usize) -> Result<u32> {
    if pc + 4 > insns.len() {
        return Err(DexError::Truncated("payload".into()));
    }
    let unit0 = u16::from_le_bytes([insns[pc], insns[pc + 1]]);
    match unit0 {
        0x0100 => {
            let size = u16::from_le_bytes([insns[pc + 2], insns[pc + 3]]) as u32;
            Ok((size * 2 + 4) * 2)
        }
        0x0200 => {
            let size = u16::from_le_bytes([insns[pc + 2], insns[pc + 3]]) as u32;
            Ok((size * 4 + 2) * 2)
        }
        0x0300 => {
            if pc + 8 > insns.len() {
                return Err(DexError::Truncated("fill-array-data".into()));
            }
            let elem_width = u16::from_le_bytes([insns[pc + 2], insns[pc + 3]]) as u32;
            let size = u32::from_le_bytes(insns[pc + 4..pc + 8].try_into().unwrap());
            let data_units = (size * elem_width + 1) / 2;
            Ok((data_units + 4) * 2)
        }
        _ => Err(DexError::Parse("not a payload".into())),
    }
}

fn read_ref_idx(insns: &[u8], pc: usize, format: Format) -> Result<u32> {
    match format {
        Format::F31c => {
            if pc + 6 > insns.len() {
                return Err(DexError::Truncated("ref u32".into()));
            }
            Ok(u32::from_le_bytes([
                insns[pc + 2],
                insns[pc + 3],
                insns[pc + 4],
                insns[pc + 5],
            ]))
        }
        _ => {
            if pc + 4 > insns.len() {
                return Err(DexError::Truncated("ref u16".into()));
            }
            Ok(u16::from_le_bytes([insns[pc + 2], insns[pc + 3]]) as u32)
        }
    }
}

fn write_ref_idx(insns: &mut [u8], pc: usize, format: Format, idx: u32) -> Result<()> {
    match format {
        Format::F31c => {
            if pc + 6 > insns.len() {
                return Err(DexError::Truncated("write ref u32".into()));
            }
            insns[pc + 2..pc + 6].copy_from_slice(&idx.to_le_bytes());
        }
        _ => {
            if idx > u16::MAX as u32 {
                return Err(DexError::Parse(format!(
                    "rewritten index {idx} exceeds u16"
                )));
            }
            if pc + 4 > insns.len() {
                return Err(DexError::Truncated("write ref u16".into()));
            }
            insns[pc + 2..pc + 4].copy_from_slice(&(idx as u16).to_le_bytes());
        }
    }
    Ok(())
}

fn rebuild_debug_from_blob(blob: &[u8], maps: &PoolMaps) -> Result<Vec<u8>> {
    if blob.len() < 8 {
        return Ok(Vec::new());
    }
    let mut p = 0usize;
    let line_start = u32::from_le_bytes(blob[p..p + 4].try_into().unwrap());
    p += 4;
    let nparams = u32::from_le_bytes(blob[p..p + 4].try_into().unwrap()) as usize;
    p += 4;
    let mut param_idxs = Vec::with_capacity(nparams);
    for _ in 0..nparams {
        if p >= blob.len() {
            break;
        }
        let tag = blob[p];
        p += 1;
        if tag == 0 {
            param_idxs.push(None);
        } else {
            let n = u32::from_le_bytes(blob[p..p + 4].try_into().unwrap()) as usize;
            p += 4;
            let s = std::str::from_utf8(&blob[p..p + n])
                .map_err(|e| DexError::Parse(e.to_string()))?;
            p += n;
            param_idxs.push(Some(maps.string(s)?));
        }
    }
    let nops = u32::from_le_bytes(blob[p..p + 4].try_into().unwrap()) as usize;
    p += 4;
    let mut ops = Vec::new();
    for _ in 0..nops {
        let reg = u32::from_le_bytes(blob[p..p + 4].try_into().unwrap());
        p += 4;
        let n = u32::from_le_bytes(blob[p..p + 4].try_into().unwrap()) as usize;
        p += 4;
        let name = std::str::from_utf8(&blob[p..p + n]).map_err(|e| DexError::Parse(e.to_string()))?;
        p += n;
        let n = u32::from_le_bytes(blob[p..p + 4].try_into().unwrap()) as usize;
        p += 4;
        let typ = std::str::from_utf8(&blob[p..p + n]).map_err(|e| DexError::Parse(e.to_string()))?;
        p += n;
        ops.push(DebugBuilderOp::StartLocal {
            reg,
            name_idx: maps.string(name)?,
            type_idx: maps.ty(typ)?,
        });
    }
    Ok(build_debug_info(line_start, &param_idxs, &ops))
}
