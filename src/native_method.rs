//! Shared analysis context for one immutable DEX method.
//!
//! Keeps instruction, block, SSA and call identities together. Expensive type
//! inference is opt-in and is not added to the ordinary rendering path.
use crate::{
    native_call_values::SsaCalls,
    native_calls::BoundCalls,
    native_cfg::ControlFlowGraph,
    native_constructors::ConstructorAnalysis,
    native_dex::{DexClass, DexMethod},
    native_ir::DecodedMethod,
    native_ssa::SsaMethod,
    native_types::InferredTypes,
};
use anyhow::{Context, Result};

pub struct MethodAnalysis<'a> {
    class: &'a DexClass,
    method: &'a DexMethod,
    ir: DecodedMethod,
    cfg: ControlFlowGraph,
    ssa: SsaMethod,
    bound: BoundCalls,
    calls: SsaCalls,
}

impl<'a> MethodAnalysis<'a> {
    pub fn build(class: &'a DexClass, method: &'a DexMethod) -> Result<Self> {
        let code = method.code.as_ref().context("method has no code")?;
        let ir = DecodedMethod::decode(code).context("instruction analysis")?;
        let cfg = ControlFlowGraph::build(code).context("block analysis")?;
        let ssa = SsaMethod::build(code, &ir, &cfg).context("SSA analysis")?;
        let bound = BoundCalls::bind(code, &ir, &class.symbols).context("call binding")?;
        let calls = SsaCalls::bind(&bound, &ssa).context("SSA call binding")?;
        Ok(Self {
            class,
            method,
            ir,
            cfg,
            ssa,
            bound,
            calls,
        })
    }

    pub fn instructions(&self) -> &DecodedMethod {
        &self.ir
    }
    pub fn blocks(&self) -> &ControlFlowGraph {
        &self.cfg
    }
    pub fn ssa(&self) -> &SsaMethod {
        &self.ssa
    }
    pub fn calls(&self) -> &SsaCalls {
        &self.calls
    }

    pub fn constructors(&self) -> Result<ConstructorAnalysis> {
        ConstructorAnalysis::analyze(
            self.class,
            self.method,
            &self.ir,
            &self.bound,
            &self.ssa,
            &self.calls,
        )
    }

    pub fn infer_types(&self) -> Result<InferredTypes> {
        InferredTypes::infer(
            self.method,
            &self.ir,
            &self.ssa,
            &self.calls,
            &self.class.symbols,
        )
    }
}
