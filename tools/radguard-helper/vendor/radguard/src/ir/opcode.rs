use std::borrow::Cow;
use std::collections::VecDeque;

use crate::analysis::MethodAnalysis;
use crate::dex::instruction::{DecodedInstruction, Operands, Payload};
use crate::dex::{CodeItem, DexFile, EncodedMethod, ResolvedMethod};
use crate::error::{Error, Result};

use super::{
    ArrayLoadExpression, ArrayStoreStatement, AssignKind, BinaryExpression, BinaryOp,
    CheckCastExpression, Constant, ConstantValue, FieldReference, FilledArrayExpression,
    InstanceFieldExpression, InstancePutStatement, InvokeExpression, InvokeKind, MethodReference,
    NewArrayExpression, Statement, UnaryExpression, UnaryOp, Value, VariableId, VariableTable,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockKind {
    Statement,
    Return,
    Throw,
    Switch,
    Condition,
    ShortCircuit,
    Loop,
    Try,
    Catch,
}

#[derive(Debug)]
pub struct LoweredBlock {
    pub start: u32,
    pub end: u32,
    pub kind: BlockKind,
    pub statements: Vec<Statement>,
    pub predecessors: Vec<usize>,
    pub successors: Vec<usize>,
    pub exception_successors: Vec<usize>,
    pub exception_handlers: Vec<(usize, Option<String>)>,
    pub branch_true: Option<usize>,
    pub branch_false: Option<usize>,
    pub exception: Option<usize>,
}

#[derive(Debug)]
pub struct LoweredMethod {
    pub variables: VariableTable,
    pub blocks: Vec<LoweredBlock>,
}

pub fn lower_method(dex: &DexFile<'_>, method: &EncodedMethod) -> Result<Option<LoweredMethod>> {
    lower_method_impl(dex, method, None, false)
}

pub(crate) fn lower_method_with_resolved(
    dex: &DexFile<'_>,
    method: &EncodedMethod,
    resolved: ResolvedMethod<'_>,
    preserve_parameter_types: bool,
) -> Result<Option<LoweredMethod>> {
    lower_method_impl(dex, method, Some(resolved), preserve_parameter_types)
}

fn lower_method_impl(
    dex: &DexFile<'_>,
    method: &EncodedMethod,
    resolved: Option<ResolvedMethod<'_>>,
    preserve_parameter_types: bool,
) -> Result<Option<LoweredMethod>> {
    let Some(code) = dex.code(method)? else {
        return Ok(None);
    };
    let analysis = MethodAnalysis::from_code(&code)?;
    let resolved = match resolved {
        Some(resolved) => resolved,
        None => dex.method(method.method_idx)?,
    };
    let mut lowerer = Lowerer::new(dex, method, &code, resolved, preserve_parameter_types)?;
    let mut statements_by_block: Vec<Vec<Statement>> = analysis
        .blocks
        .iter()
        .map(|block| Vec::with_capacity(block.instructions.len()))
        .collect();
    if !analysis.blocks.is_empty() {
        let mut visited = vec![false; analysis.blocks.len()];
        let mut queue = VecDeque::from([(0_usize, None)]);
        visited[0] = true;
        while let Some((block_index, exception_type)) = queue.pop_front() {
            let block = &analysis.blocks[block_index];
            for instruction_index in block.instructions.clone() {
                let mut statement = lowerer.lower(
                    &analysis.instructions[instruction_index],
                    &analysis.instructions,
                )?;
                if let Some(Statement::MoveException { lhs, ty }) = &mut statement {
                    *ty = exception_type.clone();
                    if !preserve_parameter_types || !lowerer.variables.is_declared(*lhs) {
                        lowerer
                            .variables
                            .get_mut(*lhs)
                            .expect("move-exception register exists")
                            .ty = exception_type.clone().map(Cow::Owned);
                    }
                }
                if let Some(statement) = statement {
                    let position = instruction_index - block.instructions.start;
                    let terminal_goto = position + 1 == block.instructions.len()
                        && matches!(
                            analysis.instructions[block.instructions.end - 1].opcode,
                            0x28..=0x2a
                        );
                    if !terminal_goto {
                        statements_by_block[block_index].push(statement);
                    }
                }
            }
            if let Some(exception) = block.exception {
                for handler in &analysis.exceptions[exception].handlers {
                    if let Some(handler_block) = handler.block
                        && !visited[handler_block]
                    {
                        let handler_type = handler
                            .type_idx
                            .map(|index| dex.type_descriptor(index).map(str::to_owned))
                            .transpose()?
                            .or_else(|| Some("Ljava/lang/Throwable;".to_owned()));
                        visited[handler_block] = true;
                        queue.push_back((handler_block, handler_type));
                    }
                }
            }
            for edge in &block.successors {
                if !visited[edge.block] {
                    visited[edge.block] = true;
                    queue.push_back((edge.block, None));
                }
            }
        }
    }

    if preserve_parameter_types && let Some(entry_stmts) = statements_by_block.first() {
        apply_kotlin_parameter_names(&mut lowerer.variables, entry_stmts);
    }

    let mut blocks = Vec::with_capacity(analysis.blocks.len());
    for (block_index, block) in analysis.blocks.iter().enumerate() {
        let statements = std::mem::take(&mut statements_by_block[block_index]);
        let last_opcode = analysis.instructions[block.instructions.end - 1].opcode;
        let kind = match last_opcode {
            0x0e..=0x11 => BlockKind::Return,
            0x27 => BlockKind::Throw,
            0x2b..=0x2c => BlockKind::Switch,
            0x32..=0x3d => BlockKind::Condition,
            _ => BlockKind::Statement,
        };
        let successors: Vec<_> = block.successors.iter().map(|edge| edge.block).collect();
        let (branch_false, branch_true) = if kind == BlockKind::Condition {
            match successors.as_slice() {
                [fallthrough, target, ..] => (Some(*fallthrough), Some(*target)),
                [target] => (Some(*target), Some(*target)),
                _ => (None, None),
            }
        } else {
            (None, None)
        };
        let mut exception_handlers = Vec::new();
        if let Some(index) = block.exception {
            for handler in &analysis.exceptions[index].handlers {
                let Some(handler_block) = handler.block else {
                    continue;
                };
                let ty = handler
                    .type_idx
                    .map(|index| dex.type_descriptor(index).map(str::to_owned))
                    .transpose()?;
                exception_handlers.push((handler_block, ty));
            }
        }
        let exception_successors = exception_handlers
            .iter()
            .map(|(handler, _)| *handler)
            .collect();
        blocks.push(LoweredBlock {
            start: block.start,
            end: block.end,
            kind,
            statements,
            predecessors: block.predecessors.iter().map(|edge| edge.block).collect(),
            successors,
            exception_successors,
            exception_handlers,
            branch_true,
            branch_false,
            exception: block.exception,
        });
    }

    Ok(Some(LoweredMethod {
        variables: lowerer.variables,
        blocks,
    }))
}

struct Lowerer<'a> {
    dex: &'a DexFile<'a>,
    variables: VariableTable,
    preserve_parameter_types: bool,
}

