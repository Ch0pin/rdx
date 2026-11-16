use std::borrow::Cow;
use std::collections::HashSet;
use std::sync::Arc;

pub mod opcode;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum VariableId {
    Register(u16),
    Temporary(u32),
    Version(u32),
}

trait VariableCollector {
    fn add(&mut self, variable: VariableId);
}

pub(crate) struct UniqueVariableCollector<'a>(pub &'a mut Vec<VariableId>);

impl VariableCollector for UniqueVariableCollector<'_> {
    fn add(&mut self, variable: VariableId) {
        if !self.0.contains(&variable) {
            self.0.push(variable);
        }
    }
}

impl VariableCollector for HashSet<VariableId> {
    fn add(&mut self, variable: VariableId) {
        self.insert(variable);
    }
}

impl VariableCollector for Vec<VariableId> {
    fn add(&mut self, variable: VariableId) {
        self.push(variable);
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Variable {
    pub ty: Option<Cow<'static, str>>,
}

impl Variable {
    pub fn register(_register: u16) -> Self {
        Self { ty: None }
    }
}

const INLINE_VARIABLE_NAME_CAPACITY: usize = 14;
const VARIABLE_NAME_LENGTH_MASK: u8 = 0x0f;
const VARIABLE_DECLARED: u8 = 0x10;
const VARIABLE_THIS: u8 = 0x20;
const VARIABLE_SUPER: u8 = 0x40;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OwnedVariableMetadata {
    flags: u8,
    name: Box<str>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum VariableMetadata {
    Inline([u8; INLINE_VARIABLE_NAME_CAPACITY + 1]),
    Owned(Box<OwnedVariableMetadata>),
}

impl VariableMetadata {
    fn numbered(prefix: &str, value: u64) -> Self {
        let mut digits = [0_u8; 20];
        let digits = write_decimal(value, &mut digits);
        let len = prefix.len() + digits.len();
        if len <= INLINE_VARIABLE_NAME_CAPACITY {
            let mut storage = [0; INLINE_VARIABLE_NAME_CAPACITY + 1];
            storage[0] = len as u8;
            storage[1..1 + prefix.len()].copy_from_slice(prefix.as_bytes());
            storage[1 + prefix.len()..1 + len].copy_from_slice(digits);
            Self::Inline(storage)
        } else {
            Self::Owned(Box::new(OwnedVariableMetadata {
                flags: 0,
                name: format!("{prefix}{value}").into_boxed_str(),
            }))
        }
    }

    fn versioned(base: &Self, version: usize) -> Self {
        let mut digits = [0_u8; 20];
        let digits = write_decimal(version as u64, &mut digits);
        let raw_base = base.as_str();
        let base = raw_base.split_once('_').map_or(raw_base, |(prefix, _)| prefix);
        let len = base.len() + 1 + digits.len();
        if len <= INLINE_VARIABLE_NAME_CAPACITY {
            let mut storage = [0; INLINE_VARIABLE_NAME_CAPACITY + 1];
            storage[0] = len as u8;
            storage[1..1 + base.len()].copy_from_slice(base.as_bytes());
            storage[1 + base.len()] = b'_';
            storage[2 + base.len()..1 + len].copy_from_slice(digits);
            Self::Inline(storage)
        } else {
            Self::Owned(Box::new(OwnedVariableMetadata {
                flags: 0,
                name: format!("{base}_{version}").into_boxed_str(),
            }))
        }
    }

    pub(crate) fn as_str(&self) -> &str {
        match self {
            Self::Inline(storage) => {
                let len = usize::from(storage[0] & VARIABLE_NAME_LENGTH_MASK);
                // Every inline name is assembled from ASCII prefixes and decimal digits.
                unsafe { std::str::from_utf8_unchecked(&storage[1..1 + len]) }
            }
            Self::Owned(metadata) => &metadata.name,
        }
    }

    fn flags(&self) -> u8 {
        match self {
            Self::Inline(storage) => storage[0] & !VARIABLE_NAME_LENGTH_MASK,
            Self::Owned(metadata) => metadata.flags,
        }
    }

    fn set_flag(&mut self, flag: u8, value: bool) {
        let flags = if value {
            self.flags() | flag
        } else {
            self.flags() & !flag
        };
        self.set_flags(flags);
    }

    fn set_flags(&mut self, flags: u8) {
        match self {
            Self::Inline(storage) => {
                storage[0] = (storage[0] & VARIABLE_NAME_LENGTH_MASK) | flags;
            }
            Self::Owned(metadata) => metadata.flags = flags,
        }
    }
}

fn write_decimal(mut value: u64, storage: &mut [u8; 20]) -> &[u8] {
    let mut start = storage.len();
    loop {
        start -= 1;
        storage[start] = b'0' + (value % 10) as u8;
        value /= 10;
        if value == 0 {
            return &storage[start..];
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum ConstantValue {
    Integer {
        value: i64,
        ty: &'static str,
    },
    String(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Constant {
    pub value: ConstantValue,
}

impl Constant {
    pub fn ty(&self) -> &'static str {
        match self.value {
            ConstantValue::Integer { ty, .. } => ty,
            ConstantValue::String(_) => "Ljava/lang/String;",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinaryOp {
    Compare,
    Add,
    Subtract,
    Multiply,
    Divide,
    Remainder,
    And,
    Or,
    Xor,
    ShiftLeft,
    ShiftRight,
    UnsignedShiftRight,
    Equal,
    NotEqual,
    Less,
    GreaterEqual,
    Greater,
    LessEqual,
    InstanceOf,
}

impl BinaryOp {
    pub fn negate(self) -> Self {
        match self {
            Self::Equal => Self::NotEqual,
            Self::NotEqual => Self::Equal,
            Self::Less => Self::GreaterEqual,
            Self::GreaterEqual => Self::Less,
            Self::Greater => Self::LessEqual,
            Self::LessEqual => Self::Greater,
            other => other,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnaryOp {
    Negate,
    Not,
    Cast,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InvokeKind {
    Virtual,
    Super,
    Direct,
    Static,
    Interface,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FieldReference {
    pub class: Arc<str>,
    pub ty: Arc<str>,
    pub name: Arc<str>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MethodReference {
    pub class: String,
    pub name: String,
    pub parameters: Vec<String>,
    pub return_type: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct InvokeExpression {
    pub kind: InvokeKind,
    pub method: MethodReference,
    pub base: Value,
    pub arguments: Vec<Value>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CheckCastExpression {
    pub value: Value,
    pub ty: String,
    pub descriptor: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FilledArrayExpression {
    pub size: u8,
    pub ty: String,
    pub arguments: Vec<Value>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct UnaryExpression {
    pub op: UnaryOp,
    pub cast: Option<&'static str>,
    pub value: Value,
    pub ty: &'static str,
}

#[derive(Clone, Debug, PartialEq)]
pub struct InstanceFieldExpression {
    pub object: Value,
    pub field: FieldReference,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ArrayLoadExpression {
    pub array: Value,
    pub index: Value,
    pub ty: Option<&'static str>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct NewArrayExpression {
    pub size: Value,
    pub ty: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BinaryExpression {
    pub op: BinaryOp,
    pub left: Value,
    pub right: Value,
    pub ty: &'static str,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ArrayStoreStatement {
    pub value: Value,
    pub array: Value,
    pub index: Value,
    pub ty: Option<&'static str>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct InstancePutStatement {
    pub value: Value,
    pub object: Value,
    pub field: FieldReference,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Variable(VariableId),
    Constant(Constant),
    BaseClass {
        name: String,
    },
    NewInstance {
        ty: String,
    },
    Invoke(Box<InvokeExpression>),
    CheckCast(Box<CheckCastExpression>),
    ArrayLoad(Box<ArrayLoadExpression>),
    ArrayLength(Box<Value>),
    NewArray(Box<NewArrayExpression>),
    FilledArray(Box<FilledArrayExpression>),
    Binary(Box<BinaryExpression>),
    Unary(Box<UnaryExpression>),
    InstanceField(Box<InstanceFieldExpression>),
    StaticField(Box<FieldReference>),
}

impl Value {
    pub fn used_variables(&self, output: &mut HashSet<VariableId>) {
        self.collect_used_variables(output);
    }

    fn collect_used_variables<C: VariableCollector>(&self, output: &mut C) {
        match self {
            Self::Variable(variable) => {
                output.add(*variable);
            }
            Self::Invoke(invoke) => {
                invoke.base.collect_used_variables(output);
                for argument in &invoke.arguments {
                    argument.collect_used_variables(output);
                }
            }
            Self::CheckCast(cast) => cast.value.collect_used_variables(output),
            Self::Unary(unary) => unary.value.collect_used_variables(output),
            Self::ArrayLoad(load) => {
                load.array.collect_used_variables(output);
                load.index.collect_used_variables(output);
            }
            Self::ArrayLength(array) => array.collect_used_variables(output),
            Self::NewArray(array) => array.size.collect_used_variables(output),
            Self::FilledArray(array) => {
                for argument in &array.arguments {
                    argument.collect_used_variables(output);
                }
            }
            Self::Binary(binary) => {
                binary.left.collect_used_variables(output);
                binary.right.collect_used_variables(output);
            }
            Self::InstanceField(field) => field.object.collect_used_variables(output),
            Self::Constant(_)
            | Self::BaseClass { .. }
            | Self::NewInstance { .. }
            | Self::StaticField(_) => {}
        }
    }

    pub fn has_side_effect(&self) -> bool {
        match self {
            Self::Invoke(_) => true,
            Self::Binary(binary) => {
                binary.left.has_side_effect() || binary.right.has_side_effect()
            }
            Self::Variable(_)
            | Self::Constant(_)
            | Self::BaseClass { .. }
            | Self::NewInstance { .. }
            | Self::CheckCast(_)
            | Self::ArrayLoad(_)
            | Self::ArrayLength(_)
            | Self::NewArray(_)
            | Self::FilledArray(_)
            | Self::Unary(_)
            | Self::InstanceField(_)
            | Self::StaticField(_) => false,
        }
    }

    pub fn replace(&mut self, old: VariableId, new: &Value) {
        match self {
            Self::Variable(variable) if *variable == old => *self = new.clone(),
            Self::Invoke(invoke) => {
                invoke.base.replace(old, new);
                for argument in &mut invoke.arguments {
                    argument.replace(old, new);
                }
            }
            Self::CheckCast(cast) => cast.value.replace(old, new),
            Self::Unary(unary) => unary.value.replace(old, new),
            Self::ArrayLoad(load) => {
                load.array.replace(old, new);
                load.index.replace(old, new);
            }
            Self::ArrayLength(array) => array.replace(old, new),
            Self::NewArray(array) => array.size.replace(old, new),
            Self::FilledArray(array) => {
                for argument in &mut array.arguments {
                    argument.replace(old, new);
                }
            }
            Self::Binary(binary) => {
                binary.left.replace(old, new);
                binary.right.replace(old, new);
            }
            Self::InstanceField(field) => field.object.replace(old, new),
            Self::Variable(_)
            | Self::Constant(_)
            | Self::BaseClass { .. }
            | Self::NewInstance { .. }
            | Self::StaticField(_) => {}
        }
    }

    fn replace_once_owned(&mut self, old: VariableId, replacement: &mut Option<Value>) {
        if replacement.is_none() {
            return;
        }
        match self {
            Self::Variable(variable) if *variable == old => {
                *self = replacement
                    .take()
                    .expect("checked replacement is available");
            }
            Self::Invoke(invoke) => {
                invoke.base.replace_once_owned(old, replacement);
                for argument in &mut invoke.arguments {
                    argument.replace_once_owned(old, replacement);
                }
            }
            Self::CheckCast(cast) => cast.value.replace_once_owned(old, replacement),
            Self::Unary(unary) => unary.value.replace_once_owned(old, replacement),
            Self::ArrayLoad(load) => {
                load.array.replace_once_owned(old, replacement);
                load.index.replace_once_owned(old, replacement);
            }
            Self::ArrayLength(array) => array.replace_once_owned(old, replacement),
            Self::NewArray(array) => array.size.replace_once_owned(old, replacement),
            Self::FilledArray(array) => {
                for argument in &mut array.arguments {
                    argument.replace_once_owned(old, replacement);
                }
            }
            Self::Binary(binary) => {
                binary.left.replace_once_owned(old, replacement);
                binary.right.replace_once_owned(old, replacement);
            }
            Self::InstanceField(field) => field.object.replace_once_owned(old, replacement),
            Self::Variable(_)
            | Self::Constant(_)
            | Self::BaseClass { .. }
            | Self::NewInstance { .. }
            | Self::StaticField(_) => {}
        }
    }

    fn remap_variables<F>(&mut self, remap: &mut F)
    where
        F: FnMut(VariableId) -> VariableId,
    {
        match self {
            Self::Variable(variable) => *variable = remap(*variable),
            Self::Invoke(invoke) => {
                invoke.base.remap_variables(remap);
                for argument in &mut invoke.arguments {
                    argument.remap_variables(remap);
                }
            }
            Self::CheckCast(cast) => cast.value.remap_variables(remap),
            Self::Unary(unary) => unary.value.remap_variables(remap),
            Self::ArrayLoad(load) => {
                load.array.remap_variables(remap);
                load.index.remap_variables(remap);
            }
            Self::ArrayLength(array) => array.remap_variables(remap),
            Self::NewArray(array) => array.size.remap_variables(remap),
            Self::FilledArray(array) => {
                for argument in &mut array.arguments {
                    argument.remap_variables(remap);
                }
            }
            Self::Binary(binary) => {
                binary.left.remap_variables(remap);
                binary.right.remap_variables(remap);
            }
            Self::InstanceField(field) => field.object.remap_variables(remap),
            Self::Constant(_)
            | Self::BaseClass { .. }
            | Self::NewInstance { .. }
            | Self::StaticField(_) => {}
        }
    }

    pub fn is_constant(&self, variables: &VariableTable) -> bool {
        match self {
            Self::Constant(_) | Self::BaseClass { .. } => true,
            Self::Variable(variable) => variables.is_declared(*variable),
            Self::CheckCast(cast) => cast.value.is_constant(variables),
            Self::Unary(unary) if unary.op == UnaryOp::Cast => {
                unary.value.is_constant(variables)
            }
            _ => false,
        }
    }

    pub fn is_propagable(&self) -> bool {
        !matches!(self, Self::NewArray(_))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssignKind {
    Normal,
    Move,
    MoveResult,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Statement {
    Assign {
        lhs: Option<VariableId>,
        rhs: Value,
        kind: AssignKind,
    },
    ArrayStore(Box<ArrayStoreStatement>),
    StaticPut {
        value: Value,
        field: FieldReference,
    },
    InstancePut(Box<InstancePutStatement>),
    Return(Option<Value>),
    Nop,
    Switch {
        value: Value,
        payload_offset: i32,
        values: Vec<i32>,
    },
    FillArray {
        array: Value,
        element_width: u16,
        size: u32,
        data: Vec<u8>,
    },
    MoveException {
        lhs: VariableId,
        ty: Option<String>,
    },
    MonitorEnter(Value),
    MonitorExit(Value),
    Throw(Value),
    Condition {
        op: BinaryOp,
        left: Value,
        right: Option<Value>,
    },
}

impl Statement {
    pub fn defined_variable(&self) -> Option<VariableId> {
        match self {
            Self::Assign { lhs, .. } => *lhs,
            Self::MoveException { lhs, .. } => Some(*lhs),
            _ => None,
        }
    }

    pub fn remove_defined_variable(&mut self) {
        if let Self::Assign { lhs, .. } = self {
            *lhs = None;
        }
    }

    pub fn replace_defined_variable(&mut self, variable: VariableId) -> bool {
        match self {
            Self::Assign { lhs: Some(lhs), .. } => {
                *lhs = variable;
                true
            }
            Self::MoveException { lhs, .. } => {
                *lhs = variable;
                true
            }
            _ => false,
        }
    }

    pub fn replace(&mut self, old: VariableId, new: &Value) {
        match self {
            Self::Assign { rhs, .. } => rhs.replace(old, new),
            Self::ArrayStore(store) => {
                store.value.replace(old, new);
                store.array.replace(old, new);
                store.index.replace(old, new);
            }
            Self::StaticPut { value, .. } => value.replace(old, new),
            Self::InstancePut(store) => {
                let object_matches = matches!(&store.object, Value::Variable(variable) if *variable == old);
                let value_matches = matches!(&store.value, Value::Variable(variable) if *variable == old);
                if object_matches || value_matches {
                    if object_matches {
                        store.object = new.clone();
                    }
                    if value_matches {
                        store.value = new.clone();
                    }
                } else if !matches!(
                    (&store.object, new),
                    (Value::Variable(object), Value::Variable(replacement)) if object == replacement
                ) {
                    store.value.replace(old, new);
                    store.object.replace(old, new);
                }
            }
            Self::Return(Some(value))
            | Self::MonitorEnter(value)
            | Self::MonitorExit(value)
            | Self::Throw(value) => value.replace(old, new),
            Self::Switch { value, .. } => value.replace(old, new),
            Self::FillArray { array, .. } => array.replace(old, new),
            Self::Condition { left, right, .. } => {
                left.replace(old, new);
                if let Some(right) = right {
                    right.replace(old, new);
                }
            }
            Self::Return(None) | Self::Nop | Self::MoveException { .. } => {}
        }
    }

    pub(crate) fn replace_once_owned(
        &mut self,
        old: VariableId,
        new: Value,
    ) -> Option<Value> {
        let mut replacement = Some(new);
        match self {
            Self::Assign { rhs, .. } => rhs.replace_once_owned(old, &mut replacement),
            Self::ArrayStore(store) => {
                store.value.replace_once_owned(old, &mut replacement);
                store.array.replace_once_owned(old, &mut replacement);
                store.index.replace_once_owned(old, &mut replacement);
            }
            Self::StaticPut { value, .. } => {
                value.replace_once_owned(old, &mut replacement);
            }
            Self::InstancePut(store) => {
                store.value.replace_once_owned(old, &mut replacement);
                store.object.replace_once_owned(old, &mut replacement);
            }
            Self::Return(Some(value))
            | Self::MonitorEnter(value)
            | Self::MonitorExit(value)
            | Self::Throw(value) => value.replace_once_owned(old, &mut replacement),
            Self::Switch { value, .. } => value.replace_once_owned(old, &mut replacement),
            Self::FillArray { array, .. } => array.replace_once_owned(old, &mut replacement),
            Self::Condition { left, right, .. } => {
                left.replace_once_owned(old, &mut replacement);
                if let Some(right) = right {
                    right.replace_once_owned(old, &mut replacement);
                }
            }
            Self::Return(None) | Self::Nop | Self::MoveException { .. } => {}
        }
        replacement
    }

    pub(crate) fn remap_used_variables<F>(&mut self, remap: &mut F)
    where
        F: FnMut(VariableId) -> VariableId,
    {
        match self {
            Self::Assign { rhs, .. } => rhs.remap_variables(remap),
            Self::ArrayStore(store) => {
                store.value.remap_variables(remap);
                store.array.remap_variables(remap);
                store.index.remap_variables(remap);
            }
            Self::StaticPut { value, .. } => value.remap_variables(remap),
            Self::InstancePut(store) => {
                store.value.remap_variables(remap);
                store.object.remap_variables(remap);
            }
            Self::Return(Some(value))
            | Self::MonitorEnter(value)
            | Self::MonitorExit(value)
            | Self::Throw(value) => value.remap_variables(remap),
            Self::Switch { value, .. } => value.remap_variables(remap),
            Self::FillArray { array, .. } => array.remap_variables(remap),
            Self::Condition { left, right, .. } => {
                left.remap_variables(remap);
                if let Some(right) = right {
                    right.remap_variables(remap);
                }
            }
            Self::Return(None) | Self::Nop | Self::MoveException { .. } => {}
        }
    }

    pub fn rhs(&self) -> Option<&Value> {
        match self {
            Self::Assign { rhs, .. } => Some(rhs),
            _ => None,
        }
    }

    pub fn is_call(&self) -> bool {
        matches!(
            self,
            Self::Assign {
                rhs: Value::Invoke(_),
                ..
            }
        )
    }

    pub fn is_propagable(&self) -> bool {
        self.rhs().is_none_or(Value::is_propagable)
    }

    pub fn used_variables(&self) -> HashSet<VariableId> {
        let mut output = HashSet::new();
        self.collect_used_variables(&mut output);
        output
    }

    pub(crate) fn collect_used_variables(&self, output: &mut HashSet<VariableId>) {
        self.collect_used_variables_into(output);
    }

    pub(crate) fn collect_used_variables_vec(&self, output: &mut Vec<VariableId>) {
        self.collect_used_variables_into(output);
    }

    pub(crate) fn collect_unique_used_variables_vec(&self, output: &mut Vec<VariableId>) {
        self.collect_used_variables_into(&mut UniqueVariableCollector(output));
    }

    fn collect_used_variables_into<C: VariableCollector>(&self, output: &mut C) {
        match self {
            Self::Assign { rhs, .. } => rhs.collect_used_variables(output),
            Self::ArrayStore(store) => {
                store.value.collect_used_variables(output);
                store.array.collect_used_variables(output);
                store.index.collect_used_variables(output);
            }
            Self::StaticPut { value, .. } => value.collect_used_variables(output),
            Self::InstancePut(store) => {
                store.value.collect_used_variables(output);
                store.object.collect_used_variables(output);
            }
            Self::Return(Some(value))
            | Self::MonitorEnter(value)
            | Self::MonitorExit(value)
            | Self::Throw(value) => value.collect_used_variables(output),
            Self::Switch { value, .. } => value.collect_used_variables(output),
            Self::FillArray { array, .. } => array.collect_used_variables(output),
            Self::Condition { left, right, .. } => {
                left.collect_used_variables(output);
                if let Some(right) = right {
                    right.collect_used_variables(output);
                }
            }
            Self::Return(None) | Self::Nop | Self::MoveException { .. } => {}
        }
    }

    pub fn has_side_effect(&self) -> bool {
        match self {
            Self::Assign {
                kind: AssignKind::Move,
                ..
            } => false,
            Self::Assign { rhs, .. } => rhs.has_side_effect(),
            Self::ArrayStore(_)
            | Self::StaticPut { .. }
            | Self::InstancePut(_)
            | Self::MoveException { .. }
            | Self::MonitorEnter(_)
            | Self::MonitorExit(_)
            | Self::Throw(_) => true,
            Self::Return(value) => value.as_ref().is_some_and(Value::has_side_effect),
            Self::Switch { value, .. } => value.has_side_effect(),
            Self::FillArray { array, .. } => array.has_side_effect(),
            Self::Condition { .. } => false,
            Self::Nop => false,
        }
    }

    pub fn negate_condition(&mut self) -> bool {
        if let Self::Condition { op, .. } = self {
            *op = op.negate();
            true
        } else {
            false
        }
    }
}

#[derive(Debug, Default)]
pub struct VariableTable {
    registers: Vec<Option<Variable>>,
    register_metadata: Vec<Option<VariableMetadata>>,
    temporaries: Vec<Variable>,
    temporary_metadata: Vec<VariableMetadata>,
    versions: Vec<Variable>,
    version_metadata: Vec<VariableMetadata>,
    next_temporary: u32,
    next_version: u32,
    last_result: Option<VariableId>,
}

impl VariableTable {
    pub fn register(&mut self, register: u16) -> VariableId {
        let id = VariableId::Register(register);
        let index = usize::from(register);
        if self.registers.len() <= index {
            self.registers.resize_with(index + 1, || None);
            self.register_metadata.resize_with(index + 1, || None);
        }
        if self.registers[index].is_none() {
            self.registers[index] = Some(Variable::register(register));
            self.register_metadata[index] =
                Some(VariableMetadata::numbered("", u64::from(register)));
        }
        id
    }

    pub fn temporary(&mut self) -> VariableId {
        self.next_temporary += 1;
        let id = VariableId::Temporary(self.next_temporary);
        self.temporaries.push(Variable { ty: None });
        self.temporary_metadata.push(VariableMetadata::numbered(
            "tmp",
            u64::from(self.next_temporary),
        ));
        self.last_result = Some(id);
        id
    }

    pub fn set_last_result(&mut self, variable: VariableId) {
        self.last_result = Some(variable);
    }

    pub fn last_result(&self) -> Option<VariableId> {
        self.last_result
    }

    pub fn version(&mut self, original: VariableId, version: usize) -> VariableId {
        let base_metadata = self
            .metadata(original)
            .expect("known original variable metadata")
            .clone();
        self.version_from_metadata(original, version, &base_metadata)
    }

    pub(crate) fn version_from_metadata(
        &mut self,
        original: VariableId,
        version: usize,
        base_metadata: &VariableMetadata,
    ) -> VariableId {
        self.next_version += 1;
        let id = VariableId::Version(self.next_version);
        let variable = self.get(original).expect("known original variable").clone();
        self.versions.push(variable);
        self.version_metadata
            .push(VariableMetadata::versioned(base_metadata, version));
        id
    }

    pub fn name(&self, id: VariableId) -> Option<&str> {
        self.metadata(id).map(VariableMetadata::as_str)
    }

    pub(crate) fn cloned_metadata(&self, id: VariableId) -> Option<VariableMetadata> {
        self.metadata(id).cloned()
    }

    pub(crate) fn set_versioned_name(
        &mut self,
        id: VariableId,
        base_metadata: &VariableMetadata,
        version: usize,
    ) {
        let flags = self
            .metadata(id)
            .expect("known variable metadata")
            .flags();
        let mut metadata = VariableMetadata::versioned(base_metadata, version);
        metadata.set_flags(flags);
        *self.metadata_mut(id).expect("known variable metadata") = metadata;
    }

    pub(crate) fn set_custom_name(&mut self, id: VariableId, custom_name: String) {
        let flags = self
            .metadata(id)
            .expect("known variable metadata")
            .flags();
        *self.metadata_mut(id).expect("known variable metadata") =
            VariableMetadata::Owned(Box::new(OwnedVariableMetadata {
                flags,
                name: custom_name.into_boxed_str(),
            }));
    }

    pub fn is_declared(&self, id: VariableId) -> bool {
        self.metadata(id)
            .is_some_and(|metadata| metadata.flags() & VARIABLE_DECLARED != 0)
    }

    pub fn set_declared(&mut self, id: VariableId, declared: bool) {
        self.metadata_mut(id)
            .expect("known variable metadata")
            .set_flag(VARIABLE_DECLARED, declared);
    }

    pub fn is_this(&self, id: VariableId) -> bool {
        self.metadata(id)
            .is_some_and(|metadata| metadata.flags() & VARIABLE_THIS != 0)
    }

    pub fn set_is_this(&mut self, id: VariableId, is_this: bool) {
        self.metadata_mut(id)
            .expect("known variable metadata")
            .set_flag(VARIABLE_THIS, is_this);
    }

    pub fn is_super(&self, id: VariableId) -> bool {
        self.metadata(id)
            .is_some_and(|metadata| metadata.flags() & VARIABLE_SUPER != 0)
    }

    pub fn set_is_super(&mut self, id: VariableId, is_super: bool) {
        self.metadata_mut(id)
            .expect("known variable metadata")
            .set_flag(VARIABLE_SUPER, is_super);
    }

    fn metadata(&self, id: VariableId) -> Option<&VariableMetadata> {
        match id {
            VariableId::Register(register) => self
                .register_metadata
                .get(usize::from(register))
                .and_then(Option::as_ref),
            VariableId::Temporary(temporary) => temporary
                .checked_sub(1)
                .and_then(|index| self.temporary_metadata.get(index as usize)),
            VariableId::Version(version) => version
                .checked_sub(1)
                .and_then(|index| self.version_metadata.get(index as usize)),
        }
    }

    fn metadata_mut(&mut self, id: VariableId) -> Option<&mut VariableMetadata> {
        match id {
            VariableId::Register(register) => self
                .register_metadata
                .get_mut(usize::from(register))
                .and_then(Option::as_mut),
            VariableId::Temporary(temporary) => temporary
                .checked_sub(1)
                .and_then(|index| self.temporary_metadata.get_mut(index as usize)),
            VariableId::Version(version) => version
                .checked_sub(1)
                .and_then(|index| self.version_metadata.get_mut(index as usize)),
        }
    }

    pub fn parameter_ids(&self) -> Vec<VariableId> {
        self.registers
            .iter()
            .enumerate()
            .filter_map(|(register, variable)| {
                let id = VariableId::Register(register as u16);
                (variable.is_some() && self.is_declared(id)).then_some(id)
            })
            .collect()
    }

    pub(crate) fn dense_len(&self) -> usize {
        self.registers.len() + self.temporaries.len() + self.versions.len()
    }

    pub(crate) fn dense_index(&self, id: VariableId) -> Option<usize> {
        match id {
            VariableId::Register(register) => {
                let index = usize::from(register);
                (index < self.registers.len()).then_some(index)
            }
            VariableId::Temporary(temporary) => temporary.checked_sub(1).and_then(|index| {
                let index = index as usize;
                (index < self.temporaries.len()).then_some(self.registers.len() + index)
            }),
            VariableId::Version(version) => version.checked_sub(1).and_then(|index| {
                let index = index as usize;
                (index < self.versions.len())
                    .then_some(self.registers.len() + self.temporaries.len() + index)
            }),
        }
    }

    pub fn get(&self, id: VariableId) -> Option<&Variable> {
        match id {
            VariableId::Register(register) => self
                .registers
                .get(usize::from(register))
                .and_then(Option::as_ref),
            VariableId::Temporary(temporary) => temporary
                .checked_sub(1)
                .and_then(|index| self.temporaries.get(index as usize)),
            VariableId::Version(version) => version
                .checked_sub(1)
                .and_then(|index| self.versions.get(index as usize)),
        }
    }

    pub fn get_mut(&mut self, id: VariableId) -> Option<&mut Variable> {
        match id {
            VariableId::Register(register) => self
                .registers
                .get_mut(usize::from(register))
                .and_then(Option::as_mut),
            VariableId::Temporary(temporary) => temporary
                .checked_sub(1)
                .and_then(|index| self.temporaries.get_mut(index as usize)),
            VariableId::Version(version) => version
                .checked_sub(1)
                .and_then(|index| self.versions.get_mut(index as usize)),
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = (VariableId, &Variable)> {
        self.registers
            .iter()
            .enumerate()
            .filter_map(|(register, variable)| {
                variable
                    .as_ref()
                    .map(|variable| (VariableId::Register(register as u16), variable))
            })
            .chain(
                self.temporaries
                    .iter()
                    .enumerate()
                    .map(|(temporary, variable)| {
                        (VariableId::Temporary(temporary as u32 + 1), variable)
                    }),
            )
            .chain(self.versions.iter().enumerate().map(|(version, variable)| {
                (VariableId::Version(version as u32 + 1), variable)
            }))
    }
}
