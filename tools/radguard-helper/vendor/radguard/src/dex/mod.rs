use std::collections::HashMap;
use std::sync::Arc;

use crate::error::{Error, Result};

pub mod instruction;

pub const NO_INDEX: u32 = u32::MAX;
const HEADER_SIZE: usize = 0x70;
const ENDIAN_CONSTANT: u32 = 0x1234_5678;
const MAX_ENCODED_VALUE_DEPTH: usize = 64;

#[derive(Clone, Debug)]
pub struct Header {
    pub version: u16,
    pub checksum: u32,
    pub file_size: u32,
    pub map_off: u32,
    pub string_ids_size: u32,
    pub string_ids_off: u32,
    pub type_ids_size: u32,
    pub type_ids_off: u32,
    pub proto_ids_size: u32,
    pub proto_ids_off: u32,
    pub field_ids_size: u32,
    pub field_ids_off: u32,
    pub method_ids_size: u32,
    pub method_ids_off: u32,
    pub class_defs_size: u32,
    pub class_defs_off: u32,
    pub data_size: u32,
    pub data_off: u32,
}

#[derive(Clone, Copy, Debug)]
pub struct ProtoId {
    pub shorty_idx: u32,
    pub return_type_idx: u32,
    pub parameters_off: u32,
}

#[derive(Clone, Copy, Debug)]
pub struct FieldId {
    pub class_idx: u16,
    pub type_idx: u16,
    pub name_idx: u32,
}

#[derive(Clone, Copy, Debug)]
pub struct MethodId {
    pub class_idx: u16,
    pub proto_idx: u16,
    pub name_idx: u32,
}

#[derive(Clone, Debug)]
pub struct EncodedField {
    pub field_idx: u32,
    pub access_flags: u32,
    pub initial_value: Option<EncodedValue>,
}

#[derive(Clone, Copy, Debug)]
pub struct EncodedMethod {
    pub method_idx: u32,
    pub access_flags: u32,
    pub code_off: u32,
}

#[derive(Clone, Debug)]
pub struct ClassDef {
    pub class_idx: u32,
    pub access_flags: u32,
    pub superclass_idx: Option<u32>,
    pub interfaces: Vec<u32>,
    pub source_file_idx: Option<u32>,
    pub annotations_off: u32,
    pub class_data_off: u32,
    pub static_values_off: u32,
    pub static_fields: Vec<EncodedField>,
    pub instance_fields: Vec<EncodedField>,
    pub direct_methods: Vec<EncodedMethod>,
    pub virtual_methods: Vec<EncodedMethod>,
}

impl ClassDef {
    pub fn fields(&self) -> impl Iterator<Item = &EncodedField> {
        self.static_fields.iter().chain(&self.instance_fields)
    }

    pub fn methods(&self) -> impl Iterator<Item = &EncodedMethod> {
        self.direct_methods.iter().chain(&self.virtual_methods)
    }
}

#[derive(Clone, Debug)]
pub enum EncodedValue {
    Byte(i8),
    Short(i16),
    Char(u16),
    Int(i64),
    Long(i64),
    Float(u32),
    Double(u64),
    MethodType(u32),
    MethodHandle(u32),
    String(u32),
    Type(u32),
    Field(u32),
    Method(u32),
    Enum(u32),
    Array(Vec<EncodedValue>),
    Annotation(EncodedAnnotation),
    Null,
    Boolean(bool),
}

#[derive(Clone, Debug)]
pub struct EncodedAnnotation {
    pub type_idx: u32,
    pub elements: Vec<AnnotationElement>,
}

#[derive(Clone, Debug)]
pub struct AnnotationElement {
    pub name_idx: u32,
    pub value: EncodedValue,
}

#[derive(Clone, Copy, Debug)]
pub struct TryItem {
    pub start_addr: u32,
    pub insn_count: u16,
    pub handler_off: u16,
}

#[derive(Clone, Debug)]
pub struct CatchHandler {
    pub offset: u32,
    pub handlers: Vec<(u32, u32)>,
    pub catch_all_addr: Option<u32>,
}

#[derive(Debug)]
pub struct CodeItem<'a> {
    pub registers_size: u16,
    pub ins_size: u16,
    pub outs_size: u16,
    pub debug_info_off: u32,
    pub insns_size: u32,
    insns: &'a [u8],
    pub tries: Vec<TryItem>,
    pub handlers: Vec<CatchHandler>,
}

impl CodeItem<'_> {
    pub fn instruction_units(&self) -> impl ExactSizeIterator<Item = u16> + '_ {
        self.insns
            .as_chunks::<2>()
            .0
            .iter()
            .copied()
            .map(u16::from_le_bytes)
    }

    pub fn instruction_bytes(&self) -> &[u8] {
        self.insns
    }

    pub fn decode(&self) -> Result<Vec<instruction::DecodedInstruction>> {
        instruction::decode(self)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ResolvedField<'a> {
    pub class: &'a str,
    pub ty: &'a str,
    pub name: &'a str,
}

#[derive(Clone, Copy, Debug)]
pub struct ResolvedMethod<'a> {
    pub class: &'a str,
    pub name: &'a str,
    pub descriptor: &'a str,
    pub parameter_type_indices: &'a [u32],
    pub return_type: &'a str,
}

#[derive(Debug)]
struct ProtoMetadata {
    descriptor: String,
    parameter_type_indices: Vec<u32>,
}

#[derive(Clone, Debug)]
pub(crate) struct FieldMetadata {
    pub class: Arc<str>,
    pub ty: Arc<str>,
    pub name: Arc<str>,
}