impl<'a> Lowerer<'a> {
    fn new(
        dex: &'a DexFile<'a>,
        method: &EncodedMethod,
        code: &CodeItem<'_>,
        resolved: ResolvedMethod<'_>,
        preserve_parameter_types: bool,
    ) -> Result<Self> {
        let mut variables = VariableTable::default();
        let mut register = code
            .registers_size
            .checked_sub(code.ins_size)
            .ok_or_else(|| {
                Error::data(
                    method.code_off as usize,
                    "code ins_size exceeds registers_size",
                )
            })?;

        if method.access_flags & 0x8 == 0 {
            let id = variables.register(register);
            variables.set_declared(id, true);
            variables.set_is_this(id, true);
            variables.get_mut(id).expect("inserted register").ty =
                Some(Cow::Owned(resolved.class.to_owned()));
            register = register
                .checked_add(1)
                .ok_or_else(|| Error::data(method.code_off as usize, "register overflow"))?;
        }
        let debug_names = if preserve_parameter_types {
            dex.debug_parameter_names(code.debug_info_off, resolved.parameter_type_indices.len())
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        for (idx, parameter_index) in resolved.parameter_type_indices.iter().enumerate() {
            let parameter = dex.type_descriptor(*parameter_index)?;
            let id = variables.register(register);
            variables.set_declared(id, true);
            variables.get_mut(id).expect("inserted register").ty =
                Some(Cow::Owned(parameter.to_owned()));
            if let Some(Some(raw_name)) = debug_names.get(idx)
                && let Some(clean) = sanitize_identifier(raw_name)
            {
                variables.set_custom_name(id, format!("{register}_{clean}"));
            }
            register = register
                .checked_add(type_width(parameter))
                .ok_or_else(|| Error::data(method.code_off as usize, "register overflow"))?;
        }

        Ok(Self {
            dex,
            variables,
            preserve_parameter_types,
        })
    }

    fn variable(&mut self, register: u16) -> Value {
        Value::Variable(self.variables.register(register))
    }

    fn assign(&mut self, register: u16, rhs: Value, kind: AssignKind) -> Statement {
        let lhs = self.variables.register(register);
        if !self.preserve_parameter_types || !self.variables.is_declared(lhs) {
            let inferred_type = match &rhs {
                Value::Variable(source) => self
                    .variables
                    .get(*source)
                    .and_then(|variable| variable.ty.clone()),
                Value::ArrayLoad(load) => array_element_type(&load.array, &self.variables)
                    .or_else(|| load.ty.map(Cow::Borrowed)),
                Value::Constant(constant) => Some(Cow::Borrowed(constant.ty())),
                Value::NewInstance { ty } => Some(Cow::Owned(ty.clone())),
                Value::NewArray(array) => Some(Cow::Owned(array.ty.clone())),
                Value::FilledArray(array) => Some(Cow::Owned(array.ty.clone())),
                Value::CheckCast(cast) => Some(Cow::Owned(cast.descriptor.clone())),
                Value::Binary(binary) => Some(Cow::Borrowed(binary.ty)),
                Value::Unary(unary) => Some(Cow::Borrowed(unary.ty)),
                Value::Invoke(invoke) => Some(Cow::Owned(invoke.method.return_type.clone())),
                Value::ArrayLength(_) => Some(Cow::Borrowed("I")),
                Value::InstanceField(field) => Some(Cow::Owned(field.field.ty.to_string())),
                Value::StaticField(field) => Some(Cow::Owned(field.ty.to_string())),
                Value::BaseClass { .. } => Some(Cow::Borrowed("Ljava/lang/Class;")),
            };
            if let Some(ty) = inferred_type {
                self.variables.get_mut(lhs).expect("registered variable").ty = Some(ty);
            }
        }
        Statement::Assign {
            lhs: Some(lhs),
            rhs,
            kind,
        }
    }

    fn lower(
        &mut self,
        instruction: &DecodedInstruction,
        instructions: &[DecodedInstruction],
    ) -> Result<Option<Statement>> {
        let opcode = instruction.opcode;
        let statement = match opcode {
            0x0100 | 0x0200 | 0x0300 => return Ok(None),
            0x00 => Statement::Nop,
            0x01..=0x09 => {
                let (a, b) = registers2(instruction)?;
                let rhs = self.variable(b);
                self.assign(a, rhs, AssignKind::Move)
            }
            0x0a..=0x0c => {
                let a = register(instruction)?;
                let result = self.variables.last_result().ok_or_else(|| {
                    Error::data(
                        instruction.offset as usize,
                        "move-result has no preceding invoke result",
                    )
                })?;
                self.assign(a, Value::Variable(result), AssignKind::MoveResult)
            }
            0x0d => {
                let lhs = self.variables.register(register(instruction)?);
                Statement::MoveException { lhs, ty: None }
            }
            0x0e => Statement::Return(None),
            0x0f..=0x11 => Statement::Return(Some(self.variable(register(instruction)?))),
            0x12..=0x19 => {
                let (a, value) = register_literal(instruction)?;
                let ty = if opcode >= 0x16 { "J" } else { "I" };
                self.assign(a, integer(value, ty), AssignKind::Normal)
            }
            0x1a | 0x1b => {
                let (a, index) = register_index(instruction)?;
                let value = self.dex.string(index)?.to_owned();
                self.assign(
                    a,
                    Value::Constant(Constant {
                        value: ConstantValue::String(value),
                    }),
                    AssignKind::Normal,
                )
            }
            0x1c => {
                let (a, index) = register_index(instruction)?;
                let descriptor = self.dex.type_descriptor(index)?.to_owned();
                self.assign(
                    a,
                    Value::BaseClass {
                        name: descriptor_to_java(&descriptor, self.preserve_parameter_types),
                    },
                    AssignKind::Normal,
                )
            }
            0x1d | 0x1e => return Ok(None),
            0x1f => {
                let (a, index) = register_index(instruction)?;
                let descriptor = self.dex.type_descriptor(index)?.to_owned();
                let source = self.variable(a);
                self.assign(
                    a,
                    Value::CheckCast(Box::new(CheckCastExpression {
                        value: source,
                        ty: descriptor_to_java(&descriptor, self.preserve_parameter_types),
                        descriptor,
                    })),
                    AssignKind::Normal,
                )
            }
            0x20 => {
                let (a, b, index) = registers2_index(instruction)?;
                let descriptor = self.dex.type_descriptor(index)?.to_owned();
                let rhs = Value::Binary(Box::new(BinaryExpression {
                    op: BinaryOp::InstanceOf,
                    left: self.variable(b),
                    right: Value::BaseClass {
                        name: descriptor_to_java(&descriptor, self.preserve_parameter_types),
                    },
                    ty: "Z",
                }));
                self.assign(a, rhs, AssignKind::Normal)
            }
            0x21 => {
                let (a, b) = registers2(instruction)?;
                let array = self.variable(b);
                self.assign(
                    a,
                    Value::ArrayLength(Box::new(array)),
                    AssignKind::Normal,
                )
            }
            0x22 => {
                let (a, index) = register_index(instruction)?;
                let ty = self.dex.type_descriptor(index)?.to_owned();
                self.assign(a, Value::NewInstance { ty }, AssignKind::Normal)
            }
            0x23 => {
                let (a, b, index) = registers2_index(instruction)?;
                let ty = self.dex.type_descriptor(index)?.to_owned();
                let size = self.variable(b);
                self.assign(
                    a,
                    Value::NewArray(Box::new(NewArrayExpression { size, ty })),
                    AssignKind::Normal,
                )
            }
            0x24 | 0x25 => self.lower_filled_array(instruction)?,
            0x26 => self.lower_fill_array(instruction, instructions)?,
            0x27 => Statement::Throw(self.variable(register(instruction)?)),
            0x28..=0x2a => Statement::Nop,
            0x2b | 0x2c => self.lower_switch(instruction, instructions)?,
            0x2d..=0x31 => {
                let (a, b, c) = registers3(instruction)?;
                let ty = match opcode {
                    0x2d | 0x2e => "F",
                    0x2f | 0x30 => "D",
                    _ => "J",
                };
                let left = self.variable(b);
                let right = self.variable(c);
                self.assign(
                    a,
                    Value::Binary(Box::new(BinaryExpression {
                        op: BinaryOp::Compare,
                        left,
                        right,
                        ty,
                    })),
                    AssignKind::Normal,
                )
            }
            0x32..=0x37 => {
                let (registers, count, _) = branch(instruction)?;
                debug_assert_eq!(count, 2);
                Statement::Condition {
                    op: condition_op(opcode),
                    left: self.variable(registers[0]),
                    right: Some(self.variable(registers[1])),
                }
            }
            0x38..=0x3d => {
                let (registers, count, _) = branch(instruction)?;
                debug_assert_eq!(count, 1);
                Statement::Condition {
                    op: condition_op(opcode),
                    left: self.variable(registers[0]),
                    right: None,
                }
            }
            0x44..=0x4a => {
                let (a, b, c) = registers3(instruction)?;
                let array = self.variable(b);
                let index = self.variable(c);
                self.assign(
                    a,
                    Value::ArrayLoad(Box::new(ArrayLoadExpression {
                        array,
                        index,
                        ty: array_opcode_type(opcode),
                    })),
                    AssignKind::Normal,
                )
            }
            0x4b..=0x51 => {
                let (a, b, c) = registers3(instruction)?;
                Statement::ArrayStore(Box::new(ArrayStoreStatement {
                    value: self.variable(a),
                    array: self.variable(b),
                    index: self.variable(c),
                    ty: array_opcode_type(opcode),
                }))
            }
            0x52..=0x58 => {
                let (a, b, index) = registers2_index(instruction)?;
                let field = self.field(index)?;
                let object = self.variable(b);
                self.assign(
                    a,
                    Value::InstanceField(Box::new(InstanceFieldExpression {
                        object,
                        field,
                    })),
                    AssignKind::Normal,
                )
            }
            0x59..=0x5f => {
                let (a, b, index) = registers2_index(instruction)?;
                Statement::InstancePut(Box::new(InstancePutStatement {
                    value: self.variable(a),
                    object: self.variable(b),
                    field: self.field(index)?,
                }))
            }
            0x60..=0x66 => {
                let (a, index) = register_index(instruction)?;
                let field = self.field(index)?;
                self.assign(a, Value::StaticField(Box::new(field)), AssignKind::Normal)
            }
            0x67..=0x6d => {
                let (a, index) = register_index(instruction)?;
                Statement::StaticPut {
                    value: self.variable(a),
                    field: self.field(index)?,
                }
            }
            0x6e..=0x72 | 0x74..=0x78 => self.lower_invoke(instruction)?,
            0x7b..=0x80 => {
                let (a, b) = registers2(instruction)?;
                let (op, ty) = match opcode {
                    0x7b => (UnaryOp::Negate, "I"),
                    0x7c => (UnaryOp::Not, "I"),
                    0x7d => (UnaryOp::Negate, "J"),
                    0x7e => (UnaryOp::Not, "J"),
                    0x7f => (UnaryOp::Negate, "F"),
                    _ => (UnaryOp::Negate, "D"),
                };
                let value = self.variable(b);
                self.assign(
                    a,
                    Value::Unary(Box::new(UnaryExpression {
                        op,
                        cast: None,
                        value,
                        ty,
                    })),
                    AssignKind::Normal,
                )
            }
            0x81..=0x8f => {
                let (a, b) = registers2(instruction)?;
                let ty = conversion_type(opcode);
                let value = self.variable(b);
                self.assign(
                    a,
                    Value::Unary(Box::new(UnaryExpression {
                        op: UnaryOp::Cast,
                        cast: Some(primitive_cast(ty)),
                        value,
                        ty,
                    })),
                    AssignKind::Normal,
                )
            }
            0x90..=0xaf => self.lower_binary(instruction, false)?,
            0xb0..=0xcf => self.lower_binary(instruction, true)?,
            0xd0..=0xe2 => self.lower_literal(instruction)?,
            _ => {
                return Err(Error::data(
                    instruction.offset as usize,
                    "opcode is not supported by DAD lowering",
                ));
            }
        };
        Ok(Some(statement))
    }

    fn field(&self, index: u32) -> Result<FieldReference> {
        let field = self.dex.field_metadata(index)?;
        Ok(FieldReference {
            class: field.class,
            ty: field.ty,
            name: field.name,
        })
    }

    fn lower_filled_array(&mut self, instruction: &DecodedInstruction) -> Result<Statement> {
        let (registers, count, index) = invoke_registers(instruction)?;
        let ty = self.dex.type_descriptor(index)?.to_owned();
        let arguments = registers[..usize::from(count)]
            .iter()
            .map(|register| self.variable(*register))
            .collect();
        let result = self.variables.temporary();
        self.variables
            .get_mut(result)
            .expect("created temporary")
            .ty = Some(Cow::Owned(ty.clone()));
        Ok(Statement::Assign {
            lhs: Some(result),
            rhs: Value::FilledArray(Box::new(FilledArrayExpression {
                size: count,
                ty,
                arguments,
            })),
            kind: AssignKind::Normal,
        })
    }

    fn lower_fill_array(
        &mut self,
        instruction: &DecodedInstruction,
        instructions: &[DecodedInstruction],
    ) -> Result<Statement> {
        let (register, relative) = payload_offset(instruction)?;
        let target = relative_target(instruction.offset, relative).ok_or_else(|| {
            Error::data(
                instruction.offset as usize,
                "fill-array payload offset overflow",
            )
        })?;
        let payload = instructions
            .iter()
            .find(|candidate| candidate.offset == target)
            .ok_or_else(|| {
                Error::data(instruction.offset as usize, "fill-array payload is missing")
            })?;
        let Operands::Payload(Payload::FillArray {
            element_width,
            size,
            data,
        }) = &payload.operands
        else {
            return Err(Error::data(
                instruction.offset as usize,
                "fill-array target is not an array payload",
            ));
        };
        Ok(Statement::FillArray {
            array: self.variable(register),
            element_width: *element_width,
            size: *size,
            data: data.clone(),
        })
    }

    fn lower_switch(
        &mut self,
        instruction: &DecodedInstruction,
        instructions: &[DecodedInstruction],
    ) -> Result<Statement> {
        let (register, relative) = payload_offset(instruction)?;
        let target = relative_target(instruction.offset, relative).ok_or_else(|| {
            Error::data(
                instruction.offset as usize,
                "switch payload offset overflow",
            )
        })?;
        let payload = instructions
            .iter()
            .find(|candidate| candidate.offset == target)
            .ok_or_else(|| Error::data(instruction.offset as usize, "switch payload is missing"))?;
        let values = match &payload.operands {
            Operands::Payload(Payload::PackedSwitch { first_key, targets }) => (0..targets.len())
                .map(|index| first_key.wrapping_add(index as i32))
                .collect(),
            Operands::Payload(Payload::SparseSwitch { keys, .. }) => keys.clone(),
            _ => {
                return Err(Error::data(
                    instruction.offset as usize,
                    "switch target is not a switch payload",
                ));
            }
        };
        Ok(Statement::Switch {
            value: self.variable(register),
            payload_offset: relative,
            values,
        })
    }

    fn lower_invoke(&mut self, instruction: &DecodedInstruction) -> Result<Statement> {
        let (registers, count, method_index) = invoke_registers(instruction)?;
        let resolved = self.dex.method(method_index)?;
        let return_type = resolved.return_type;
        let kind = match instruction.opcode {
            0x6e | 0x74 => InvokeKind::Virtual,
            0x6f | 0x75 => InvokeKind::Super,
            0x70 | 0x76 => InvokeKind::Direct,
            0x71 | 0x77 => InvokeKind::Static,
            0x72 | 0x78 => InvokeKind::Interface,
            _ => unreachable!(),
        };
        let used = &registers[..usize::from(count)];
        let (base, argument_registers) = if kind == InvokeKind::Static {
            (
                Value::BaseClass {
                    name: descriptor_to_java(resolved.class, self.preserve_parameter_types),
                },
                used,
            )
        } else {
            let first = *used.first().ok_or_else(|| {
                Error::data(
                    instruction.offset as usize,
                    "instance invoke has no receiver",
                )
            })?;
            let base = if kind == InvokeKind::Super {
                Value::BaseClass {
                    name: "super".to_owned(),
                }
            } else {
                self.variable(first)
            };
            (base, &used[1..])
        };
        let (arguments, parameters) = self.invoke_arguments(
            resolved.parameter_type_indices,
            argument_registers,
            instruction.offset,
        )?;
        let rhs = Value::Invoke(Box::new(InvokeExpression {
            kind,
            method: MethodReference {
                class: resolved.class.to_owned(),
                name: resolved.name.to_owned(),
                parameters,
                return_type: return_type.to_owned(),
            },
            base,
            arguments,
        }));

        let lhs = if return_type != "V" {
            let result = self.variables.temporary();
            self.variables
                .get_mut(result)
                .expect("created temporary")
                .ty = Some(Cow::Owned(return_type.to_owned()));
            Some(result)
        } else if kind == InvokeKind::Direct {
            let receiver = *used.first().ok_or_else(|| {
                Error::data(instruction.offset as usize, "direct invoke has no receiver")
            })?;
            let receiver = self.variables.register(receiver);
            if self.variables.is_this(receiver) {
                None
            } else {
                if resolved.name != "<init>"
                    && (!self.preserve_parameter_types || !self.variables.is_declared(receiver))
                {
                    self.variables
                        .get_mut(receiver)
                        .expect("registered receiver")
                        .ty = Some(Cow::Owned(return_type.to_owned()));
                }
                self.variables.set_last_result(receiver);
                Some(receiver)
            }
        } else if kind == InvokeKind::Super && instruction.opcode == 0x75 {
            let receiver = self.variables.register(used[0]);
            if !self.preserve_parameter_types || !self.variables.is_declared(receiver) {
                self.variables
                    .get_mut(receiver)
                    .expect("registered receiver")
                    .ty = Some(Cow::Borrowed("V"));
            }
            self.variables.set_last_result(receiver);
            Some(receiver)
        } else {
            None
        };
        Ok(Statement::Assign {
            lhs,
            rhs,
            kind: AssignKind::Normal,
        })
    }

    fn invoke_arguments(
        &mut self,
        parameter_type_indices: &[u32],
        registers: &[u16],
        offset: u32,
    ) -> Result<(Vec<Value>, Vec<String>)> {
        let mut required = 0;
        let mut parameters = Vec::with_capacity(parameter_type_indices.len());
        for parameter_index in parameter_type_indices {
            let parameter = self.dex.type_descriptor(*parameter_index)?;
            required += usize::from(type_width(parameter));
            parameters.push(parameter.to_owned());
        }
        if required > registers.len() {
            return Err(Error::data(
                offset as usize,
                "invoke register count does not cover method parameters",
            ));
        }
        let mut arguments = Vec::with_capacity(parameters.len());
        let mut register_index = 0;
        for parameter in &parameters {
            arguments.push(self.variable(registers[register_index]));
            register_index += usize::from(type_width(parameter));
        }
        Ok((arguments, parameters))
    }

    fn lower_binary(
        &mut self,
        instruction: &DecodedInstruction,
        two_address: bool,
    ) -> Result<Statement> {
        let normalized = if two_address {
            instruction.opcode - 0xb0
        } else {
            instruction.opcode - 0x90
        };
        let (op, ty) = binary_spec(normalized);
        let (a, b, c) = if two_address {
            let (a, b) = registers2(instruction)?;
            (a, a, b)
        } else {
            registers3(instruction)?
        };
        let left = self.variable(b);
        let right = self.variable(c);
        Ok(self.assign(
            a,
            Value::Binary(Box::new(BinaryExpression {
                op,
                left,
                right,
                ty,
            })),
            AssignKind::Normal,
        ))
    }

    fn lower_literal(&mut self, instruction: &DecodedInstruction) -> Result<Statement> {
        let (a, b, mut literal) = registers2_literal(instruction)?;
        let index = instruction.opcode
            - if instruction.opcode < 0xd8 {
                0xd0
            } else {
                0xd8
            };
        let mut op = match index {
            0 => BinaryOp::Add,
            1 => BinaryOp::Subtract,
            2 => BinaryOp::Multiply,
            3 => BinaryOp::Divide,
            4 => BinaryOp::Remainder,
            5 => BinaryOp::And,
            6 => BinaryOp::Or,
            7 => BinaryOp::Xor,
            8 => BinaryOp::ShiftLeft,
            9 => BinaryOp::ShiftRight,
            10 => BinaryOp::UnsignedShiftRight,
            _ => unreachable!(),
        };
        let reverse = matches!(instruction.opcode, 0xd1 | 0xd9);
        if instruction.opcode == 0xd8 && literal < 0 {
            literal = -literal;
            op = BinaryOp::Subtract;
        }
        let variable = self.variable(b);
        let constant = integer(i64::from(literal), "I");
        let (left, right) = if reverse {
            (constant, variable)
        } else {
            (variable, constant)
        };
        Ok(self.assign(
            a,
            Value::Binary(Box::new(BinaryExpression {
                op,
                left,
                right,
                ty: "I",
            })),
            AssignKind::Normal,
        ))
    }
}

fn integer(value: i64, ty: &'static str) -> Value {
    Value::Constant(Constant {
        value: ConstantValue::Integer { value, ty },
    })
}

fn array_element_type(
    array: &Value,
    variables: &VariableTable,
) -> Option<Cow<'static, str>> {
    let descriptor = match array {
        Value::Variable(variable) => variables
            .get(*variable)
            .and_then(|variable| variable.ty.as_deref()),
        Value::InstanceField(field) => Some(field.field.ty.as_ref()),
        Value::StaticField(field) => Some(field.ty.as_ref()),
        Value::CheckCast(cast) => Some(cast.descriptor.as_str()),
        _ => None,
    }?;
    Some(Cow::Owned(
        descriptor
            .strip_prefix('[')
            .unwrap_or(descriptor)
            .to_owned(),
    ))
}

