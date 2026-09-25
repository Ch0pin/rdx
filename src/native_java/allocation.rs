//! Typed, ordered expressions for delayed DEX constructor arguments.
use anyhow::{Context, Result, bail, ensure};
use std::collections::HashSet;

const MAX_DEPTH: usize = 32;
const MAX_NODES: usize = 256;
const MAX_CHARS: usize = 16 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Symbol {
    pub text: String,
    pub label: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Expr {
    Local(String),
    /// A typed conversion of an already materialized local, validated by argument().
    #[allow(dead_code)]
    ConvertedLocal {
        name: String,
        text: String,
    },
    #[allow(dead_code)] // constructed by production decoder, absent in standalone tests
    Boolean(bool),
    #[allow(dead_code)] // production decoder emits exact char overload arguments
    Char(i32),
    #[allow(dead_code)] // constructed by production decoder, absent in standalone tests
    Null,
    /// A typed input conversion; the operand retains its bound-local identity.
    Cast {
        ty: Symbol,
        value: Box<Expr>,
    },
    #[allow(dead_code)] // production decoder captures DEX check-cast effects
    CheckCast {
        site: usize,
        ty: Symbol,
        value: Box<Expr>,
    },
    #[allow(dead_code)] // constructed by the production decoder, not the renderer-only test module
    StringConstant {
        site: usize,
        literal: String,
    },
    #[allow(dead_code)] // constructed by the production decoder, not the renderer-only test module
    ClassConstant {
        site: usize,
        ty: Symbol,
    },
    FieldRead {
        site: usize,
        receiver: Box<Expr>,
        field: Symbol,
    },
    Call {
        site: usize,
        target: Box<Expr>,
        method: Symbol,
        args: Vec<Expr>,
    },
    #[allow(dead_code)] // constructed by the production decoder
    FilledArray {
        site: usize,
        ty: Symbol,
        elements: Vec<Expr>,
    },
    #[allow(dead_code)]
    Binary {
        site: usize,
        operator: &'static str,
        left: Box<Expr>,
        right: Box<Expr>,
    },
    #[allow(dead_code)]
    NewArray {
        site: usize,
        ty: Symbol,
        length: Box<Expr>,
    },
    #[allow(dead_code)]
    ArrayRead {
        site: usize,
        array: Box<Expr>,
        index: Box<Expr>,
    },
    #[allow(dead_code)]
    Unary {
        site: usize,
        prefix: String,
        suffix: &'static str,
        value: Box<Expr>,
    },
    #[allow(dead_code)]
    ArrayStore {
        site: usize,
        array: Box<Expr>,
        index: Box<Expr>,
        value: Box<Expr>,
    },
    #[allow(dead_code)]
    FieldStore {
        site: usize,
        receiver: Box<Expr>,
        field: Symbol,
        value: Box<Expr>,
    },
    Capture(usize),
    #[allow(dead_code)] // exercised by the standalone capture renderer tests
    New(Box<Allocation>),
    /// Child allocation whose arguments use the enclosing capture environment.
    #[allow(dead_code)]
    // constructed by the production decoder, absent in standalone renderer tests
    SharedNew {
        allocation: Box<Allocation>,
        constructor_label: String,
    },
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Capture {
    pub ty: String,
    pub name: String,
    pub expression: Expr,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Allocation {
    pub site: usize,
    pub constructor_site: usize,
    pub ty: Symbol,
    pub captures: Vec<Capture>,
    pub arguments: Vec<Expr>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Event {
    Allocate { site: usize, ty: String },
    StringResolution { site: usize },
    ClassResolution { site: usize, ty: String },
    CheckCast { site: usize, ty: String },
    Read { site: usize, field: String },
    Write { site: usize, field: String },
    Call { site: usize, method: String },
    Construct { site: usize, ty: String },
    Compute { site: usize },
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Link {
    pub start: usize,
    pub end: usize,
    pub label: String,
}
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Rendered {
    pub declarations: Vec<String>,
    pub declaration_links: Vec<Vec<Link>>,
    pub expression: String,
    pub links: Vec<Link>,
    pub events: Vec<Event>,
}

#[derive(Default)]
struct Fragment {
    text: String,
    links: Vec<Link>,
}
impl Fragment {
    fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            ..Self::default()
        }
    }
    fn symbol(symbol: &Symbol) -> Self {
        if symbol.label.is_empty() {
            return Self::plain(&symbol.text);
        }
        Self {
            text: symbol.text.clone(),
            links: vec![Link {
                start: 0,
                end: symbol.text.chars().count(),
                label: symbol.label.clone(),
            }],
        }
    }
    fn append(&mut self, other: Self) {
        let offset = self.text.chars().count();
        self.links.extend(other.links.into_iter().map(|mut link| {
            link.start += offset;
            link.end += offset;
            link
        }));
        self.text.push_str(&other.text);
    }
}

impl Allocation {
    /// Declarations have no initializers. Effects occur only inside the returned
    /// expression, whose site-specific trace must match the DEX trace exactly.
    pub(crate) fn render_checked(&self, dex_events: &[Event], locals: &[&str]) -> Result<Rendered> {
        let mut names = HashSet::new();
        let mut nodes = 0;
        self.validate(locals, &mut names, 0, &mut nodes)?;
        let mut declarations = Vec::new();
        self.declarations(&mut declarations)?;
        let mut events = Vec::new();
        let expression = self.render(&mut events)?;
        ensure!(
            events == dex_events,
            "allocation capture changes effect order"
        );
        ensure!(
            expression.text.chars().count() <= MAX_CHARS,
            "allocation capture output exceeds budget"
        );
        Ok(Rendered {
            declaration_links: vec![Vec::new(); declarations.len()],
            declarations,
            expression: expression.text,
            links: expression.links,
            events,
        })
    }
    /// JADX-compatible readable fallback: retain ordered capture statements,
    /// then allocate at the constructor. This intentionally moves allocation
    /// (including initialization/linkage/failure timing), not other effects.
    #[allow(dead_code)] // production decoder and dedicated staged tests
    pub(crate) fn render_staged_checked(
        &self,
        dex_events: &[Event],
        locals: &[&str],
    ) -> Result<Rendered> {
        self.render_staged_with_discarded(dex_events, locals, &[])
    }

    pub(crate) fn render_staged_with_discarded(
        &self,
        dex_events: &[Event],
        locals: &[&str],
        discarded: &[usize],
    ) -> Result<Rendered> {
        self.validate(locals, &mut HashSet::new(), 0, &mut 0)?;
        ensure!(
            discarded.iter().all(|index| *index < self.captures.len()),
            "invalid discarded capture"
        );
        let allocate = Event::Allocate {
            site: self.site,
            ty: self.ty.label.clone(),
        };
        let construct = Event::Construct {
            site: self.constructor_site,
            ty: self.ty.label.clone(),
        };
        ensure!(
            dex_events.first() == Some(&allocate) && dex_events.last() == Some(&construct),
            "staged allocation boundaries mismatch"
        );
        // Only backward capture dependencies are allowed. All effects must be
        // reachable from constructor arguments or explicitly retained discarded
        // effects (invoke results and check-casts). Other unused captures fail.
        let mut used: HashSet<usize> = discarded.iter().copied().collect();
        for arg in &self.arguments {
            arg.flat_dependencies(self.captures.len(), &mut used)?;
        }
        for index in (0..self.captures.len()).rev() {
            let mut dependencies = HashSet::new();
            self.captures[index]
                .expression
                .flat_dependencies(index, &mut dependencies)?;
            if used.contains(&index) {
                used.extend(dependencies);
            }
        }
        ensure!(
            used.len() == self.captures.len(),
            "unconsumed effectful allocation capture"
        );
        let mut events = Vec::new();
        let mut captures = CaptureRenderer::new(&self.captures, &mut events);
        let mut declarations = Vec::new();
        let mut declaration_links = Vec::new();
        let mut chars = 0usize;
        for (index, capture) in self.captures.iter().enumerate() {
            let assignment = captures.capture(index)?;
            let prefix = format!("{} ", capture.ty);
            // A void call has no Java assignment target. It is an explicitly
            // discarded capture so its statement remains at its DEX position.
            let (declaration, shift) = if capture.ty == "void" {
                ensure!(discarded.contains(&index), "void capture must be discarded");
                let start = format!("({} = ", capture.name).len();
                (
                    format!("{};", &assignment.text[start..assignment.text.len() - 1]),
                    -(start as isize),
                )
            } else {
                (
                    format!(
                        "{}{};",
                        prefix,
                        &assignment.text[1..assignment.text.len() - 1]
                    ),
                    prefix.chars().count() as isize - 1,
                )
            };
            declaration_links.push(
                assignment
                    .links
                    .into_iter()
                    .map(|link| Link {
                        start: link
                            .start
                            .checked_add_signed(shift)
                            .expect("capture link offset"),
                        end: link
                            .end
                            .checked_add_signed(shift)
                            .expect("capture link offset"),
                        label: link.label,
                    })
                    .collect(),
            );
            chars = chars.saturating_add(declaration.chars().count());
            ensure!(
                chars <= MAX_CHARS,
                "staged allocation output exceeds budget"
            );
            declarations.push(declaration);
        }
        let expression = self.render_shared(&mut captures)?;
        ensure!(
            chars.saturating_add(expression.text.chars().count()) <= MAX_CHARS,
            "staged allocation output exceeds budget"
        );
        // Readable staging may move allocation itself, including nested new,
        // but must preserve every observable argument/constructor effect.
        let effects = |events: &[Event]| {
            events
                .iter()
                .filter(|event| !matches!(event, Event::Allocate { .. }))
                .cloned()
                .collect::<Vec<_>>()
        };
        ensure!(
            effects(&events) == effects(dex_events),
            "staged allocation changes nonallocation effect order"
        );
        let allocations = |events: &[Event]| {
            let mut sites = events
                .iter()
                .filter_map(|event| match event {
                    Event::Allocate { site, ty } => Some((*site, ty.clone())),
                    _ => None,
                })
                .collect::<Vec<_>>();
            sites.sort();
            sites
        };
        ensure!(
            allocations(&events) == allocations(dex_events),
            "staged allocation changes allocation identities"
        );
        Ok(Rendered {
            declarations,
            declaration_links,
            expression: expression.text,
            links: expression.links,
            events,
        })
    }
    fn validate(
        &self,
        locals: &[&str],
        names: &mut HashSet<String>,
        depth: usize,
        nodes: &mut usize,
    ) -> Result<()> {
        ensure!(
            depth <= MAX_DEPTH,
            "allocation capture nesting exceeds budget"
        );
        *nodes += 1;
        ensure!(
            *nodes <= MAX_NODES,
            "allocation capture node budget exceeded"
        );
        ensure!(
            !self.ty.text.is_empty() && !self.ty.label.is_empty(),
            "invalid allocation type"
        );
        for capture in &self.captures {
            ensure!(
                !capture.ty.is_empty() && !capture.name.is_empty(),
                "invalid allocation capture declaration"
            );
            ensure!(
                names.insert(capture.name.clone()),
                "duplicate allocation capture name"
            );
            Self::validate_expr(&capture.expression, locals, names, depth + 1, nodes)?;
        }
        for argument in &self.arguments {
            Self::validate_expr(argument, locals, names, depth + 1, nodes)?;
        }
        Ok(())
    }
    fn validate_expr(
        expression: &Expr,
        locals: &[&str],
        names: &mut HashSet<String>,
        depth: usize,
        nodes: &mut usize,
    ) -> Result<()> {
        ensure!(
            depth <= MAX_DEPTH,
            "allocation capture nesting exceeds budget"
        );
        *nodes += 1;
        ensure!(
            *nodes <= MAX_NODES,
            "allocation capture node budget exceeded"
        );
        match expression {
            Expr::Local(name) => {
                ensure!(locals.contains(&name.as_str()), "unbound allocation local")
            }
            Expr::ConvertedLocal { name, text } => {
                ensure!(
                    locals.contains(&name.as_str()) && !text.is_empty(),
                    "unbound converted allocation local"
                );
            }
            Expr::Boolean(_) | Expr::Char(_) | Expr::Null => {}
            Expr::Cast { ty, value } | Expr::CheckCast { ty, value, .. } => {
                ensure!(
                    !ty.text.is_empty()
                        && (!ty.label.is_empty()
                            || (matches!(expression, Expr::Cast { .. })
                                && ty.text.ends_with("[]")
                                && matches!(
                                    ty.text.trim_end_matches("[]"),
                                    "boolean"
                                        | "byte"
                                        | "char"
                                        | "short"
                                        | "int"
                                        | "long"
                                        | "float"
                                        | "double"
                                ))),
                    "invalid allocation cast type"
                );
                Self::validate_expr(value, locals, names, depth + 1, nodes)?;
            }
            Expr::StringConstant { literal, .. } => {
                ensure!(
                    literal.starts_with('"') && literal.ends_with('"'),
                    "invalid allocation string literal"
                )
            }
            Expr::ClassConstant { ty, .. } => ensure!(
                !ty.text.is_empty() && !ty.label.is_empty(),
                "invalid allocation class literal"
            ),
            Expr::FilledArray { ty, elements, .. } => {
                ensure!(ty.text.ends_with("[]"), "invalid filled array type");
                for element in elements {
                    Self::validate_expr(element, locals, names, depth + 1, nodes)?;
                }
            }
            Expr::Binary {
                operator,
                left,
                right,
                ..
            } => {
                ensure!(
                    ["+", "-", "*", "/", "%", "&", "|", "^", "<<", ">>", ">>>"].contains(operator),
                    "invalid allocation operator"
                );
                Self::validate_expr(left, locals, names, depth + 1, nodes)?;
                Self::validate_expr(right, locals, names, depth + 1, nodes)?;
            }
            Expr::NewArray { ty, length, .. } => {
                ensure!(ty.text.ends_with("[]"), "invalid allocation array type");
                Self::validate_expr(length, locals, names, depth + 1, nodes)?;
            }
            Expr::ArrayRead { array, index, .. } => {
                Self::validate_expr(array, locals, names, depth + 1, nodes)?;
                Self::validate_expr(index, locals, names, depth + 1, nodes)?;
            }
            Expr::Unary {
                prefix,
                suffix,
                value,
                ..
            } => {
                ensure!(
                    [
                        "",
                        "-",
                        "~",
                        "(long) ",
                        "(float) ",
                        "(double) ",
                        "(int) ",
                        "(byte) ",
                        "(char) ",
                        "(short) "
                    ]
                    .contains(&prefix.as_str())
                        && matches!(*suffix, "" | ".length"),
                    "invalid allocation unary operation"
                );
                Self::validate_expr(value, locals, names, depth + 1, nodes)?;
            }
            Expr::ArrayStore {
                array,
                index,
                value,
                ..
            } => {
                Self::validate_expr(array, locals, names, depth + 1, nodes)?;
                Self::validate_expr(index, locals, names, depth + 1, nodes)?;
                Self::validate_expr(value, locals, names, depth + 1, nodes)?;
            }
            Expr::FieldStore {
                receiver,
                field,
                value,
                ..
            } => {
                ensure!(
                    !field.text.is_empty() && !field.label.is_empty(),
                    "invalid allocation field store"
                );
                Self::validate_expr(receiver, locals, names, depth + 1, nodes)?;
                Self::validate_expr(value, locals, names, depth + 1, nodes)?;
            }
            Expr::Capture(_) => {}
            Expr::FieldRead {
                receiver, field, ..
            } => {
                ensure!(
                    !field.text.is_empty() && !field.label.is_empty(),
                    "invalid field symbol"
                );
                Self::validate_expr(receiver, locals, names, depth + 1, nodes)?;
            }
            Expr::Call {
                target,
                method,
                args,
                ..
            } => {
                ensure!(
                    !method.text.is_empty() && !method.label.is_empty(),
                    "invalid call symbol"
                );
                Self::validate_expr(target, locals, names, depth + 1, nodes)?;
                for arg in args {
                    Self::validate_expr(arg, locals, names, depth + 1, nodes)?;
                }
            }
            Expr::New(allocation) | Expr::SharedNew { allocation, .. } => {
                allocation.validate(locals, names, depth + 1, nodes)?
            }
        }
        Ok(())
    }
    fn declarations(&self, out: &mut Vec<String>) -> Result<()> {
        for capture in &self.captures {
            out.push(format!("{} {};", capture.ty, capture.name));
            capture.expression.declarations(out)?;
        }
        for argument in &self.arguments {
            argument.declarations(out)?;
        }
        Ok(())
    }
    fn render(&self, events: &mut Vec<Event>) -> Result<Fragment> {
        let mut captures = CaptureRenderer::new(&self.captures, events);
        let result = self.render_shared(&mut captures)?;
        ensure!(
            captures.all_consumed(),
            "unconsumed effectful allocation capture"
        );
        Ok(result)
    }
    fn render_shared(&self, captures: &mut CaptureRenderer<'_>) -> Result<Fragment> {
        captures.events.push(Event::Allocate {
            site: self.site,
            ty: self.ty.label.clone(),
        });
        let mut result = Fragment::plain("new ");
        result.append(Fragment::symbol(&self.ty));
        result.append(Fragment::plain("("));
        for (index, argument) in self.arguments.iter().enumerate() {
            if index != 0 {
                result.append(Fragment::plain(", "));
            }
            result.append(captures.expression(argument)?);
        }
        result.append(Fragment::plain(")"));
        captures.events.push(Event::Construct {
            site: self.constructor_site,
            ty: self.ty.label.clone(),
        });
        Ok(result)
    }
}
impl Expr {
    fn flat_dependencies(&self, before: usize, used: &mut HashSet<usize>) -> Result<()> {
        match self {
            Self::Capture(index) => {
                ensure!(
                    *index < before,
                    "forward or cyclic staged capture dependency"
                );
                used.insert(*index);
            }
            Self::Cast { value, .. } | Self::CheckCast { value, .. } => {
                value.flat_dependencies(before, used)?
            }
            Self::Binary { left, right, .. } => {
                left.flat_dependencies(before, used)?;
                right.flat_dependencies(before, used)?;
            }
            Self::Unary { value, .. } => value.flat_dependencies(before, used)?,
            Self::NewArray { length, .. } => length.flat_dependencies(before, used)?,
            Self::ArrayRead { array, index, .. } => {
                array.flat_dependencies(before, used)?;
                index.flat_dependencies(before, used)?;
            }
            Self::ArrayStore {
                array,
                index,
                value,
                ..
            } => {
                array.flat_dependencies(before, used)?;
                index.flat_dependencies(before, used)?;
                value.flat_dependencies(before, used)?;
            }
            Self::FieldStore {
                receiver, value, ..
            } => {
                receiver.flat_dependencies(before, used)?;
                value.flat_dependencies(before, used)?;
            }
            Self::FieldRead { receiver, .. } => receiver.flat_dependencies(before, used)?,
            Self::Call { target, args, .. } => {
                target.flat_dependencies(before, used)?;
                for arg in args {
                    arg.flat_dependencies(before, used)?;
                }
            }
            Self::FilledArray { elements, .. } => {
                for element in elements {
                    element.flat_dependencies(before, used)?;
                }
            }
            Self::SharedNew { allocation, .. } => {
                ensure!(
                    allocation.captures.is_empty(),
                    "shared allocation has private captures"
                );
                for argument in &allocation.arguments {
                    argument.flat_dependencies(before, used)?;
                }
            }
            Self::New(_) => anyhow::bail!("private nested staged allocation is unsupported"),
            _ => {}
        }
        Ok(())
    }
    fn declarations(&self, out: &mut Vec<String>) -> Result<()> {
        match self {
            Self::Local(_)
            | Self::ConvertedLocal { .. }
            | Self::Boolean(_)
            | Self::Char(_)
            | Self::Null
            | Self::StringConstant { .. }
            | Self::ClassConstant { .. }
            | Self::Capture(_) => Ok(()),
            Self::Cast { value, .. } | Self::CheckCast { value, .. } => value.declarations(out),
            Self::Binary { left, right, .. } => {
                left.declarations(out)?;
                right.declarations(out)
            }
            Self::Unary { value, .. } => value.declarations(out),
            Self::NewArray { length, .. } => length.declarations(out),
            Self::ArrayRead { array, index, .. } => {
                array.declarations(out)?;
                index.declarations(out)
            }
            Self::ArrayStore {
                array,
                index,
                value,
                ..
            } => {
                array.declarations(out)?;
                index.declarations(out)?;
                value.declarations(out)
            }
            Self::FieldStore {
                receiver, value, ..
            } => {
                receiver.declarations(out)?;
                value.declarations(out)
            }
            Self::FieldRead { receiver, .. } => receiver.declarations(out),
            Self::Call { target, args, .. } => {
                target.declarations(out)?;
                for arg in args {
                    arg.declarations(out)?;
                }
                Ok(())
            }
            Self::FilledArray { elements, .. } => {
                for element in elements {
                    element.declarations(out)?;
                }
                Ok(())
            }
            Self::New(allocation) | Self::SharedNew { allocation, .. } => {
                allocation.declarations(out)
            }
        }
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum CaptureState {
    Waiting,
    Rendering,
    Done,
}
struct CaptureRenderer<'a> {
    captures: &'a [Capture],
    states: Vec<CaptureState>,
    next: usize,
    events: &'a mut Vec<Event>,
}
impl<'a> CaptureRenderer<'a> {
    fn new(captures: &'a [Capture], events: &'a mut Vec<Event>) -> Self {
        Self {
            captures,
            states: vec![CaptureState::Waiting; captures.len()],
            next: 0,
            events,
        }
    }
    fn all_consumed(&self) -> bool {
        self.next == self.captures.len()
    }
    fn expression(&mut self, expression: &Expr) -> Result<Fragment> {
        match expression {
            Expr::Local(name) => Ok(Fragment::plain(name)),
            Expr::ConvertedLocal { text, .. } => Ok(Fragment::plain(text)),
            Expr::Boolean(value) => Ok(Fragment::plain(if *value { "true" } else { "false" })),
            Expr::Null => Ok(Fragment::plain("null")),
            Expr::Char(value) => Ok(Fragment::plain(format!("(char) {value}"))),
            Expr::Cast { ty, value } => {
                let mut result = Fragment::plain("((");
                result.append(Fragment::symbol(ty));
                result.append(Fragment::plain(") "));
                result.append(self.expression(value)?);
                result.append(Fragment::plain(")"));
                Ok(result)
            }
            Expr::CheckCast { site, ty, value } => {
                let mut result = Fragment::plain("((");
                result.append(Fragment::symbol(ty));
                result.append(Fragment::plain(") "));
                result.append(self.expression(value)?);
                result.append(Fragment::plain(")"));
                self.events.push(Event::CheckCast {
                    site: *site,
                    ty: ty.label.clone(),
                });
                Ok(result)
            }
            Expr::StringConstant { site, literal } => {
                self.events.push(Event::StringResolution { site: *site });
                Ok(Fragment::plain(literal))
            }
            Expr::ClassConstant { site, ty } => {
                self.events.push(Event::ClassResolution {
                    site: *site,
                    ty: ty.label.clone(),
                });
                let mut result = Fragment::symbol(ty);
                result.append(Fragment::plain(".class"));
                Ok(result)
            }
            Expr::FieldRead {
                site,
                receiver,
                field,
            } => {
                let mut result = self.expression(receiver)?;
                result.append(Fragment::plain("."));
                result.append(Fragment::symbol(field));
                self.events.push(Event::Read {
                    site: *site,
                    field: field.label.clone(),
                });
                Ok(result)
            }
            Expr::Call {
                site,
                target,
                method,
                args,
            } => {
                let mut result = self.expression(target)?;
                result.append(Fragment::plain("."));
                result.append(Fragment::symbol(method));
                result.append(Fragment::plain("("));
                for (index, arg) in args.iter().enumerate() {
                    if index != 0 {
                        result.append(Fragment::plain(", "));
                    }
                    result.append(self.expression(arg)?);
                }
                result.append(Fragment::plain(")"));
                self.events.push(Event::Call {
                    site: *site,
                    method: method.label.clone(),
                });
                Ok(result)
            }
            Expr::FilledArray { site, ty, elements } => {
                let mut result = Fragment::plain("new ");
                result.append(Fragment::symbol(ty));
                result.append(Fragment::plain(" {"));
                self.events.push(Event::Allocate {
                    site: *site,
                    ty: ty.text.clone(),
                });
                for (index, element) in elements.iter().enumerate() {
                    if index != 0 {
                        result.append(Fragment::plain(", "));
                    }
                    result.append(self.expression(element)?);
                }
                result.append(Fragment::plain("}"));
                Ok(result)
            }
            Expr::Binary {
                site,
                operator,
                left,
                right,
            } => {
                let mut result = Fragment::plain("(");
                result.append(self.expression(left)?);
                result.append(Fragment::plain(format!(") {operator} (")));
                result.append(self.expression(right)?);
                result.append(Fragment::plain(")"));
                self.events.push(Event::Compute { site: *site });
                Ok(result)
            }
            Expr::NewArray { site, ty, length } => {
                let base = ty.text.trim_end_matches("[]");
                let suffix = &ty.text[base.len() + 2..];
                let mut result = Fragment::plain(format!("new {base}["));
                result.append(self.expression(length)?);
                result.append(Fragment::plain(format!("]{suffix}")));
                self.events.push(Event::Allocate {
                    site: *site,
                    ty: ty.text.clone(),
                });
                self.events.push(Event::Compute { site: *site });
                Ok(result)
            }
            Expr::ArrayRead { site, array, index } => {
                let mut result = Fragment::plain("(");
                result.append(self.expression(array)?);
                result.append(Fragment::plain(")["));
                result.append(self.expression(index)?);
                result.append(Fragment::plain("]"));
                self.events.push(Event::Read {
                    site: *site,
                    field: "<array>".into(),
                });
                Ok(result)
            }
            Expr::Unary {
                site,
                prefix,
                suffix,
                value,
            } => {
                let mut result = Fragment::plain(format!("{prefix}("));
                result.append(self.expression(value)?);
                result.append(Fragment::plain(format!("){suffix}")));
                self.events.push(Event::Compute { site: *site });
                Ok(result)
            }
            Expr::ArrayStore {
                site,
                array,
                index,
                value,
            } => {
                let mut result = Fragment::plain("(");
                result.append(self.expression(array)?);
                result.append(Fragment::plain(")["));
                result.append(self.expression(index)?);
                result.append(Fragment::plain("] = "));
                result.append(self.expression(value)?);
                self.events.push(Event::Write {
                    site: *site,
                    field: "<array>".into(),
                });
                Ok(result)
            }
            Expr::FieldStore {
                site,
                receiver,
                field,
                value,
            } => {
                let mut result = self.expression(receiver)?;
                result.append(Fragment::plain("."));
                result.append(Fragment::symbol(field));
                result.append(Fragment::plain(" = "));
                result.append(self.expression(value)?);
                self.events.push(Event::Write {
                    site: *site,
                    field: field.label.clone(),
                });
                Ok(result)
            }
            Expr::Capture(index) => self.capture(*index),
            Expr::New(allocation) => allocation.render(self.events),
            Expr::SharedNew {
                allocation,
                constructor_label,
            } => {
                ensure!(
                    allocation.captures.is_empty(),
                    "shared allocation has private captures"
                );
                let mut rendered = allocation.render_shared(self)?;
                rendered
                    .links
                    .first_mut()
                    .context("missing child allocation link")?
                    .label = constructor_label.clone();
                Ok(rendered)
            }
        }
    }
    fn capture(&mut self, index: usize) -> Result<Fragment> {
        let capture = self
            .captures
            .get(index)
            .context("invalid allocation capture")?;
        match self.states[index] {
            CaptureState::Done => return Ok(Fragment::plain(&capture.name)),
            CaptureState::Rendering => bail!("recursive allocation capture"),
            CaptureState::Waiting => {}
        }
        // Later captures may be requested first only if their structured
        // expression emits every earlier capture as an actual dependency.
        self.states[index] = CaptureState::Rendering;
        let expression = self.expression(&capture.expression)?;
        ensure!(
            index == self.next,
            "allocation constructor arguments reorder captured effects"
        );
        self.next += 1;
        self.states[index] = CaptureState::Done;
        let mut result = Fragment::plain(format!("({} = ", capture.name));
        result.append(expression);
        result.append(Fragment::plain(")"));
        Ok(result)
    }
}