#[derive(Debug)]
enum DexBytes<'a> {
    Borrowed(&'a [u8]),
    Shared(Arc<[u8]>),
}

impl DexBytes<'_> {
    fn as_slice(&self) -> &[u8] {
        match self {
            Self::Borrowed(data) => data,
            Self::Shared(data) => data,
        }
    }
}

#[derive(Debug)]
pub struct DexFile<'a> {
    data: DexBytes<'a>,
    pub header: Header,
    strings: Vec<String>,
    types: Vec<u32>,
    protos: Vec<ProtoId>,
    proto_metadata: Vec<ProtoMetadata>,
    fields: Vec<FieldId>,
    field_metadata: Vec<FieldMetadata>,
    methods: Vec<MethodId>,
    classes: Vec<ClassDef>,
    class_lookup: HashMap<String, usize>,
}

impl<'a> DexFile<'a> {
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        let header = Header::parse(data)?;
        let strings = parse_strings(data, &header)?;
        let types = parse_type_ids(data, &header, strings.len())?;
        let protos = parse_proto_ids(data, &header, types.len(), strings.len())?;
        let proto_metadata = protos
            .iter()
            .map(|proto| build_proto_metadata(data, proto, &strings, &types))
            .collect::<Result<Vec<_>>>()?;
        let fields = parse_field_ids(data, &header, types.len(), strings.len())?;
        let type_descriptors: Vec<Arc<str>> = types
            .iter()
            .map(|index| Arc::from(strings[*index as usize].as_str()))
            .collect();
        let field_metadata = fields
            .iter()
            .map(|field| FieldMetadata {
                class: Arc::clone(&type_descriptors[usize::from(field.class_idx)]),
                ty: Arc::clone(&type_descriptors[usize::from(field.type_idx)]),
                name: Arc::from(strings[field.name_idx as usize].as_str()),
            })
            .collect();
        let methods = parse_method_ids(data, &header, types.len(), protos.len(), strings.len())?;
        let mut classes = parse_class_defs(data, &header, types.len(), strings.len())?;

        for class in &mut classes {
            if class.class_data_off != 0 {
                let parsed =
                    parse_class_data(data, class.class_data_off, fields.len(), methods.len())?;
                class.static_fields = parsed.static_fields;
                class.instance_fields = parsed.instance_fields;
                class.direct_methods = parsed.direct_methods;
                class.virtual_methods = parsed.virtual_methods;
            }
            if class.static_values_off != 0 {
                let mut cursor = Cursor::at(data, class.static_values_off as usize)?;
                let values = parse_encoded_array(&mut cursor, 0)?;
                if values.len() > class.static_fields.len() {
                    return Err(Error::data(
                        class.static_values_off as usize,
                        "static value count exceeds static field count",
                    ));
                }
                for (field, value) in class.static_fields.iter_mut().zip(values) {
                    field.initial_value = Some(value);
                }
            }
        }

        let mut class_lookup = HashMap::with_capacity(classes.len());
        for (index, class) in classes.iter().enumerate() {
            let descriptor = resolve_type(&strings, &types, class.class_idx)?;
            class_lookup.entry(descriptor.to_owned()).or_insert(index);
        }