fn condition_op(opcode: u16) -> BinaryOp {
    match (opcode - 0x32) % 6 {
        0 => BinaryOp::Equal,
        1 => BinaryOp::NotEqual,
        2 => BinaryOp::Less,
        3 => BinaryOp::GreaterEqual,
        4 => BinaryOp::Greater,
        _ => BinaryOp::LessEqual,
    }
}

fn array_opcode_type(opcode: u16) -> Option<&'static str> {
    match (opcode - 0x44) % 7 {
        0 => None,
        1 => Some("W"),
        2 => Some("O"),
        3 => Some("Z"),
        4 => Some("B"),
        5 => Some("C"),
        _ => Some("S"),
    }
}

fn primitive_cast(descriptor: &str) -> &'static str {
    match descriptor {
        "B" => "(byte)",
        "S" => "(short)",
        "C" => "(char)",
        "I" => "(int)",
        "J" => "(long)",
        "F" => "(float)",
        "D" => "(double)",
        _ => unreachable!(),
    }
}

fn conversion_type(opcode: u16) -> &'static str {
    match opcode {
        0x81 | 0x88 | 0x8b => "J",
        0x82 | 0x85 | 0x8c => "F",
        0x83 | 0x86 | 0x89 => "D",
        0x84 | 0x87 | 0x8a => "I",
        0x8d => "B",
        0x8e => "C",
        0x8f => "S",
        _ => unreachable!(),
    }
}

