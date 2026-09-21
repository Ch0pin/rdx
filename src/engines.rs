//! Catalog of native Rust decompilation engines. Additional ports belong here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EngineId {
    NativeDex,
}
impl EngineId {
    pub fn key(self) -> &'static str {
        "native"
    }
}
#[derive(Clone, Copy, Debug)]
pub struct EngineDescriptor {
    pub id: EngineId,
    pub name: &'static str,
    pub version: &'static str,
    pub alpha: bool,
    pub partial_java_source: bool,
    pub navigation: bool,
    pub usages: bool,
    pub decoded_resources: bool,
}
pub const ENGINES: &[EngineDescriptor] = &[EngineDescriptor {
    id: EngineId::NativeDex,
    name: "RDX Native DEX",
    version: "0.1",
    alpha: true,
    partial_java_source: true,
    navigation: true,
    usages: true,
    decoded_resources: true,
}];