        Ok(Self {
            data: DexBytes::Borrowed(data),
            header,
            strings,
            types,
            protos,
            proto_metadata,
            fields,
            field_metadata,
            methods,
            classes,
            class_lookup,
        })
    }

    pub fn strings(&self) -> &[String] {
        &self.strings
    }

    pub fn type_ids(&self) -> &[u32] {
        &self.types
    }

    pub fn protos(&self) -> &[ProtoId] {
        &self.protos
    }

    pub fn field_ids(&self) -> &[FieldId] {
        &self.fields
    }

    pub fn method_ids(&self) -> &[MethodId] {
        &self.methods
    }

    pub fn classes(&self) -> &[ClassDef] {
        &self.classes
    }

    pub fn string(&self, index: u32) -> Result<&str> {
        self.strings
            .get(index as usize)
            .map(String::as_str)
            .ok_or(Error::InvalidIndex {
                table: "string",
                index,
                len: self.strings.len(),
            })
    }

    pub fn type_descriptor(&self, index: u32) -> Result<&str> {
        resolve_type(&self.strings, &self.types, index)
    }

    pub fn class_descriptor(&self, class: &ClassDef) -> Result<&str> {
        self.type_descriptor(class.class_idx)
    }

    pub fn class(&self, descriptor: &str) -> Option<&ClassDef> {
        self.class_lookup
            .get(descriptor)
            .and_then(|index| self.classes.get(*index))
    }

    pub fn class_superclass(&self, class: &ClassDef) -> Result<Option<&str>> {
        class
            .superclass_idx
            .map(|index| self.type_descriptor(index))
            .transpose()
    }

    pub fn class_interfaces(&self, class: &ClassDef) -> Result<Vec<&str>> {
        class
            .interfaces
            .iter()
            .map(|index| self.type_descriptor(*index))
            .collect()
    }

    pub fn field(&self, index: u32) -> Result<ResolvedField<'_>> {
        let field = self
            .field_metadata
            .get(index as usize)
            .ok_or(Error::InvalidIndex {
                table: "field",
                index,
                len: self.fields.len(),
            })?;
        Ok(ResolvedField {
            class: field.class.as_ref(),
            ty: field.ty.as_ref(),
            name: field.name.as_ref(),
        })
    }

    pub(crate) fn field_metadata(&self, index: u32) -> Result<FieldMetadata> {
        self.field_metadata
            .get(index as usize)
            .cloned()
            .ok_or(Error::InvalidIndex {
                table: "field",
                index,
                len: self.fields.len(),
            })
    }

    pub fn method(&self, index: u32) -> Result<ResolvedMethod<'_>> {
        let method = self
            .methods
            .get(index as usize)
            .ok_or(Error::InvalidIndex {
                table: "method",
                index,
                len: self.methods.len(),
            })?;
        let proto_index = u32::from(method.proto_idx);
        let proto = self
            .protos
            .get(proto_index as usize)
            .ok_or(Error::InvalidIndex {
                table: "proto",
                index: proto_index,
                len: self.protos.len(),
            })?;
        let metadata =
            self.proto_metadata
                .get(proto_index as usize)
                .ok_or(Error::InvalidIndex {
                    table: "proto metadata",
                    index: proto_index,
                    len: self.proto_metadata.len(),
                })?;
        Ok(ResolvedMethod {
            class: self.type_descriptor(method.class_idx.into())?,
            name: self.string(method.name_idx)?,
            descriptor: &metadata.descriptor,
            parameter_type_indices: &metadata.parameter_type_indices,
            return_type: self.type_descriptor(proto.return_type_idx)?,
        })
    }

    pub fn proto_descriptor(&self, index: u32) -> Result<String> {
        self.proto_descriptor_ref(index).map(str::to_owned)
    }

    fn proto_descriptor_ref(&self, index: u32) -> Result<&str> {
        self.proto_metadata
            .get(index as usize)
            .map(|metadata| metadata.descriptor.as_str())
            .ok_or(Error::InvalidIndex {
                table: "proto",
                index,
                len: self.proto_metadata.len(),
            })
    }

    pub fn code(&self, method: &EncodedMethod) -> Result<Option<CodeItem<'_>>> {
        if method.code_off == 0 {
            return Ok(None);
        }
        parse_code_item(self.data.as_slice(), method.code_off)
    }

    pub fn debug_parameter_names(
        &self,
        debug_info_off: u32,
        param_count: usize,
    ) -> Result<Vec<Option<&str>>> {
        if debug_info_off == 0 || param_count == 0 {
            return Ok(Vec::new());
        }
        let mut cursor = Cursor::at(self.data.as_slice(), debug_info_off as usize)?;
        let _line_start = cursor.uleb128()?;
        let parameters_size = cursor.uleb128()? as usize;
        let count = parameters_size.min(param_count);
        let mut names = Vec::with_capacity(count);
        for _ in 0..count {
            let idx_plus_one = cursor.uleb128()?;
            if idx_plus_one == 0 {
                names.push(None);
            } else {
                let idx = idx_plus_one - 1;
                names.push(self.string(idx).ok().filter(|s| !s.is_empty()));
            }
        }
        Ok(names)
    }

    pub fn class_annotations(&self, class: &ClassDef) -> Result<Vec<EncodedAnnotation>> {
        if class.annotations_off == 0 {
            return Ok(Vec::new());
        }
        let directory = class.annotations_off as usize;
        let class_annotations_off = read_u32(self.data.as_slice(), directory)?;
        parse_annotation_set(self.data.as_slice(), class_annotations_off)
    }

    pub fn field_annotations(
        &self,
        class: &ClassDef,
    ) -> Result<HashMap<u32, Vec<EncodedAnnotation>>> {
        let mut annotations = HashMap::new();
        if class.annotations_off == 0 {
            return Ok(annotations);
        }
        let directory = class.annotations_off as usize;
        let fields_size = read_u32(self.data.as_slice(), directory + 4)? as usize;
        let field_bytes = fields_size
            .checked_mul(8)
            .ok_or_else(|| Error::data(directory, "field annotation table size overflow"))?;
        let field_start = directory
            .checked_add(16)
            .ok_or_else(|| Error::data(directory, "annotation directory offset overflow"))?;
        require_range(self.data.as_slice(), field_start, field_bytes)?;
        for index in 0..fields_size {
            let entry = field_start + index * 8;
            let field_idx = read_u32(self.data.as_slice(), entry)?;
            check_index("field", field_idx, self.fields.len())?;
            let set_off = read_u32(self.data.as_slice(), entry + 4)?;
            annotations.insert(
                field_idx,
                parse_annotation_set(self.data.as_slice(), set_off)?,
            );
        }
        Ok(annotations)
    }

    pub fn method_annotations(
        &self,
        class: &ClassDef,
    ) -> Result<HashMap<u32, Vec<EncodedAnnotation>>> {
        let mut annotations = HashMap::new();
        if class.annotations_off == 0 {
            return Ok(annotations);
        }
        let directory = class.annotations_off as usize;
        let fields_size = read_u32(self.data.as_slice(), directory + 4)? as usize;
        let methods_size = read_u32(self.data.as_slice(), directory + 8)? as usize;
        let parameters_size = read_u32(self.data.as_slice(), directory + 12)? as usize;
        let field_bytes = fields_size
            .checked_mul(8)
            .ok_or_else(|| Error::data(directory, "field annotation table size overflow"))?;
        let method_start = directory
            .checked_add(16)
            .and_then(|offset| offset.checked_add(field_bytes))
            .ok_or_else(|| Error::data(directory, "annotation directory offset overflow"))?;
        let method_bytes = methods_size
            .checked_mul(8)
            .ok_or_else(|| Error::data(directory, "method annotation table size overflow"))?;
        let parameter_bytes = parameters_size
            .checked_mul(8)
            .ok_or_else(|| Error::data(directory, "parameter annotation table size overflow"))?;
        require_range(
            self.data.as_slice(),
            method_start,
            method_bytes
                .checked_add(parameter_bytes)
                .ok_or_else(|| Error::data(directory, "annotation directory size overflow"))?,
        )?;
        for index in 0..methods_size {
            let entry = method_start + index * 8;
            let method_idx = read_u32(self.data.as_slice(), entry)?;
            check_index("method", method_idx, self.methods.len())?;
            let set_off = read_u32(self.data.as_slice(), entry + 4)?;
            annotations.insert(
                method_idx,
                parse_annotation_set(self.data.as_slice(), set_off)?,
            );
        }
        Ok(annotations)
    }
}