fn binary_spec(index: u16) -> (BinaryOp, &'static str) {
    const OPERATORS: [BinaryOp; 11] = [
        BinaryOp::Add,
        BinaryOp::Subtract,
        BinaryOp::Multiply,
        BinaryOp::Divide,
        BinaryOp::Remainder,
        BinaryOp::And,
        BinaryOp::Or,
        BinaryOp::Xor,
        BinaryOp::ShiftLeft,
        BinaryOp::ShiftRight,
        BinaryOp::UnsignedShiftRight,
    ];
    match index {
        0..=10 => (OPERATORS[index as usize], "I"),
        11..=21 => (OPERATORS[(index - 11) as usize], "J"),
        22..=26 => (OPERATORS[(index - 22) as usize], "F"),
        27..=31 => (OPERATORS[(index - 27) as usize], "D"),
        _ => unreachable!(),
    }
}

fn descriptor_to_java(descriptor: &str, enhanced: bool) -> String {
    if let Some(element) = descriptor.strip_prefix('[') {
        return format!("{}[]", descriptor_to_java(element, enhanced));
    }
    match descriptor {
        "V" => "void".to_owned(),
        "Z" => "boolean".to_owned(),
        "B" => "byte".to_owned(),
        "S" => "short".to_owned(),
        "C" => "char".to_owned(),
        "I" => "int".to_owned(),
        "J" => "long".to_owned(),
        "F" => "float".to_owned(),
        "D" => "double".to_owned(),
        "O" => "Object".to_owned(),
        _ if descriptor.starts_with('L') && descriptor.ends_with(';') => {
            let class = descriptor[1..descriptor.len() - 1].replace('/', ".");
            if enhanced {
                if let Some(rest) = class.strip_prefix("java.lang.")
                    && !rest.contains('.')
                {
                    rest.to_owned()
                } else {
                    class
                }
            } else {
                class
                    .strip_prefix("java.lang.")
                    .unwrap_or(&class)
                    .to_owned()
            }
        }
        _ => descriptor.to_owned(),
    }
}

