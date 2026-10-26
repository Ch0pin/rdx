//! Static OkHttp-compatible role graphs. No runtime reads or hook installation.
use crate::{
    engine::{NativeEngine, Project},
    native_dex::{DexClass, DexField, DexMethod},
    native_engine::disassembly::{method_id, type_name},
    native_ir::DecodedMethod,
};
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::atomic::{AtomicBool, Ordering},
};

const STRING: &str = "Ljava/lang/String;";
const STRINGS: &str = "[Ljava/lang/String;";
const MAX_MODELS: usize = 512;
const MAX_WORK: usize = 8_000_000;
fn instance(m: &DexMethod) -> bool {
    m.access_flags & 8 == 0 && !m.name.starts_with('<')
}
fn field_id(f: &DexField) -> String {
    format!(
        "{}.{}:{}",
        type_name(&f.declaring_type),
        f.name,
        f.field_type
    )
}
fn fields<'a>(c: &'a DexClass, ty: &str) -> Vec<&'a DexField> {
    c.fields
        .iter()
        .filter(|f| !f.is_static && f.field_type.as_ref() == ty)
        .collect()
}
fn signature(m: &DexMethod, params: &[&str], ret: &str) -> bool {
    instance(m)
        && m.return_type.as_ref() == ret
        && m.parameters
            .iter()
            .map(|p| p.as_ref())
            .eq(params.iter().copied())
}
fn headers(c: &DexClass) -> bool {
    fields(c, STRINGS).len() == 1
        && c.methods.iter().any(|m| signature(m, &[STRING], STRING))
        && c.methods
            .iter()
            .filter(|m| signature(m, &["I"], STRING))
            .count()
            >= 2
}
fn url(c: &DexClass) -> bool {
    fields(c, STRING).len() >= 3
        && !fields(c, "I").is_empty()
        && c.methods
            .iter()
            .any(|m| signature(m, &[], "Ljava/net/URI;") || signature(m, &[], "Ljava/net/URL;"))
}
#[derive(Clone, Copy, PartialEq)]
enum Origin {
    Unknown,
    This,
    Index(i32, i32),
    Array,
    Entry(i32),
}
// Prove name/value accessor parity from original DEX, independent of method names.
fn entry_accessor(c: &DexClass, m: &DexMethod) -> Option<i32> {
    if !signature(m, &["I"], STRING) {
        return None;
    }
    let code = m.code.as_ref()?;
    if code.instructions.len() > 128 || code.ins != 2 || !code.try_regions.is_empty() {
        return None;
    }
    let ir = DecodedMethod::decode(code).ok()?;
    let start = code.registers.checked_sub(2)? as usize;
    let mut state = vec![Origin::Unknown; code.registers as usize];
    state[start] = Origin::This;
    state[start + 1] = Origin::Index(1, 0);
    for i in &ir.instructions {
        let read = |n: usize| {
            i.reads
                .get(n)
                .and_then(|r| state.get(r.register as usize))
                .copied()
                .unwrap_or(Origin::Unknown)
        };
        if i.opcode == 0x11 {
            return match read(0) {
                Origin::Entry(n) => Some(n),
                _ => None,
            };
        }
        if matches!(i.opcode,0x0e..=0x10|0x27..=0x3d|0x4b..=0x51|0x59..=0x78|0xfa..=0xff) {
            return None;
        }
        let value = match i.opcode {
            0x01..=0x09 => read(0),
            0x54 if read(0) == Origin::This => {
                let &(owner, ty, name) = c.symbols.fields.get(i.reference?.index as usize)?;
                let owner = c.symbols.types.get(owner as usize)?;
                let ty = c.symbols.types.get(ty as usize)?;
                let name = c.symbols.strings.get(name as usize)?;
                if owner == &c.descriptor
                    && ty.as_ref() == STRINGS
                    && c.fields.iter().any(|f| {
                        !f.is_static && f.name.as_ref() == name.as_str() && f.field_type == *ty
                    })
                {
                    Origin::Array
                } else {
                    Origin::Unknown
                }
            }
            0xd2 | 0xda | 0xd0 | 0xd8 | 0xe0 => {
                let Origin::Index(a, b) = read(0) else {
                    return None;
                };
                let n = i32::try_from(i.literal?).ok()?;
                if matches!(i.opcode, 0xd0 | 0xd8) {
                    Origin::Index(a, b.checked_add(n)?)
                } else {
                    let factor = if i.opcode == 0xe0 {
                        1i32.checked_shl((n & 31) as u32)?
                    } else {
                        n
                    };
                    Origin::Index(a.checked_mul(factor)?, b.checked_mul(factor)?)
                }
            }
            0x46 if read(0) == Origin::Array => match read(1) {
                Origin::Index(2, n) if n == 0 || n == 1 => Origin::Entry(n),
                _ => Origin::Unknown,
            },
            _ => Origin::Unknown,
        };
        for w in &i.writes {
            for word in 0..w.kind.word_count() {
                *state.get_mut(w.register as usize + word)? = value;
            }
        }
    }
    None
}
struct Model<'a> {
    request: &'a DexClass,
    response: &'a DexClass,
    url: &'a DexClass,
    headers: &'a DexClass,
    url_field: &'a DexField,
    headers_field: &'a DexField,
    method_field: &'a DexField,
}
struct Work<'a> {
    remaining: usize,
    cancel: &'a AtomicBool,
}
impl Work<'_> {
    fn take(&mut self, n: usize) -> Result<bool> {
        ensure!(!self.cancel.load(Ordering::Relaxed), "CANCELLED");
        if n > self.remaining {
            self.remaining = 0;
            return Ok(false);
        }
        self.remaining -= n;
        Ok(true)
    }
}
pub(crate) fn search(
    engine: &NativeEngine,
    project: &Project,
    args: &Value,
    cancel: &AtomicBool,
) -> Result<Value> {
    let mut classes = Vec::with_capacity(project.classes.len());
    for name in &project.classes {
        ensure!(!cancel.load(Ordering::Relaxed), "CANCELLED");
        if let Some(c) = engine.dex_class(name) {
            classes.push(c);
        }
    }
    detect(&classes, args, cancel, MAX_WORK)
}
fn detect(
    classes: &[&DexClass],
    args: &Value,
    cancel: &AtomicBool,
    budget: usize,
) -> Result<Value> {
    let offset = args["offset"].as_u64().unwrap_or(0) as usize;
    let limit = args["limit"].as_u64().unwrap_or(25) as usize;
    ensure!((1..=100).contains(&limit), "limit must be 1..100");
    let prefix = args["package_prefix"]
        .as_str()
        .unwrap_or("")
        .trim_end_matches('.');
    let mut work = Work {
        remaining: budget,
        cancel,
    };
    let by_type: BTreeMap<_, _> = classes
        .iter()
        .map(|c| (c.descriptor.as_ref(), *c))
        .collect();
    let mut header_types = BTreeSet::new();
    let mut url_types = BTreeSet::new();
    let mut complete = true;
    let mut examined = 0;
    for c in classes {
        if !work.take(1 + c.methods.len() + c.fields.len())? {
            complete = false;
            break;
        }
        examined += 1;
        if headers(c) {
            header_types.insert(c.descriptor.as_ref());
        }
        if url(c) {
            url_types.insert(c.descriptor.as_ref());
        }
    }
    let mut models = Vec::new();
    'models: for response in classes {
        if !work.take(1 + response.fields.len() + response.methods.len())? {
            complete = false;
            break;
        }
        if !response.interfaces.iter().any(|i| {
            i.as_ref() == "Ljava/io/Closeable;" || i.as_ref() == "Ljava/lang/AutoCloseable;"
        }) || fields(response, "I").is_empty()
            || fields(response, STRING).is_empty()
        {
            continue;
        }
        for rf in response.fields.iter().filter(|f| !f.is_static) {
            if !work.take(1)? {
                complete = false;
                break 'models;
            }
            let Some(request) = by_type.get(rf.field_type.as_ref()).copied() else {
                continue;
            };
            if !work.take(request.fields.len())? {
                complete = false;
                break 'models;
            }
            let strings = fields(request, STRING);
            let urls: Vec<_> = request
                .fields
                .iter()
                .filter(|f| !f.is_static && url_types.contains(f.field_type.as_ref()))
                .collect();
            let hs: Vec<_> = request
                .fields
                .iter()
                .filter(|f| !f.is_static && header_types.contains(f.field_type.as_ref()))
                .collect();
            if strings.len() != 1
                || urls.len() != 1
                || hs.len() != 1
                || fields(response, &hs[0].field_type).len() != 1
            {
                continue;
            }
            if models.len() == MAX_MODELS {
                complete = false;
                break 'models;
            }
            if models.iter().any(|m: &Model<'_>| {
                m.request.descriptor == request.descriptor
                    && m.response.descriptor == response.descriptor
            }) {
                continue;
            }
            models.push(Model {
                request,
                response,
                url: by_type[urls[0].field_type.as_ref()],
                headers: by_type[hs[0].field_type.as_ref()],
                url_field: urls[0],
                headers_field: hs[0],
                method_field: strings[0],
            });
        }
    }
    // Index only methods that consume a discovered request and return its response.
    let mut chains = Vec::new();
    'chains: for c in classes {
        for m in &c.methods {
            if !work.take(1 + models.len())? {
                complete = false;
                break 'chains;
            }
            for (index, model) in models.iter().enumerate() {
                if signature(m, &[&model.request.descriptor], &model.response.descriptor) {
                    let request_fields = fields(c, &model.request.descriptor);
                    let getters: Vec<_> = c
                        .methods
                        .iter()
                        .filter(|g| signature(g, &[], &model.request.descriptor))
                        .collect();
                    if request_fields.len() != 1 && getters.len() != 1 {
                        continue;
                    }
                    if chains.len() == MAX_MODELS {
                        complete = false;
                        break 'chains;
                    }
                    chains.push((
                        index,
                        *c,
                        m,
                        if request_fields.len() == 1 {
                            request_fields.first().copied()
                        } else {
                            None
                        },
                        if getters.len() == 1 {
                            getters.first().copied()
                        } else {
                            None
                        },
                    ));
                }
            }
        }
    }
    let mut items = Vec::new();
    let mut total = 0usize;
    let mut bytes = 0;
    let mut next = None;
    'hooks: for c in classes {
        let name = type_name(&c.descriptor);
        if !prefix.is_empty()
            && name != prefix
            && !name
                .strip_prefix(prefix)
                .is_some_and(|s| s.starts_with('.'))
        {
            continue;
        }
        for m in &c.methods {
            if !work.take(1 + chains.len())? {
                complete = false;
                break 'hooks;
            }
            if m.code.is_none() {
                continue;
            }
            for (index, chain, proceed, request_field, request_getter) in &chains {
                let model = &models[*index];
                if !signature(m, &[&chain.descriptor], &model.response.descriptor) {
                    continue;
                }
                // Require the matching interceptor interface contract as an independent signal.
                let contract = c
                    .interfaces
                    .iter()
                    .filter_map(|i| by_type.get(i.as_ref()))
                    .flat_map(|i| i.methods.iter())
                    .find(|i| {
                        i.name == m.name
                            && signature(i, &[&chain.descriptor], &model.response.descriptor)
                    });
                let Some(contract) = contract else {
                    continue;
                };
                let position = total;
                total += 1;
                if position < offset {
                    continue;
                }
                if items.len() >= limit || bytes >= 850_000 {
                    next.get_or_insert(position);
                    continue;
                }
                let names: Vec<_> = model
                    .headers
                    .methods
                    .iter()
                    .filter(|g| entry_accessor(model.headers, g) == Some(0))
                    .map(method_id)
                    .collect();
                let values_proven = model
                    .headers
                    .methods
                    .iter()
                    .any(|g| entry_accessor(model.headers, g) == Some(1));
                let names_proven = names.len() == 1 && values_proven;
                let item = json!({"family":"okhttp-compatible","identification":"structural_candidate","version":Value::Null,
                    "standard_namespace":model.request.descriptor.starts_with("Lokhttp3/"),
                    "roles":{"request":type_name(&model.request.descriptor),"response":type_name(&model.response.descriptor),"url":type_name(&model.url.descriptor),"headers":type_name(&model.headers.descriptor),"chain":type_name(&chain.descriptor)},
                    "hook_map":{"interceptor_method_id":method_id(m),"interceptor_contract_method_id":method_id(contract),"chain_proceed_method_id":method_id(proceed),"chain_argument_index":0,
                        "request_from_chain_field_id":request_field.map(field_id),"request_from_chain_getter_id":request_getter.map(method_id),
                        "request_url_field_id":field_id(model.url_field),"request_method_field_id":field_id(model.method_field),"request_headers_field_id":field_id(model.headers_field),
                        "header_pairs_field_id":field_id(fields(model.headers,STRINGS)[0]),"header_pair_layout":"inferred alternating name/value slots; validate before capture",
                        "header_name_method_id":if names_proven {names.first().cloned()}else{None},"header_name_indexing_proven":names_proven,"header_name_role":"inferred from OkHttp-compatible paired-array layout; validate before runtime capture"},
                    "evidence":["String-array headers with string lookup and two indexed string accessors","URI/URL conversion type with URL-shaped fields","Request joins unique URL, headers and String fields","Closeable response joins request, shared headers, status integer and message String","Chain accepts request and returns response","Concrete interceptor matches a declared interface contract"],
                    "unresolved_roles":if names_proven {vec![]} else {vec!["header_name_accessor"]},
                    "runtime_verified":false});
                let size = serde_json::to_vec(&item)?.len();
                ensure!(size <= 150_000, "Network candidate exceeds response budget");
                bytes += size;
                items.push(item);
            }
        }
    }
    Ok(
        json!({"items":items,"total":if complete {Some(total)}else{None},"matching_candidates_so_far":total,"offset":offset,"next_offset":next,
        "coverage":{"complete":complete,"profiled_classes":examined,"loaded_classes":classes.len(),"work_budget_exhausted":work.remaining==0,"candidate_graph_limit":MAX_MODELS,"supported_families":["okhttp-compatible"],"scope":"Loaded declared fields, method descriptors and direct interface contracts. Structural candidates require review; unsupported, inherited-only or aggressively optimized layouts can be missed. No exact library version or runtime activity is inferred."},
        "runtime_values_included":false,"observer_redaction_requirements":{"url":"Emit scheme, host and port only; omit userinfo, path, query and fragment","http_method":"Emit a validated HTTP method token only","headers":"Emit header names only; never values","credential_presence":"Boolean membership of lowercase header names: authorization, proxy-authorization, cookie, x-api-key","request_and_response_bodies":"Omit"},
        "hook_map_kind":"Static member selectors and observer policy; no hooks are installed or runtime data collected"}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_dex::{DexCode, DexSymbols};
    use std::sync::Arc;
    fn method(
        owner: &str,
        name: &str,
        params: &[&str],
        ret: &str,
        code: Option<Vec<u16>>,
    ) -> DexMethod {
        DexMethod {
            declaring_type: owner.into(),
            name: name.into(),
            return_type: ret.into(),
            parameters: params.iter().map(|s| Arc::from(*s)).collect(),
            thrown_types: vec![],
            access_flags: 1,
            code: code.map(|instructions| DexCode {
                registers: 4,
                ins: 2,
                outs: 0,
                tries: 0,
                try_regions: vec![],
                instructions,
                offset: 0,
            }),
        }
    }
    fn class(
        owner: &str,
        fs: &[(&str, &str)],
        methods: Vec<DexMethod>,
        interfaces: &[&str],
    ) -> DexClass {
        DexClass {
            symbols: Arc::new(DexSymbols::default()),
            descriptor: owner.into(),
            superclass: Some("Ljava/lang/Object;".into()),
            interfaces: interfaces.iter().map(|s| Arc::from(*s)).collect(),
            access_flags: 1,
            annotations_offset: 0,
            static_values_offset: 0,
            static_values: vec![],
            fields: fs
                .iter()
                .map(|(n, t)| DexField {
                    declaring_type: owner.into(),
                    name: (*n).into(),
                    field_type: (*t).into(),
                    access_flags: 0x11,
                    is_static: false,
                })
                .collect(),
            methods,
        }
    }
    fn fixture() -> Vec<DexClass> {
        let (h, u, q, r, c, i) = (
            "Lother/HeadersX;",
            "Luri/Z;",
            "Lreq/Q;",
            "Lres/R;",
            "Lchain/C;",
            "Lcontract/I;",
        );
        let name = vec![0x00da, 0x0203, 0x2154, 0, 0x0046, 0x0001, 0x0011];
        let value = vec![
            0x00da, 0x0203, 0x00d8, 0x0100, 0x2154, 0, 0x0046, 0x0001, 0x0011,
        ];
        let mut hc = class(
            h,
            &[("pairs", STRINGS)],
            vec![
                method(h, "get", &[STRING], STRING, None),
                method(h, "x", &["I"], STRING, Some(name)),
                method(h, "y", &["I"], STRING, Some(value)),
            ],
            &[],
        );
        hc.symbols = Arc::new(DexSymbols {
            types: vec![h.into(), STRINGS.into()],
            strings: vec!["pairs".into(), "secret-cookie-value-do-not-export".into()],
            fields: vec![(0, 1, 0)],
            ..Default::default()
        });
        let mut result = vec![
            hc,
            class(
                u,
                &[("a", STRING), ("b", STRING), ("c", STRING), ("port", "I")],
                vec![method(u, "uri", &[], "Ljava/net/URI;", None)],
                &[],
            ),
            class(
                q,
                &[("url", u), ("method", STRING), ("headers", h)],
                vec![],
                &[],
            ),
            class(
                r,
                &[
                    ("request", q),
                    ("status", "I"),
                    ("message", STRING),
                    ("headers", h),
                ],
                vec![],
                &["Ljava/io/Closeable;"],
            ),
            class(
                c,
                &[("request", q)],
                vec![method(c, "proceed", &[q], r, Some(vec![0x0011]))],
                &[],
            ),
            class(i, &[], vec![method(i, "intercept", &[c], r, None)], &[]),
        ];
        for owner in ["Lapp/First;", "Lapp/Second;"] {
            result.push(class(
                owner,
                &[],
                vec![method(owner, "intercept", &[c], r, Some(vec![0x0011]))],
                &[i],
            ));
        }
        result
    }
    fn run(classes: &[DexClass], args: Value) -> Value {
        detect(
            &classes.iter().collect::<Vec<_>>(),
            &args,
            &false.into(),
            MAX_WORK,
        )
        .unwrap()
    }
    #[test]
    fn renamed_cross_package_roles_and_header_parity_are_found_without_literal_values() {
        let classes = fixture();
        let result = run(&classes, json!({}));
        assert_eq!(result["total"], 2, "{result}");
        assert_eq!(result["coverage"]["complete"], true);
        let map = &result["items"][0]["hook_map"];
        assert_eq!(
            map["interceptor_method_id"],
            "app.First.intercept(Lchain/C;)Lres/R;"
        );
        assert_eq!(
            map["header_name_method_id"],
            "other.HeadersX.x(I)Ljava/lang/String;"
        );
        assert_eq!(map["header_name_indexing_proven"], true);
        assert_eq!(
            map["request_from_chain_field_id"],
            "chain.C.request:Lreq/Q;"
        );
        assert_eq!(result["runtime_values_included"], false);
        assert!(!result.to_string().contains("secret-cookie-value"));
        assert_eq!(
            run(&classes, json!({"package_prefix":"app.First"}))["total"],
            1
        );
        assert_eq!(run(&classes, json!({"package_prefix":"ap"}))["total"], 0);
        let first = run(&classes, json!({"limit":1}));
        let second = run(&classes, json!({"limit":1,"offset":first["next_offset"]}));
        assert_eq!(first["items"][0], result["items"][0]);
        assert_eq!(second["items"][0], result["items"][1]);
        assert!(second["next_offset"].is_null());
    }
    #[test]
    fn ambiguous_roles_missing_contract_and_wrong_header_index_stay_explicit() {
        let mut classes = fixture();
        // A value getter must never be advertised as a header-name getter.
        classes[0].methods[1].code.as_mut().unwrap().instructions = vec![
            0x00da, 0x0203, 0x00d8, 0x0100, 0x2154, 0, 0x0046, 0x0001, 0x0011,
        ];
        let result = run(&classes, json!({}));
        assert_eq!(result["total"], 2);
        assert_eq!(
            result["items"][0]["hook_map"]["header_name_indexing_proven"],
            false
        );
        assert!(result["items"][0]["hook_map"]["header_name_method_id"].is_null());
        classes[6].interfaces.clear();
        classes[7].interfaces.clear();
        assert_eq!(run(&classes, json!({}))["total"], 0);
        let mut classes = fixture();
        classes[2].fields.push(DexField {
            declaring_type: "Lreq/Q;".into(),
            name: "ambiguous".into(),
            field_type: STRING.into(),
            access_flags: 1,
            is_static: false,
        });
        assert_eq!(run(&classes, json!({}))["total"], 0);
    }
    #[test]
    fn cancellation_and_budget_never_claim_complete_empty_results() {
        let classes = fixture();
        let refs = classes.iter().collect::<Vec<_>>();
        assert!(
            detect(&refs, &json!({}), &true.into(), MAX_WORK)
                .unwrap_err()
                .to_string()
                .contains("CANCELLED")
        );
        let partial = detect(&refs, &json!({}), &false.into(), 1).unwrap();
        assert_eq!(partial["coverage"]["complete"], false);
        assert!(partial["total"].is_null());
        assert!(detect(&refs, &json!({"limit":101}), &false.into(), MAX_WORK).is_err());
    }
}