impl DexFile<'static> {
    /// Parse an owned buffer that may be retained by asynchronous class jobs.
    pub fn parse_owned(data: Arc<[u8]>) -> Result<Self> {
        let parsed: DexFile<'_> = DexFile::parse(data.as_ref())?;
        let DexFile {
            data: _,
            header,
            strings,
            types,
            protos,
            proto_metadata,
            fields,
            field_metadata,
            methods,
            classes,
            class_lookup,
        } = parsed;
        Ok(Self {
            data: DexBytes::Shared(data),
            header,
            strings,
            types,
            protos,
            proto_metadata,
            fields,
            field_metadata,
            methods,
            classes,
            class_lookup,
        })
    }
}

#[derive(Debug)]
struct RawClassDef {
    class_idx: u32,
    access_flags: u32,
    superclass_idx: u32,
    interfaces_off: u32,
    source_file_idx: u32,
    annotations_off: u32,
    class_data_off: u32,
    static_values_off: u32,
}

#[derive(Debug)]
struct ParsedClassData {
    static_fields: Vec<EncodedField>,
    instance_fields: Vec<EncodedField>,
    direct_methods: Vec<EncodedMethod>,
    virtual_methods: Vec<EncodedMethod>,
}

impl Header {
    fn parse(data: &[u8]) -> Result<Self> {
        require_range(data, 0, HEADER_SIZE)?;
        let mut magic = [0_u8; 8];
        magic.copy_from_slice(&data[..8]);
        if &magic[..4] != b"dex\n" || magic[7] != 0 {
            return Err(Error::InvalidMagic(magic));
        }
        let version = std::str::from_utf8(&magic[4..7])
            .ok()
            .and_then(|value| value.parse::<u16>().ok())
            .ok_or(Error::InvalidHeader("non-numeric DEX version"))?;
        let file_size = read_u32(data, 32)?;
        if file_size as usize != data.len() {
            return Err(Error::InvalidHeader(
                "file_size does not match input length",
            ));
        }
        if read_u32(data, 36)? as usize != HEADER_SIZE {
            return Err(Error::InvalidHeader("header_size is not 0x70"));
        }
        let endian = read_u32(data, 40)?;
        if endian != ENDIAN_CONSTANT {
            return Err(Error::UnsupportedEndian(endian));
        }

        let header = Self {
            version,
            checksum: read_u32(data, 8)?,
            file_size,
            map_off: read_u32(data, 52)?,
            string_ids_size: read_u32(data, 56)?,
            string_ids_off: read_u32(data, 60)?,
            type_ids_size: read_u32(data, 64)?,
            type_ids_off: read_u32(data, 68)?,
            proto_ids_size: read_u32(data, 72)?,
            proto_ids_off: read_u32(data, 76)?,
            field_ids_size: read_u32(data, 80)?,
            field_ids_off: read_u32(data, 84)?,
            method_ids_size: read_u32(data, 88)?,
            method_ids_off: read_u32(data, 92)?,
            class_defs_size: read_u32(data, 96)?,
            class_defs_off: read_u32(data, 100)?,
            data_size: read_u32(data, 104)?,
            data_off: read_u32(data, 108)?,
        };

        validate_table(data, header.string_ids_off, header.string_ids_size, 4)?;
        validate_table(data, header.type_ids_off, header.type_ids_size, 4)?;
        validate_table(data, header.proto_ids_off, header.proto_ids_size, 12)?;
        validate_table(data, header.field_ids_off, header.field_ids_size, 8)?;
        validate_table(data, header.method_ids_off, header.method_ids_size, 8)?;
        validate_table(data, header.class_defs_off, header.class_defs_size, 32)?;
        validate_table(data, header.data_off, header.data_size, 1)?;
        Ok(header)
    }
}

fn parse_strings(data: &[u8], header: &Header) -> Result<Vec<String>> {
    let count = count_to_usize(header.string_ids_size)?;
    let mut strings = Vec::with_capacity(count);
    for index in 0..count {
        let table_offset = table_entry_offset(header.string_ids_off, index, 4)?;
        let string_offset = read_u32(data, table_offset)? as usize;
        strings.push(read_mutf8(data, string_offset)?);
    }
    Ok(strings)
}

fn parse_type_ids(data: &[u8], header: &Header, string_count: usize) -> Result<Vec<u32>> {
    let count = count_to_usize(header.type_ids_size)?;
    let mut values = Vec::with_capacity(count);
    for index in 0..count {
        let value = read_u32(data, table_entry_offset(header.type_ids_off, index, 4)?)?;
        check_index("string", value, string_count)?;
        values.push(value);
    }
    Ok(values)
}

fn parse_proto_ids(
    data: &[u8],
    header: &Header,
    type_count: usize,
    string_count: usize,
) -> Result<Vec<ProtoId>> {
    let count = count_to_usize(header.proto_ids_size)?;
    let mut values = Vec::with_capacity(count);
    for index in 0..count {
        let offset = table_entry_offset(header.proto_ids_off, index, 12)?;
        let proto = ProtoId {
            shorty_idx: read_u32(data, offset)?,
            return_type_idx: read_u32(data, offset + 4)?,
            parameters_off: read_u32(data, offset + 8)?,
        };
        check_index("string", proto.shorty_idx, string_count)?;
        check_index("type", proto.return_type_idx, type_count)?;
        values.push(proto);
    }
    Ok(values)
}

