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
    Call { site: usize, method: String },
    Construct { site: usize, ty: String },
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
        self.validate(locals, &mut HashSet::new(), 0, &mut 0)?;
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
        ensure!(
            dex_events
                .iter()
                .filter(|event| matches!(event, Event::Allocate { .. } | Event::Construct { .. }))
                .count()
                == 2,
            "nested staged allocation is unsupported"
        );
        // Only backward capture dependencies are allowed. All effects must be
        // reachable from constructor arguments; overwritten captures still fail.
        let mut used = HashSet::new();
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
            let declaration = format!(
                "{}{};",
                prefix,
                &assignment.text[1..assignment.text.len() - 1]
            );
            let shift = prefix.chars().count() - 1;
            declaration_links.push(
                assignment
                    .links
                    .into_iter()
                    .map(|link| Link {
                        start: link.start + shift,
                        end: link.end + shift,
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
        let mut expected = dex_events[1..dex_events.len() - 1].to_vec();
        expected.push(allocate);
        expected.push(construct);
        ensure!(
            events == expected,
            "staged allocation changes nonallocation effect order"
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
            Expr::Boolean(_) | Expr::Char(_) | Expr::Null => {}
            Expr::Cast { ty, value } | Expr::CheckCast { ty, value, .. } => {
                ensure!(
                    !ty.text.is_empty() && !ty.label.is_empty(),
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
            Self::FieldRead { receiver, .. } => receiver.flat_dependencies(before, used)?,
            Self::Call { target, args, .. } => {
                target.flat_dependencies(before, used)?;
                for arg in args {
                    arg.flat_dependencies(before, used)?;
                }
            }
            Self::New(_) | Self::SharedNew { .. } => {
                anyhow::bail!("nested staged allocation is unsupported")
            }
            _ => {}
        }
        Ok(())
    }
    fn declarations(&self, out: &mut Vec<String>) -> Result<()> {
        match self {
            Self::Local(_)
            | Self::Boolean(_)
            | Self::Char(_)
            | Self::Null
            | Self::StringConstant { .. }
            | Self::ClassConstant { .. }
            | Self::Capture(_) => Ok(()),
            Self::Cast { value, .. } | Self::CheckCast { value, .. } => value.declarations(out),
            Self::FieldRead { receiver, .. } => receiver.declarations(out),
            Self::Call { target, args, .. } => {
                target.declarations(out)?;
                for arg in args {
                    arg.declarations(out)?;
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
