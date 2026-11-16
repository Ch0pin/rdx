use crate::control_flow;
use crate::dataflow;
use crate::dex::{
    ClassDef, DexFile, EncodedAnnotation, EncodedField, EncodedMethod, EncodedValue,
};
use crate::error::{Error, Result};
use crate::graph::Graph;
use crate::ir::opcode::lower_method_with_resolved;
use crate::types::{
    class_access, descriptor_to_java, descriptor_to_java_enhanced, field_access, method_access,
};
use crate::writer::{
    escape_java_string, format_java_char, format_java_f32, format_java_f64, MemberKind,
    MemberReference, MethodWriter, SourceOutput,
};

pub struct Decompiler<'a> {
    dex: DexFile<'a>,
    include_metadata: bool,
    debug: bool,
}

impl<'a> Decompiler<'a> {
    pub fn parse(dex_bytes: &'a [u8]) -> Result<Self> {
        Ok(Self {
            dex: DexFile::parse(dex_bytes)?,
            include_metadata: true,
            debug: false,
        })
    }

    pub fn parse_owned(dex_bytes: std::sync::Arc<[u8]>) -> Result<Decompiler<'static>> {
        Ok(Decompiler {
            dex: DexFile::parse_owned(dex_bytes)?,
            include_metadata: true,
            debug: false,
        })
    }

    /// Disables DEX annotations in rendered source while retaining member-reference metadata.
    pub const fn without_metadata(mut self) -> Self {
        self.include_metadata = false;
        self
    }

    pub const fn with_debug(mut self, debug: bool) -> Self {
        self.debug = debug;
        self
    }

    pub fn dex(&self) -> &DexFile<'a> {
        &self.dex
    }

    pub fn decompile_class(&self, dalvik_class: &str) -> Result<Option<SourceOutput>> {
        let Some(class) = self.dex.class(dalvik_class) else {
            return Ok(None);
        };
        self.write_class(class).map(Some)
    }

    pub fn decompile_all(&self) -> Result<Vec<(String, SourceOutput)>> {
        self.dex
            .classes()
            .iter()
            .map(|class| {
                Ok((
                    self.dex.class_descriptor(class)?.to_owned(),
                    self.write_class(class)?,
                ))
            })
            .collect()
    }

    /// Decompile one scheduler-owned class range. APK pipeline workers use this
    /// primitive so every DEX shares one global class-worker pool rather than
    /// creating nested pools per DEX.
    pub(crate) fn decompile_class_indices_for_export<F>(
        &self,
        indices: &[usize],
        mut visit: F,
    ) -> Result<usize>
    where
        F: FnMut(&str, SourceOutput) -> Result<()>,
    {
        let all_classes = self.dex.classes();
        for &index in indices {
            let class = all_classes
                .get(index)
                .ok_or_else(|| Error::data(0, "APK class job has an invalid class index"))?;
            let descriptor = self.dex.class_descriptor(class)?;
            let output = self.write_class(class)?;
            visit(descriptor, output)?;
        }
        Ok(indices.len())
    }

    pub fn decompile_selected_parallel<P>(
        &self,
        thread_count: Option<usize>,
        predicate: P,
    ) -> Result<Vec<(String, SourceOutput)>>
    where
        P: Fn(&str) -> bool + Sync,
    {
        let classes = self.dex.classes();
        let mut selected = Vec::new();
        for (idx, class) in classes.iter().enumerate() {
            let descriptor = self.dex.class_descriptor(class)?;
            if predicate(descriptor) {
                selected.push(idx);
            }
        }
        let thread_count = thread_count.unwrap_or(1);
        if thread_count <= 1 || selected.len() <= 1 {
            return selected
                .into_iter()
                .map(|idx| {
                    let class = &classes[idx];
                    Ok((
                        self.dex.class_descriptor(class)?.to_owned(),
                        self.write_class(class)?,
                    ))
                })
                .collect();
        }
        let worker_count = thread_count.min(selected.len());
        let chunk_size = selected.len().div_ceil(worker_count);
        std::thread::scope(|scope| {
            let mut handles = Vec::with_capacity(worker_count);
            for chunk in selected.chunks(chunk_size) {
                handles.push(scope.spawn(move || {
                    chunk
                        .iter()
                        .map(|&idx| {
                            let class = &classes[idx];
                            Ok((
                                self.dex.class_descriptor(class)?.to_owned(),
                                self.write_class(class)?,
                            ))
                        })
                        .collect::<Result<Vec<_>>>()
                }));
            }
            let mut output = Vec::with_capacity(selected.len());
            for handle in handles {
                let chunk = handle
                    .join()
                    .map_err(|_| Error::data(0, "parallel decompiler worker panicked"))??;
                output.extend(chunk);
            }
            Ok(output)
        })
    }

    pub fn decompile_all_parallel(
        &self,
        thread_count: usize,
    ) -> Result<Vec<(String, SourceOutput)>> {
        let classes = self.dex.classes();
        if thread_count <= 1 || classes.len() <= 1 {
            return self.decompile_all();
        }

        let worker_count = thread_count.min(classes.len());
        let chunk_size = classes.len().div_ceil(worker_count);
        std::thread::scope(|scope| {
            let mut handles = Vec::with_capacity(worker_count);
            for chunk in classes.chunks(chunk_size) {
                handles.push(scope.spawn(move || {
                    chunk
                        .iter()
                        .map(|class| {
                            Ok((
                                self.dex.class_descriptor(class)?.to_owned(),
                                self.write_class(class)?,
                            ))
                        })
                        .collect::<Result<Vec<_>>>()
                }));
            }

            let mut output = Vec::with_capacity(classes.len());
            for handle in handles {
                let chunk = handle
                    .join()
                    .map_err(|_| Error::data(0, "parallel decompiler worker panicked"))??;
                output.extend(chunk);
            }
            Ok(output)
        })
    }

    fn write_class(&self, class: &ClassDef) -> Result<SourceOutput> {
        self.write_class_into(SourceOutput::default(), class)
    }

    fn write_class_into(
        &self,
        mut output: SourceOutput,
        class: &ClassDef,
    ) -> Result<SourceOutput> {
        let descriptor = self.dex.class_descriptor(class)?;
        let (package, name) = class_parts(descriptor);
        if !package.is_empty() {
            output.source.push_str("package ");
            output.source.push_str(&package);
            output.source.push_str(";\n");
        }

        let class_signature = if self.include_metadata {
            let class_anns = self.dex.class_annotations(class)?;
            self.extract_class_signature(&class_anns)
        } else {
            None
        };

        let interface = class.access_flags & 0x200 != 0;
        let access_flags = if interface {
            class.access_flags & !0x400
        } else {
            class.access_flags
        };
        output
            .source
            .push_str(&class_access(access_flags).join(" "));
        output.source.push(' ');
        if !interface {
            output.source.push_str("class ");
        }
        output.source.push_str(&name);
        if let Some((ref type_params, _, _)) = class_signature
            && !type_params.is_empty()
        {
            output.source.push('<');
            output.source.push_str(&type_params.join(", "));
            output.source.push('>');
        }

        if let Some(superclass) = self.dex.class_superclass(class)?
            && superclass != "Ljava/lang/Object;"
        {
            output.source.push_str(" extends ");
            if let Some((_, Some(ref generic_super), _)) = class_signature {
                output.source.push_str(generic_super);
            } else {
                output
                    .source
                    .push_str(&superclass[1..superclass.len() - 1].replace('/', "."));
            }
        }
        let interfaces = self.dex.class_interfaces(class)?;
        if !interfaces.is_empty() {
            output.source.push_str(" implements ");
            for (index, interface) in interfaces.iter().enumerate() {
                if index != 0 {
                    output.source.push_str(", ");
                }
                if let Some((_, _, ref generic_ifaces)) = class_signature
                    && generic_ifaces.len() == interfaces.len()
                    && let Some(generic_iface) = generic_ifaces.get(index)
                {
                    output.source.push_str(generic_iface);
                } else {
                    output
                        .source
                        .push_str(&interface[1..interface.len() - 1].replace('/', "."));
                }
            }
        }
        output.source.push_str(" {\n");

        let mut synthetic_members = std::collections::HashSet::new();
        let field_annotations = if self.include_metadata {
            Some(self.dex.field_annotations(class)?)
        } else {
            None
        };
        for field in class.fields() {
            if field.access_flags & 0x1000 != 0
                && let Ok(resolved) = self.dex.field(field.field_idx)
            {
                synthetic_members.insert((
                    MemberKind::Field,
                    resolved.class.to_owned(),
                    resolved.name.to_owned(),
                    resolved.ty.to_owned(),
                ));
            }
            let annotations = field_annotations
                .as_ref()
                .and_then(|annotations| annotations.get(&field.field_idx))
                .map(Vec::as_slice)
                .unwrap_or_default();
            self.write_field(&mut output, field, annotations)?;
        }
        let method_annotations = if self.include_metadata {
            Some(self.dex.method_annotations(class)?)
        } else {
            None
        };
        for method in class.methods() {
            if (method.access_flags & 0x1040 != 0)
                && let Ok(resolved) = self.dex.method(method.method_idx)
            {
                synthetic_members.insert((
                    MemberKind::Method,
                     descriptor.to_owned(),
                    resolved.name.to_owned(),
                    resolved.descriptor.to_owned(),
                ));
            }
            let annotations = method_annotations
                .as_ref()
                .and_then(|annotations| annotations.get(&method.method_idx))
                .map(Vec::as_slice)
                .unwrap_or_default();
            output = self.write_method(output, descriptor, &name, method, annotations)?;
        }
        if !synthetic_members.is_empty() {
            for r in &mut output.member_references {
                if !r.synthetic
                    && synthetic_members.contains(&(
                        r.kind.clone(),
                        r.class.clone(),
                        r.name.clone(),
                        r.descriptor.clone(),
                    ))
                {
                    r.synthetic = true;
                }
            }
        }
        output.source.push_str("}\n");
        Ok(output)
    }

    fn write_field(
        &self,
        output: &mut SourceOutput,
        field: &EncodedField,
        annotations: &[EncodedAnnotation],
    ) -> Result<()> {
        let resolved = self.dex.field(field.field_idx)?;
        output.source.push_str("    ");
        let access = field_access(field.access_flags);
        if !access.is_empty() {
            output.source.push_str(&access.join(" "));
            output.source.push(' ');
        }
        if let Some(generic_ty) = self
            .include_metadata
            .then(|| self.extract_field_signature(annotations))
            .flatten()
        {
            output.source.push_str(&generic_ty);
        } else if self.include_metadata {
            output.source.push_str(&descriptor_to_java_enhanced(resolved.ty));
        } else {
            output.source.push_str(&descriptor_to_java(resolved.ty));
        }
        output.source.push(' ');
        let start = output.source.len();
        output.source.push_str(resolved.name);
        output.member_references.push(MemberReference {
            start,
            end: output.source.len(),
            kind: MemberKind::Field,
            class: resolved.class.to_owned(),
            name: resolved.name.to_owned(),
            descriptor: resolved.ty.to_owned(),
            declaration: true,
            synthetic: field.access_flags & 0x1000 != 0,
        });
        if let Some(value) = &field.initial_value {
            output.source.push_str(" = ");
            output.source.push_str(&self.encoded_value(value, resolved.ty)?);
        }
        output.source.push_str(";\n");
        Ok(())
    }

    pub fn write_method(
        &self,
        mut output: SourceOutput,
        class_descriptor: &str,
        class_name: &str,
        method: &EncodedMethod,
        annotations: &[EncodedAnnotation],
    ) -> Result<SourceOutput> {
        let resolved = self.dex.method(method.method_idx)?;
        if self.debug {
            eprintln!(
                "[radguard-debug] method {}->{}{}",
                class_descriptor,
                resolved.name,
                resolved.descriptor
            );
        }
        let return_type = resolved.return_type;
        let access = method_access(method.access_flags);
        let constructor = access.iter().any(|value| value == "constructor");
        let modifiers: Vec<_> = access
            .iter()
            .filter(|value| value.as_str() != "constructor")
            .map(String::as_str)
            .collect();
        for annotation in annotations {
            output.source.push_str("\n    ");
            output.source.push_str(&self.annotation(annotation)?);
        }
        output.source.push_str("\n    ");
        if !modifiers.is_empty() {
            output.source.push_str(&modifiers.join(" "));
            output.source.push(' ');
        }
        let generic_signature = if self.include_metadata {
            self.extract_method_signature(annotations, resolved.parameter_type_indices.len())
        } else {
            None
        };
        if !constructor {
            if let Some((_, ref ret_ty)) = generic_signature {
                output.source.push_str(ret_ty);
            } else {
                output.source.push_str(&descriptor_to_java(return_type));
            }
            output.source.push(' ');
        }
        let displayed_name = if constructor {
            class_name
        } else {
            resolved.name
        };
        let name_start = output.source.len();
        output.source.push_str(displayed_name);
        output.member_references.push(MemberReference {
            start: name_start,
            end: output.source.len(),
            kind: MemberKind::Method,
            class: class_descriptor.to_owned(),
            name: resolved.name.to_owned(),
            descriptor: resolved.descriptor.to_owned(),
            declaration: true,
            synthetic: method.access_flags & 0x1040 != 0,
        });

        let lowered =
            lower_method_with_resolved(&self.dex, method, resolved, self.include_metadata)?;
        let mut graph = lowered.map(Graph::from_lowered);
        let parameter_registers = graph
            .as_ref()
            .map(|graph| {
                let mut parameters: Vec<_> = graph
                    .variables
                    .parameter_ids()
                    .into_iter()
                    .filter(|variable| !graph.variables.is_this(*variable))
                    .collect();
                parameters.sort();
                parameters
            })
            .unwrap_or_default();
        output.source.push('(');
        // Methods without code still have prototype parameters, but no registers.
        for (index, parameter_type_index) in resolved
            .parameter_type_indices
            .iter()
            .enumerate()
        {
            let parameter_type = self.dex.type_descriptor(*parameter_type_index)?;
            if index != 0 {
                output.source.push_str(", ");
            }
            if let Some((ref param_types, _)) = generic_signature
                && let Some(generic_param) = param_types.get(index)
            {
                output.source.push_str(generic_param);
            } else {
                output
                    .source
                    .push_str(&descriptor_to_java(parameter_type));
            }
            output.source.push(' ');
            output.source.push('p');
            let name = parameter_registers.get(index).and_then(|parameter| {
                graph
                    .as_ref()
                    .and_then(|graph| graph.variables.name(*parameter))
            });
            if let Some(name) = name {
                output.source.push_str(name);
            } else {
                output.source.push_str(&index.to_string());
            }
        }
        output.source.push(')');

        let Some(graph) = graph.as_mut() else {
            output.source.push_str(";\n");
            return Ok(output);
        };
        if self.debug {
            graph.debug_dump("lowered");
        }
        if self.include_metadata {
            dataflow::optimize_for_decompilation_enhanced(graph, return_type);
        } else {
            dataflow::optimize_for_decompilation(graph);
        }
        graph.split_condition_nodes();
        graph.simplify();
        graph.compute_rpo();
        if self.debug {
            graph.debug_dump("after-dataflow-and-simplify");
            control_flow::identify_structures_debug(graph);
            graph.debug_dump("after-structure");
        } else {
            control_flow::identify_structures(graph);
        }
        Ok(
            MethodWriter::new_moving_conditions(
                graph,
                class_descriptor,
                constructor,
                output,
                self.debug,
                self.include_metadata,
            )
            .write_body(),
        )
    }

    fn encoded_value(&self, value: &EncodedValue, field_type: &str) -> Result<String> {
        let rendered = match value {
            EncodedValue::Byte(value) => {
                if field_type == "B" {
                    signed_hex(i64::from(*value))
                } else {
                    value.to_string()
                }
            }
            EncodedValue::Short(value) => value.to_string(),
            EncodedValue::Char(value) => {
                if self.include_metadata && field_type == "C" {
                    format_java_char(*value)
                } else {
                    value.to_string()
                }
            }
            EncodedValue::Int(value) => value.to_string(),
            EncodedValue::Long(value) => {
                if self.include_metadata {
                    format!("{value}L")
                } else {
                    value.to_string()
                }
            }
            EncodedValue::Float(bits) => {
                let value = f32::from_bits(*bits);
                if self.include_metadata {
                    format_java_f32(value)
                } else {
                    value.to_string()
                }
            }
            EncodedValue::Double(bits) => {
                let value = f64::from_bits(*bits);
                if self.include_metadata {
                    format_java_f64(value)
                } else {
                    value.to_string()
                }
            }
            EncodedValue::String(index) => escape_java_string(self.dex.string(*index)?),
            EncodedValue::Type(index) => {
                format!("{}.class", descriptor_to_java(self.dex.type_descriptor(*index)?))
            }
            EncodedValue::Field(index) | EncodedValue::Enum(index) => {
                let field = self.dex.field(*index)?;
                format!("{}.{}", descriptor_to_java(field.class), field.name)
            }
            EncodedValue::Method(index) => {
                let method = self.dex.method(*index)?;
                format!("{}.{}", descriptor_to_java(method.class), method.name)
            }
            EncodedValue::MethodType(index) => self.dex.proto_descriptor(*index)?,
            EncodedValue::MethodHandle(index) => index.to_string(),
            EncodedValue::Array(values) => {
                let values = values
                    .iter()
                    .map(|value| self.encoded_value(value, field_type))
                    .collect::<Result<Vec<_>>>()?;
                format!("{{{}}}", values.join(", "))
            }
            EncodedValue::Annotation(annotation) => {
                let name = descriptor_to_java(self.dex.type_descriptor(annotation.type_idx)?);
                let mut elements = Vec::with_capacity(annotation.elements.len());
                for element in &annotation.elements {
                    elements.push(format!(
                        "{}={}",
                        self.dex.string(element.name_idx)?,
                        self.encoded_value(&element.value, field_type)?
                    ));
                }
                if elements.is_empty() {
                    format!("@{name}")
                } else {
                    format!("@{name}({})", elements.join(", "))
                }
            }
            EncodedValue::Null if field_type == "Ljava/lang/String;" && !self.include_metadata => {
                "\"\"".to_owned()
            }
            EncodedValue::Null => {
                if self.include_metadata {
                    "null".to_owned()
                } else {
                    "None".to_owned()
                }
            }
            EncodedValue::Boolean(value) => {
                if self.include_metadata {
                    if *value {
                        "true".to_owned()
                    } else {
                        "false".to_owned()
                    }
                } else if *value {
                    "True".to_owned()
                } else {
                    "False".to_owned()
                }
            }
        };
        Ok(rendered)
    }

    fn annotation(&self, annotation: &EncodedAnnotation) -> Result<String> {
        let name = annotation_type_name(self.dex.type_descriptor(annotation.type_idx)?);
        if annotation.elements.is_empty() {
            return Ok(format!("@{name}"));
        }
        let mut elements = Vec::with_capacity(annotation.elements.len());
        for element in &annotation.elements {
            elements.push(format!(
                "{}={}",
                self.dex.string(element.name_idx)?,
                self.annotation_value(&element.value)?
            ));
        }
        Ok(format!("@{name}({})", elements.join(", ")))
    }

    fn annotation_value(&self, value: &EncodedValue) -> Result<String> {
        Ok(match value {
            EncodedValue::String(index) => escape_java_string(self.dex.string(*index)?),
            EncodedValue::Type(index) => format!(
                "{}.class",
                annotation_type_name(self.dex.type_descriptor(*index)?)
            ),
            EncodedValue::Array(values) => {
                let values = values
                    .iter()
                    .map(|value| self.annotation_value(value))
                    .collect::<Result<Vec<_>>>()?;
                format!("{{{}}}", values.join(", "))
            }
            EncodedValue::Annotation(annotation) => self.annotation(annotation)?,
            EncodedValue::Boolean(value) => {
                if *value {
                    "true".to_owned()
                } else {
                    "false".to_owned()
                }
            }
            EncodedValue::Null => "null".to_owned(),
            _ => self.encoded_value(value, "")?,
        })
    }

    fn extract_signature_string(&self, annotations: &[EncodedAnnotation]) -> Option<String> {
        for annotation in annotations {
            let ty = self.dex.type_descriptor(annotation.type_idx).ok()?;
            if ty != "Ldalvik/annotation/Signature;" {
                continue;
            }
            for element in &annotation.elements {
                if self.dex.string(element.name_idx).ok()? != "value" {
                    continue;
                }
                let EncodedValue::Array(values) = &element.value else {
                    continue;
                };
                let mut combined = String::new();
                for val in values {
                    let EncodedValue::String(idx) = val else {
                        return None;
                    };
                    combined.push_str(self.dex.string(*idx).ok()?);
                }
                return Some(combined);
            }
        }
        None
    }

    fn extract_method_signature(
        &self,
        annotations: &[EncodedAnnotation],
        expected_params: usize,
    ) -> Option<(Vec<String>, String)> {
        let combined = self.extract_signature_string(annotations)?;
        let (params, ret) = parse_dalvik_method_signature(&combined)?;
        (params.len() == expected_params).then_some((params, ret))
    }

    fn extract_field_signature(&self, annotations: &[EncodedAnnotation]) -> Option<String> {
        let combined = self.extract_signature_string(annotations)?;
        let mut chars = combined.chars().peekable();
        parse_signature_type(&mut chars)
    }

    fn extract_class_signature(
        &self,
        annotations: &[EncodedAnnotation],
    ) -> Option<(Vec<String>, Option<String>, Vec<String>)> {
        let combined = self.extract_signature_string(annotations)?;
        parse_dalvik_class_signature(&combined)
    }
}