fn build_proto_metadata(
    data: &[u8],
    proto: &ProtoId,
    strings: &[String],
    types: &[u32],
) -> Result<ProtoMetadata> {
    let parameters = read_type_list(data, proto.parameters_off, types.len())?;
    let mut descriptor = String::with_capacity(parameters.len() * 8 + 4);
    descriptor.push('(');
    for (position, parameter) in parameters.iter().enumerate() {
        if position != 0 {
            descriptor.push(' ');
        }
        descriptor.push_str(resolve_type(strings, types, *parameter)?);
    }
    descriptor.push(')');
    descriptor.push_str(resolve_type(strings, types, proto.return_type_idx)?);
    Ok(ProtoMetadata {
        descriptor,
        parameter_type_indices: parameters,
    })
}

fn parse_field_ids(
    data: &[u8],
    header: &Header,
    type_count: usize,
    string_count: usize,
) -> Result<Vec<FieldId>> {
    let count = count_to_usize(header.field_ids_size)?;
    let mut values = Vec::with_capacity(count);
    for index in 0..count {
        let offset = table_entry_offset(header.field_ids_off, index, 8)?;
        let field = FieldId {
            class_idx: read_u16(data, offset)?,
            type_idx: read_u16(data, offset + 2)?,
            name_idx: read_u32(data, offset + 4)?,
        };
        check_index("type", field.class_idx.into(), type_count)?;
        check_index("type", field.type_idx.into(), type_count)?;
        check_index("string", field.name_idx, string_count)?;
        values.push(field);
    }
    Ok(values)
}

fn parse_method_ids(
    data: &[u8],
    header: &Header,
    type_count: usize,
    proto_count: usize,
    string_count: usize,
) -> Result<Vec<MethodId>> {
    let count = count_to_usize(header.method_ids_size)?;
    let mut values = Vec::with_capacity(count);
    for index in 0..count {
        let offset = table_entry_offset(header.method_ids_off, index, 8)?;
        let method = MethodId {
            class_idx: read_u16(data, offset)?,
            proto_idx: read_u16(data, offset + 2)?,
            name_idx: read_u32(data, offset + 4)?,
        };
        check_index("type", method.class_idx.into(), type_count)?;
        check_index("proto", method.proto_idx.into(), proto_count)?;
        check_index("string", method.name_idx, string_count)?;
        values.push(method);
    }
    Ok(values)
}

fn parse_class_defs(
    data: &[u8],
    header: &Header,
    type_count: usize,
    string_count: usize,
) -> Result<Vec<ClassDef>> {
    let count = count_to_usize(header.class_defs_size)?;
    let mut values = Vec::with_capacity(count);
    for index in 0..count {
        let offset = table_entry_offset(header.class_defs_off, index, 32)?;
        let raw = RawClassDef {
            class_idx: read_u32(data, offset)?,
            access_flags: read_u32(data, offset + 4)?,
            superclass_idx: read_u32(data, offset + 8)?,
            interfaces_off: read_u32(data, offset + 12)?,
            source_file_idx: read_u32(data, offset + 16)?,
            annotations_off: read_u32(data, offset + 20)?,
            class_data_off: read_u32(data, offset + 24)?,
            static_values_off: read_u32(data, offset + 28)?,
        };
        check_index("type", raw.class_idx, type_count)?;
        if raw.superclass_idx != NO_INDEX {
            check_index("type", raw.superclass_idx, type_count)?;
        }
        if raw.source_file_idx != NO_INDEX {
            check_index("string", raw.source_file_idx, string_count)?;
        }
        if raw.annotations_off != 0 {
            require_range(data, raw.annotations_off as usize, 16)?;
        }
        if raw.class_data_off != 0 {
            require_range(data, raw.class_data_off as usize, 1)?;
        }
        if raw.static_values_off != 0 {
            require_range(data, raw.static_values_off as usize, 1)?;
        }
        values.push(ClassDef {
            class_idx: raw.class_idx,
            access_flags: raw.access_flags,
            superclass_idx: (raw.superclass_idx != NO_INDEX).then_some(raw.superclass_idx),
            interfaces: read_type_list(data, raw.interfaces_off, type_count)?,
            source_file_idx: (raw.source_file_idx != NO_INDEX).then_some(raw.source_file_idx),
            annotations_off: raw.annotations_off,
            class_data_off: raw.class_data_off,
            static_values_off: raw.static_values_off,
            static_fields: Vec::new(),
            instance_fields: Vec::new(),
            direct_methods: Vec::new(),
            virtual_methods: Vec::new(),
        });
    }
    Ok(values)
}

fn parse_class_data(
    data: &[u8],
    offset: u32,
    field_count: usize,
    method_count: usize,
) -> Result<ParsedClassData> {
    let mut cursor = Cursor::at(data, offset as usize)?;
    let static_count = cursor.uleb128()? as usize;
    let instance_count = cursor.uleb128()? as usize;
    let direct_count = cursor.uleb128()? as usize;
    let virtual_count = cursor.uleb128()? as usize;
    if static_count.saturating_add(instance_count) > field_count
        || direct_count.saturating_add(virtual_count) > method_count
    {
        return Err(Error::data(
            offset as usize,
            "class_data count exceeds ID table",
        ));
    }
    Ok(ParsedClassData {
        static_fields: parse_encoded_fields(&mut cursor, static_count, field_count)?,
        instance_fields: parse_encoded_fields(&mut cursor, instance_count, field_count)?,
        direct_methods: parse_encoded_methods(&mut cursor, direct_count, method_count)?,
        virtual_methods: parse_encoded_methods(&mut cursor, virtual_count, method_count)?,
    })
}