fn type_width(descriptor: &str) -> u16 {
    if matches!(descriptor, "J" | "D") {
        2
    } else {
        1
    }
}

fn relative_target(offset: u32, relative_units: i32) -> Option<u32> {
    let target = i64::from(offset) + i64::from(relative_units) * 2;
    u32::try_from(target).ok()
}

fn register(instruction: &DecodedInstruction) -> Result<u16> {
    let Operands::Register(register) = instruction.operands else {
        return Err(invalid_operands(instruction));
    };
    Ok(register)
}

fn registers2(instruction: &DecodedInstruction) -> Result<(u16, u16)> {
    let Operands::Registers2 { a, b } = instruction.operands else {
        return Err(invalid_operands(instruction));
    };
    Ok((a, b))
}

fn registers3(instruction: &DecodedInstruction) -> Result<(u16, u16, u16)> {
    let Operands::Registers3 { a, b, c } = instruction.operands else {
        return Err(invalid_operands(instruction));
    };
    Ok((a, b, c))
}

fn register_literal(instruction: &DecodedInstruction) -> Result<(u16, i64)> {
    let Operands::RegisterLiteral { a, value } = instruction.operands else {
        return Err(invalid_operands(instruction));
    };
    Ok((a, value))
}

fn registers2_literal(instruction: &DecodedInstruction) -> Result<(u16, u16, i32)> {
    let Operands::Registers2Literal { a, b, value } = instruction.operands else {
        return Err(invalid_operands(instruction));
    };
    Ok((a, b, value))
}