fn parse_type_parameters(
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
) -> Option<Vec<String>> {
    if chars.peek() != Some(&'<') {
        return Some(Vec::new());
    }
    chars.next();
    let mut params = Vec::new();
    while chars.peek().is_some_and(|&c| c != '>') {
        let mut ident = String::new();
        while let Some(&c) = chars.peek() {
            if c == ':' || c == '>' {
                break;
            }
            ident.push(c);
            chars.next();
        }
        if ident.is_empty() {
            return None;
        }
        let mut bounds = Vec::new();
        while chars.peek() == Some(&':') {
            chars.next();
            if chars.peek() == Some(&':') || chars.peek() == Some(&'>') {
                continue;
            }
            let bound = parse_signature_type(chars)?;
            if bound != "Object" && bound != "java.lang.Object" {
                bounds.push(bound);
            }
        }
        if bounds.is_empty() {
            params.push(ident);
        } else {
            params.push(format!("{ident} extends {}", bounds.join(" & ")));
        }
    }
    if chars.next() != Some('>') {
        return None;
    }
    Some(params)
}

fn parse_dalvik_class_signature(
    sig: &str,
) -> Option<(Vec<String>, Option<String>, Vec<String>)> {
    let mut chars = sig.chars().peekable();
    let type_params = parse_type_parameters(&mut chars)?;
    let super_ty = parse_signature_type(&mut chars)?;
    let super_ty = (super_ty != "Object" && super_ty != "java.lang.Object").then_some(super_ty);
    let mut interfaces = Vec::new();
    while chars.peek().is_some() {
        interfaces.push(parse_signature_type(&mut chars)?);
    }
    Some((type_params, super_ty, interfaces))
}