fn parse_encoded_fields(
    cursor: &mut Cursor<'_>,
    count: usize,
    field_count: usize,
) -> Result<Vec<EncodedField>> {
    let mut fields = Vec::with_capacity(count);
    let mut index = 0_u32;
    for _ in 0..count {
        index = index
            .checked_add(cursor.uleb128()?)
            .ok_or_else(|| Error::data(cursor.offset, "field index overflow"))?;
        check_index("field", index, field_count)?;
        fields.push(EncodedField {
            field_idx: index,
            access_flags: cursor.uleb128()?,
            initial_value: None,
        });
    }
    Ok(fields)
}

fn parse_encoded_methods(
    cursor: &mut Cursor<'_>,
    count: usize,
    method_count: usize,
) -> Result<Vec<EncodedMethod>> {
    let mut methods = Vec::with_capacity(count);
    let mut index = 0_u32;
    for _ in 0..count {
        index = index
            .checked_add(cursor.uleb128()?)
            .ok_or_else(|| Error::data(cursor.offset, "method index overflow"))?;
        check_index("method", index, method_count)?;
        methods.push(EncodedMethod {
            method_idx: index,
            access_flags: cursor.uleb128()?,
            code_off: cursor.uleb128()?,
        });
    }
    Ok(methods)
}

fn parse_code_item(data: &[u8], offset: u32) -> Result<Option<CodeItem<'_>>> {
    let start = offset as usize;
    require_range(data, start, 16)?;
    if !start.is_multiple_of(4) {
        return Err(Error::data(start, "code_item is not four-byte aligned"));
    }
    let registers_size = read_u16(data, start)?;
    let ins_size = read_u16(data, start + 2)?;
    let outs_size = read_u16(data, start + 4)?;
    let tries_size = read_u16(data, start + 6)?;
    let debug_info_off = read_u32(data, start + 8)?;
    let insns_size = read_u32(data, start + 12)?;
    let insns_bytes = (insns_size as usize)
        .checked_mul(2)
        .ok_or_else(|| Error::data(start + 12, "instruction size overflow"))?;
    let insns_offset = start + 16;
    let insns = slice(data, insns_offset, insns_bytes)?;
    let mut cursor_offset = insns_offset + insns_bytes;
    if tries_size != 0 && insns_size % 2 != 0 {
        require_range(data, cursor_offset, 2)?;
        cursor_offset += 2;
    }
    let mut tries = Vec::with_capacity(tries_size as usize);
    for _ in 0..tries_size {
        tries.push(TryItem {
            start_addr: read_u32(data, cursor_offset)?,
            insn_count: read_u16(data, cursor_offset + 4)?,
            handler_off: read_u16(data, cursor_offset + 6)?,
        });
        cursor_offset += 8;
    }
    let handlers = if tries_size == 0 {
        Vec::new()
    } else {
        parse_catch_handlers(data, cursor_offset)?
    };
    Ok(Some(CodeItem {
        registers_size,
        ins_size,
        outs_size,
        debug_info_off,
        insns_size,
        insns,
        tries,
        handlers,
    }))
}

fn parse_catch_handlers(data: &[u8], offset: usize) -> Result<Vec<CatchHandler>> {
    let list_start = offset;
    let mut cursor = Cursor::at(data, offset)?;
    let count = cursor.uleb128()? as usize;
    if count > data.len() {
        return Err(Error::data(offset, "catch handler count exceeds file size"));
    }
    let mut handlers = Vec::with_capacity(count);
    for _ in 0..count {
        let handler_offset = u32::try_from(cursor.offset - list_start)
            .map_err(|_| Error::data(cursor.offset, "catch handler offset overflow"))?;
        let size = cursor.sleb128()?;
        let typed_count = size.unsigned_abs() as usize;
        let mut typed = Vec::with_capacity(typed_count);
        for _ in 0..typed_count {
            typed.push((cursor.uleb128()?, cursor.uleb128()?));
        }
        let catch_all_addr = if size <= 0 {
            Some(cursor.uleb128()?)
        } else {
            None
        };
        handlers.push(CatchHandler {
            offset: handler_offset,
            handlers: typed,
            catch_all_addr,
        });
    }
    Ok(handlers)
}

fn read_type_list(data: &[u8], offset: u32, type_count: usize) -> Result<Vec<u32>> {
    if offset == 0 {
        return Ok(Vec::new());
    }
    let start = offset as usize;
    let count = read_u32(data, start)? as usize;
    let byte_len = count
        .checked_mul(2)
        .ok_or_else(|| Error::data(start, "type_list size overflow"))?;
    require_range(data, start + 4, byte_len)?;
    let mut types = Vec::with_capacity(count);
    for index in 0..count {
        let value = read_u16(data, start + 4 + index * 2)? as u32;
        check_index("type", value, type_count)?;
        types.push(value);
    }
    Ok(types)
}

fn parse_encoded_array(cursor: &mut Cursor<'_>, depth: usize) -> Result<Vec<EncodedValue>> {
    if depth >= MAX_ENCODED_VALUE_DEPTH {
        return Err(Error::data(
            cursor.offset,
            "encoded value nesting is too deep",
        ));
    }
    let count = cursor.uleb128()? as usize;
    if count > cursor.data.len() {
        return Err(Error::data(
            cursor.offset,
            "encoded array count exceeds file size",
        ));
    }
    let mut values = Vec::with_capacity(count);
    for _ in 0..count {
        values.push(parse_encoded_value(cursor, depth + 1)?);
    }
    Ok(values)
}