fn register_index(instruction: &DecodedInstruction) -> Result<(u16, u32)> {
    let Operands::RegisterIndex { a, index } = instruction.operands else {
        return Err(invalid_operands(instruction));
    };
    Ok((a, index))
}

fn registers2_index(instruction: &DecodedInstruction) -> Result<(u16, u16, u32)> {
    let Operands::Registers2Index { a, b, index } = instruction.operands else {
        return Err(invalid_operands(instruction));
    };
    Ok((a, b, index))
}

fn payload_offset(instruction: &DecodedInstruction) -> Result<(u16, i32)> {
    let Operands::PayloadOffset { register, relative } = instruction.operands else {
        return Err(invalid_operands(instruction));
    };
    Ok((register, relative))
}

fn branch(instruction: &DecodedInstruction) -> Result<([u16; 2], u8, i32)> {
    let Operands::Branch {
        registers,
        count,
        relative,
    } = instruction.operands
    else {
        return Err(invalid_operands(instruction));
    };
    Ok((registers, count, relative))
}

fn invoke_registers(instruction: &DecodedInstruction) -> Result<(Vec<u16>, u8, u32)> {
    match instruction.operands {
        Operands::Invoke {
            registers,
            count,
            index,
        } => Ok((registers.to_vec(), count, index)),
        Operands::InvokeRange {
            start,
            count,
            index,
        } => {
            let end = start.checked_add(u16::from(count)).ok_or_else(|| {
                Error::data(
                    instruction.offset as usize,
                    "invoke register range overflow",
                )
            })?;
            Ok(((start..end).collect(), count, index))
        }
        _ => Err(invalid_operands(instruction)),
    }
}