fn parse_dalvik_method_signature(sig: &str) -> Option<(Vec<String>, String)> {
    let mut chars = sig.chars().peekable();
    if chars.peek() == Some(&'<') {
        let mut depth = 0;
        for ch in chars.by_ref() {
            match ch {
                '<' => depth += 1,
                '>' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                _ => {}
            }
        }
    }
    if chars.next() != Some('(') {
        return None;
    }
    let mut params = Vec::new();
    while chars.peek().is_some_and(|&c| c != ')') {
        params.push(parse_signature_type(&mut chars)?);
    }
    if chars.next() != Some(')') {
        return None;
    }
    let ret = parse_signature_type(&mut chars)?;
    Some((params, ret))
}

fn parse_signature_type(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> Option<String> {
    match chars.next()? {
        'V' => Some("void".to_owned()),
        'Z' => Some("boolean".to_owned()),
        'B' => Some("byte".to_owned()),
        'S' => Some("short".to_owned()),
        'C' => Some("char".to_owned()),
        'I' => Some("int".to_owned()),
        'J' => Some("long".to_owned()),
        'F' => Some("float".to_owned()),
        'D' => Some("double".to_owned()),
        '[' => {
            let elem = parse_signature_type(chars)?;
            Some(format!("{elem}[]"))
        }
        'T' => {
            let mut name = String::new();
            for ch in chars.by_ref() {
                if ch == ';' {
                    return (!name.is_empty()).then_some(name);
                }
                name.push(ch);
            }
            None
        }
        'L' => {
            let mut raw = String::new();
            let mut type_args = Vec::new();
            loop {
                match chars.next()? {
                    ';' => break,
                    '<' => {
                        while chars.peek().is_some_and(|&c| c != '>') {
                            type_args.push(parse_signature_type_argument(chars)?);
                        }
                        if chars.next() != Some('>') {
                            return None;
                        }
                    }
                    '.' => {
                        raw.push('$');
                    }
                    ch => raw.push(ch),
                }
            }
            let base = descriptor_to_java_enhanced(&format!("L{raw};"));
            if type_args.is_empty() {
                Some(base)
            } else {
                Some(format!("{base}<{}>", type_args.join(", ")))
            }
        }
        _ => None,
    }
}

fn parse_signature_type_argument(
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
) -> Option<String> {
    match chars.peek()? {
        '*' => {
            chars.next();
            Some("?".to_owned())
        }
        '+' => {
            chars.next();
            let bound = parse_signature_type(chars)?;
            Some(format!("? extends {bound}"))
        }
        '-' => {
            chars.next();
            let bound = parse_signature_type(chars)?;
            Some(format!("? super {bound}"))
        }
        _ => parse_signature_type(chars),
    }
}

pub fn decompile_dex_bytes(
    dex_bytes: &[u8],
    dalvik_class_fmt: &str,
) -> Result<SourceOutput> {
    let decompiler = Decompiler::parse(dex_bytes)?;
    Ok(match decompiler.decompile_class(dalvik_class_fmt)? {
        Some(output) => output,
        None => SourceOutput {
            source: format!(
                "Error: Class {dalvik_class_fmt} not found in the reconstructed DEX."
            ),
            member_references: Vec::new(),
        },
    })
}

fn class_parts(descriptor: &str) -> (String, String) {
    let body = descriptor
        .strip_prefix('L')
        .and_then(|value| value.strip_suffix(';'))
        .unwrap_or(descriptor);
    if let Some((package, name)) = body.rsplit_once('/') {
        (package.replace('/', "."), name.to_owned())
    } else {
        (String::new(), body.to_owned())
    }
}

fn signed_hex(value: i64) -> String {
    if value < 0 {
        format!("-0x{:x}", value.unsigned_abs())
    } else {
        format!("0x{value:x}")
    }
}

fn annotation_type_name(descriptor: &str) -> String {
    descriptor
        .strip_prefix('L')
        .and_then(|value| value.strip_suffix(';'))
        .unwrap_or(descriptor)
        .replace('/', ".")
        .rsplit_once('.')
        .map(|(_, name)| name)
        .unwrap_or(descriptor)
        .replace('$', ".")
}