fn parse_encoded_value(cursor: &mut Cursor<'_>, depth: usize) -> Result<EncodedValue> {
    let offset = cursor.offset;
    let header = cursor.byte()?;
    let value_type = header & 0x1f;
    let value_arg = header >> 5;
    let width = value_arg as usize + 1;
    let unsigned = |cursor: &mut Cursor<'_>| read_variable_unsigned(cursor, width);
    Ok(match value_type {
        0x00 if value_arg == 0 => EncodedValue::Byte(cursor.byte()? as i8),
        0x02 if value_arg <= 1 => EncodedValue::Short(sign_extend(unsigned(cursor)?, width) as i16),
        0x03 if value_arg <= 1 => EncodedValue::Char(unsigned(cursor)? as u16),
        0x04 if value_arg <= 3 => EncodedValue::Int(unsigned(cursor)? as i64),
        0x06 if value_arg <= 7 => EncodedValue::Long(unsigned(cursor)? as i64),
        0x10 if value_arg <= 3 => {
            EncodedValue::Float((unsigned(cursor)? << (8 * (4 - width))) as u32)
        }
        0x11 if value_arg <= 7 => EncodedValue::Double(unsigned(cursor)? << (8 * (8 - width))),
        0x15 if value_arg <= 3 => EncodedValue::MethodType(unsigned(cursor)? as u32),
        0x16 if value_arg <= 3 => EncodedValue::MethodHandle(unsigned(cursor)? as u32),
        0x17 if value_arg <= 3 => EncodedValue::String(unsigned(cursor)? as u32),
        0x18 if value_arg <= 3 => EncodedValue::Type(unsigned(cursor)? as u32),
        0x19 if value_arg <= 3 => EncodedValue::Field(unsigned(cursor)? as u32),
        0x1a if value_arg <= 3 => EncodedValue::Method(unsigned(cursor)? as u32),
        0x1b if value_arg <= 3 => EncodedValue::Enum(unsigned(cursor)? as u32),
        0x1c if value_arg == 0 => EncodedValue::Array(parse_encoded_array(cursor, depth)?),
        0x1d if value_arg == 0 => {
            EncodedValue::Annotation(parse_encoded_annotation(cursor, depth)?)
        }
        0x1e if value_arg == 0 => EncodedValue::Null,
        0x1f if value_arg <= 1 => EncodedValue::Boolean(value_arg != 0),
        _ => return Err(Error::data(offset, "invalid encoded_value header")),
    })
}

fn parse_annotation_set(data: &[u8], offset: u32) -> Result<Vec<EncodedAnnotation>> {
    if offset == 0 {
        return Ok(Vec::new());
    }
    let offset = offset as usize;
    let count = read_u32(data, offset)? as usize;
    let entries_size = count
        .checked_mul(4)
        .ok_or_else(|| Error::data(offset, "annotation set size overflow"))?;
    require_range(data, offset + 4, entries_size)?;
    let mut annotations = Vec::with_capacity(count);
    for index in 0..count {
        let annotation_off = read_u32(data, offset + 4 + index * 4)? as usize;
        let mut cursor = Cursor::at(data, annotation_off)?;
        cursor.byte()?;
        annotations.push(parse_encoded_annotation(&mut cursor, 0)?);
    }
    Ok(annotations)
}

fn parse_encoded_annotation(cursor: &mut Cursor<'_>, depth: usize) -> Result<EncodedAnnotation> {
    if depth >= MAX_ENCODED_VALUE_DEPTH {
        return Err(Error::data(
            cursor.offset,
            "encoded annotation nesting is too deep",
        ));
    }
    let offset = cursor.offset;
    let type_idx = cursor.uleb128()?;
    let count = cursor.uleb128()? as usize;
    if count > cursor.data.len() {
        return Err(Error::data(
            offset,
            "annotation element count exceeds file size",
        ));
    }
    let mut elements = Vec::with_capacity(count);
    for _ in 0..count {
        elements.push(AnnotationElement {
            name_idx: cursor.uleb128()?,
            value: parse_encoded_value(cursor, depth + 1)?,
        });
    }
    Ok(EncodedAnnotation { type_idx, elements })
}

fn read_variable_unsigned(cursor: &mut Cursor<'_>, width: usize) -> Result<u64> {
    let mut value = 0_u64;
    for shift in 0..width {
        value |= u64::from(cursor.byte()?) << (shift * 8);
    }
    Ok(value)
}

fn sign_extend(value: u64, width: usize) -> i64 {
    let shift = 64 - width * 8;
    ((value << shift) as i64) >> shift
}