fn invalid_operands(instruction: &DecodedInstruction) -> Error {
    Error::data(
        instruction.offset as usize,
        "opcode has an unexpected operand format",
    )
}

fn sanitize_identifier(raw: &str) -> Option<&str> {
    let trimmed = raw
        .trim()
        .trim_start_matches('<')
        .trim_end_matches('>');
    if trimmed.is_empty()
        || trimmed == "this"
        || trimmed == "super"
        || trimmed == "null"
        || trimmed == "true"
        || trimmed == "false"
        || trimmed.len() > 40
    {
        return None;
    }
    let mut chars = trimmed.chars();
    let first = chars.next()?;
    if !first.is_ascii_alphabetic() && first != '_' && first != '$' {
        return None;
    }
    if !chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$') {
        return None;
    }
    Some(trimmed)
}

fn apply_kotlin_parameter_names(variables: &mut VariableTable, entry_stmts: &[Statement]) {
    let mut name_regs: std::collections::HashMap<u16, &str> = std::collections::HashMap::new();
    for stmt in entry_stmts.iter().take(24) {
        match stmt {
            Statement::Assign {
                lhs: Some(VariableId::Register(reg)),
                rhs:
                    Value::Constant(Constant {
                        value: ConstantValue::String(s),
                    }),
                ..
            } => {
                name_regs.insert(*reg, s.as_str());
            }
            Statement::Assign {
                lhs: None,
                rhs: Value::Invoke(inv),
                ..
            } if inv.kind == InvokeKind::Static
                && inv.method.return_type == "V"
                && inv.arguments.len() == 2
                && inv.method.parameters.len() == 2
                && inv.method.parameters[1] == "Ljava/lang/String;"
                && ((inv.method.class == "Lkotlin/jvm/internal/Intrinsics;"
                    && matches!(
                        inv.method.name.as_str(),
                        "checkNotNullParameter" | "checkParameterIsNotNull"
                    ))
                    || inv.method.parameters[0] == "Ljava/lang/Object;") =>
            {
                let param_id = match &inv.arguments[0] {
                    Value::Variable(id @ VariableId::Register(_))
                        if variables.is_declared(*id) && !variables.is_this(*id) =>
                    {
                        Some(*id)
                    }
                    _ => None,
                };
                let raw_name = match &inv.arguments[1] {
                    Value::Constant(Constant {
                        value: ConstantValue::String(s),
                    }) => Some(s.as_str()),
                    Value::Variable(VariableId::Register(r)) => name_regs.get(r).copied(),
                    _ => None,
                };
                if let (Some(param_id), Some(raw_name)) = (param_id, raw_name)
                    && let VariableId::Register(reg_num) = param_id
                    && variables.name(param_id).is_some_and(|n| !n.contains('_'))
                    && let Some(clean) = sanitize_identifier(raw_name)
                {
                    variables.set_custom_name(param_id, format!("{reg_num}_{clean}"));
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::VariableId;

    #[test]
    fn test_assign_base_class_sets_java_class_type_ut() {
        let bytes = std::fs::read("tests/fixtures/sample.dex").expect("read phase-one sample");
        let dex = DexFile::parse(&bytes).expect("parse sample.dex");
        let (method, code) = dex
            .classes()
            .iter()
            .flat_map(|class| class.methods())
            .find_map(|method| {
                let code = dex.code(method).ok()??;
                Some((method, code))
            })
            .expect("sample has a concrete method");
        let resolved = dex.method(method.method_idx).expect("resolve method");
        let mut lowerer =
            Lowerer::new(&dex, method, &code, resolved, false).expect("initialize lowering state");
        let destination_register = u16::MAX;

        lowerer.assign(
            destination_register,
            Value::BaseClass {
                name: "java.lang.String".to_owned(),
            },
            AssignKind::Normal,
        );

        assert_eq!(
            lowerer
                .variables
                .get(VariableId::Register(destination_register))
                .and_then(|variable| variable.ty.as_deref()),
            Some("Ljava/lang/Class;")
        );
    }

    #[test]
    fn test_assign_move_inherits_source_variable_type_bits_ut() {
        let bytes = std::fs::read("tests/fixtures/sample.dex").expect("read phase-one sample");
        let dex = DexFile::parse(&bytes).expect("parse sample.dex");
        let (method, code) = dex
            .classes()
            .iter()
            .flat_map(|class| class.methods())
            .filter(|method| method.access_flags & 0x8 == 0)
            .find_map(|method| {
                let code = dex.code(method).ok()??;
                Some((method, code))
            })
            .expect("sample has a concrete instance method");
        let source_register = code.registers_size - code.ins_size;
        let resolved = dex.method(method.method_idx).expect("resolve method");
        let mut lowerer =
            Lowerer::new(&dex, method, &code, resolved, false).expect("initialize lowering state");
        let source = lowerer.variable(source_register);
        let source_type = lowerer
            .variables
            .get(VariableId::Register(source_register))
            .and_then(|variable| variable.ty.clone())
            .expect("this parameter has a type");
        let destination_register = u16::MAX;

        lowerer.assign(destination_register, source, AssignKind::Move);

        assert_eq!(
            lowerer
                .variables
                .get(VariableId::Register(destination_register))
                .and_then(|variable| variable.ty.as_deref()),
            Some(source_type.as_ref())
        );
    }
}