fn read_mutf8(data: &[u8], offset: usize) -> Result<String> {
    let mut cursor = Cursor::at(data, offset)?;
    let utf16_len = cursor.uleb128()? as usize;
    let bytes_start = cursor.offset;
    let bytes_end = data[bytes_start..]
        .iter()
        .position(|byte| *byte == 0)
        .map(|relative| bytes_start + relative)
        .ok_or(Error::InvalidUtf8 { offset })?;
    let bytes = &data[bytes_start..bytes_end];
    if bytes.iter().all(|byte| *byte < 0x80) {
        let value = std::str::from_utf8(bytes)
            .map_err(|_| Error::InvalidUtf8 { offset })?
            .to_owned();
        if value.encode_utf16().count() != utf16_len {
            return Err(Error::InvalidUtf8 { offset });
        }
        return Ok(value);
    }

    let mut units = Vec::with_capacity(utf16_len);
    let mut index = 0;
    while index < bytes.len() {
        let first = bytes[index];
        if first & 0x80 == 0 {
            if first == 0 {
                return Err(Error::InvalidUtf8 { offset });
            }
            units.push(u16::from(first));
            index += 1;
        } else if first & 0xe0 == 0xc0 {
            let second = *bytes.get(index + 1).ok_or(Error::InvalidUtf8 { offset })?;
            if second & 0xc0 != 0x80 {
                return Err(Error::InvalidUtf8 { offset });
            }
            units.push((u16::from(first & 0x1f) << 6) | u16::from(second & 0x3f));
            index += 2;
        } else if first & 0xf0 == 0xe0 {
            let second = *bytes.get(index + 1).ok_or(Error::InvalidUtf8 { offset })?;
            let third = *bytes.get(index + 2).ok_or(Error::InvalidUtf8 { offset })?;
            if second & 0xc0 != 0x80 || third & 0xc0 != 0x80 {
                return Err(Error::InvalidUtf8 { offset });
            }
            units.push(
                (u16::from(first & 0x0f) << 12)
                    | (u16::from(second & 0x3f) << 6)
                    | u16::from(third & 0x3f),
            );
            index += 3;
        } else {
            return Err(Error::InvalidUtf8 { offset });
        }
    }
    if units.len() != utf16_len {
        return Err(Error::InvalidUtf8 { offset });
    }
    // Android accepts isolated UTF-16 surrogates in DEX MUTF-8 strings. Rust
    // strings cannot represent them, so retain the rest of the string and use
    // the Unicode replacement character for each malformed code unit.
    Ok(char::decode_utf16(units)
        .map(|character| character.unwrap_or(char::REPLACEMENT_CHARACTER))
        .collect())
}

fn resolve_type<'a>(strings: &'a [String], types: &[u32], index: u32) -> Result<&'a str> {
    let string_index = *types.get(index as usize).ok_or(Error::InvalidIndex {
        table: "type",
        index,
        len: types.len(),
    })?;
    strings
        .get(string_index as usize)
        .map(String::as_str)
        .ok_or(Error::InvalidIndex {
            table: "string",
            index: string_index,
            len: strings.len(),
        })
}

fn count_to_usize(count: u32) -> Result<usize> {
    usize::try_from(count).map_err(|_| Error::InvalidHeader("table count does not fit usize"))
}

fn check_index(table: &'static str, index: u32, len: usize) -> Result<()> {
    if index as usize >= len {
        return Err(Error::InvalidIndex { table, index, len });
    }
    Ok(())
}

fn validate_table(data: &[u8], offset: u32, count: u32, item_size: usize) -> Result<()> {
    if count == 0 {
        return Ok(());
    }
    if offset == 0 {
        return Err(Error::InvalidHeader("non-empty table has zero offset"));
    }
    let byte_len = (count as usize)
        .checked_mul(item_size)
        .ok_or(Error::InvalidHeader("table size overflow"))?;
    require_range(data, offset as usize, byte_len)
}

fn table_entry_offset(base: u32, index: usize, item_size: usize) -> Result<usize> {
    (base as usize)
        .checked_add(
            index
                .checked_mul(item_size)
                .ok_or(Error::InvalidHeader("table offset overflow"))?,
        )
        .ok_or(Error::InvalidHeader("table offset overflow"))
}

fn require_range(data: &[u8], offset: usize, len: usize) -> Result<()> {
    let end = offset.checked_add(len).ok_or(Error::UnexpectedEof {
        offset,
        needed: len,
        len: data.len(),
    })?;
    if end > data.len() {
        return Err(Error::UnexpectedEof {
            offset,
            needed: len,
            len: data.len(),
        });
    }
    Ok(())
}

fn slice(data: &[u8], offset: usize, len: usize) -> Result<&[u8]> {
    require_range(data, offset, len)?;
    Ok(&data[offset..offset + len])
}

fn read_u16(data: &[u8], offset: usize) -> Result<u16> {
    let bytes = slice(data, offset, 2)?;
    Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
}

fn read_u32(data: &[u8], offset: usize) -> Result<u32> {
    let bytes = slice(data, offset, 4)?;
    Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

struct Cursor<'a> {
    data: &'a [u8],
    offset: usize,
}

impl<'a> Cursor<'a> {
    fn at(data: &'a [u8], offset: usize) -> Result<Self> {
        require_range(data, offset, 1)?;
        Ok(Self { data, offset })
    }

    fn byte(&mut self) -> Result<u8> {
        let value = *self.data.get(self.offset).ok_or(Error::UnexpectedEof {
            offset: self.offset,
            needed: 1,
            len: self.data.len(),
        })?;
        self.offset += 1;
        Ok(value)
    }

    fn uleb128(&mut self) -> Result<u32> {
        let start = self.offset;
        let mut value = 0_u32;
        for shift in (0..35).step_by(7) {
            let byte = self.byte()?;
            if shift == 28 && byte > 0x0f {
                return Err(Error::data(start, "ULEB128 value overflows u32"));
            }
            value |= u32::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Ok(value);
            }
        }
        Err(Error::data(start, "ULEB128 is longer than five bytes"))
    }

    fn sleb128(&mut self) -> Result<i32> {
        let start = self.offset;
        let mut value = 0_i32;
        let mut shift = 0;
        loop {
            if shift >= 35 {
                return Err(Error::data(start, "SLEB128 is longer than five bytes"));
            }
            let byte = self.byte()?;
            value |= i32::from(byte & 0x7f) << shift;
            shift += 7;
            if byte & 0x80 == 0 {
                if shift < 32 && byte & 0x40 != 0 {
                    value |= !0_i32 << shift;
                }
                return Ok(value);
            }
        }
    }
}
