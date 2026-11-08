//! Exact named-API evidence complementary to relocated structural profiles.
use super::*;
use crate::{
    native_cfg::instruction_width,
    native_dex::{DexAnnotationSet, DexSymbols},
    native_xrefs,
};
use anyhow::Context;
use std::{collections::HashMap, sync::Arc};

#[path = "retrofit_semantics.rs"]
mod retrofit;

fn call_role(owner: &str, name: &str, params: &[Arc<str>], ret: &str) -> Option<&'static str> {
    let p: Vec<_> = params.iter().map(AsRef::as_ref).collect();
    match owner {
        "Lokhttp3/OkHttpClient$Builder;" => match (name, p.as_slice(), ret) {
            ("<init>", [], "V") => Some("client_builder_constructor"),
            ("build", [], "Lokhttp3/OkHttpClient;") => Some("client_build"),
            ("addInterceptor", ["Lokhttp3/Interceptor;"], "Lokhttp3/OkHttpClient$Builder;") => {
                Some("application_interceptor_registration")
            }
            (
                "addNetworkInterceptor",
                ["Lokhttp3/Interceptor;"],
                "Lokhttp3/OkHttpClient$Builder;",
            ) => Some("network_interceptor_registration"),
            ("authenticator", ["Lokhttp3/Authenticator;"], "Lokhttp3/OkHttpClient$Builder;") => {
                Some("authenticator_configuration")
            }
            (
                "proxyAuthenticator",
                ["Lokhttp3/Authenticator;"],
                "Lokhttp3/OkHttpClient$Builder;",
            ) => Some("proxy_authenticator_configuration"),
            (
                "certificatePinner",
                ["Lokhttp3/CertificatePinner;"],
                "Lokhttp3/OkHttpClient$Builder;",
            ) => Some("certificate_pinner_configuration"),
            (
                "hostnameVerifier",
                ["Ljavax/net/ssl/HostnameVerifier;"],
                "Lokhttp3/OkHttpClient$Builder;",
            ) => Some("hostname_verifier_configuration"),
            (
                "sslSocketFactory",
                ["Ljavax/net/ssl/SSLSocketFactory;"],
                "Lokhttp3/OkHttpClient$Builder;",
            )
            | (
                "sslSocketFactory",
                [
                    "Ljavax/net/ssl/SSLSocketFactory;",
                    "Ljavax/net/ssl/X509TrustManager;",
                ],
                "Lokhttp3/OkHttpClient$Builder;",
            ) => Some("tls_socket_factory_configuration"),
            _ => None,
        },
        "Lokhttp3/CertificatePinner$Builder;" => match (name, p.as_slice(), ret) {
            ("<init>", [], "V") => Some("certificate_pinner_builder"),
            (
                "add",
                ["Ljava/lang/String;", "[Ljava/lang/String;"],
                "Lokhttp3/CertificatePinner$Builder;",
            ) => Some("certificate_pin_declaration"),
            ("build", [], "Lokhttp3/CertificatePinner;") => Some("certificate_pinner_build"),
            _ => None,
        },
        "Lokhttp3/OkHttpClient;"
            if name == "newBuilder" && p.is_empty() && ret == "Lokhttp3/OkHttpClient$Builder;" =>
        {
            Some("client_builder_copy")
        }
        "Lretrofit2/Retrofit$Builder;" => match (name, p.as_slice(), ret) {
            ("<init>", [], "V") => Some("retrofit_builder"),
            ("client", ["Lokhttp3/OkHttpClient;"], "Lretrofit2/Retrofit$Builder;") => {
                Some("retrofit_client_configuration")
            }
            ("callFactory", ["Lokhttp3/Call$Factory;"], "Lretrofit2/Retrofit$Builder;") => {
                Some("retrofit_call_factory_configuration")
            }
            ("build", [], "Lretrofit2/Retrofit;") => Some("retrofit_build"),
            (
                "baseUrl",
                ["Ljava/lang/String;" | "Ljava/net/URL;" | "Lokhttp3/HttpUrl;"],
                "Lretrofit2/Retrofit$Builder;",
            ) => Some("retrofit_base_url_configuration"),
            _ => None,
        },
        "Lretrofit2/Retrofit;"
            if name == "create" && p == ["Ljava/lang/Class;"] && ret == "Ljava/lang/Object;" =>
        {
            Some("retrofit_service_creation")
        }
        _ => None,
    }
}
#[derive(Default)]
struct TableRoles {
    methods: BTreeMap<usize, &'static str>,
    fields: BTreeMap<usize, &'static str>,
}
fn field_role(owner: &str, name: &str, ty: &str) -> Option<&'static str> {
    match (owner, name, ty) {
        (
            "Lokhttp3/OkHttpClient$Builder;",
            "hostnameVerifier",
            "Ljavax/net/ssl/HostnameVerifier;",
        ) => Some("hostname_verifier_configuration"),
        ("Lokhttp3/OkHttpClient$Builder;", "certificatePinner", "Lokhttp3/CertificatePinner;") => {
            Some("certificate_pinner_configuration")
        }
        (
            "Lokhttp3/OkHttpClient$Builder;",
            "sslSocketFactory" | "sslSocketFactoryOrNull",
            "Ljavax/net/ssl/SSLSocketFactory;",
        ) => Some("tls_socket_factory_configuration"),
        (
            "Lokhttp3/OkHttpClient$Builder;",
            "x509TrustManager" | "x509TrustManagerOrNull",
            "Ljavax/net/ssl/X509TrustManager;",
        ) => Some("trust_manager_configuration"),
        ("Lokhttp3/OkHttpClient$Builder;", "authenticator", "Lokhttp3/Authenticator;") => {
            Some("authenticator_configuration")
        }
        ("Lokhttp3/OkHttpClient$Builder;", "proxyAuthenticator", "Lokhttp3/Authenticator;") => {
            Some("proxy_authenticator_configuration")
        }
        ("Lretrofit2/Retrofit$Builder;", "callFactory", "Lokhttp3/Call$Factory;") => {
            Some("retrofit_call_factory_configuration")
        }
        ("Lretrofit2/Retrofit$Builder;", "baseUrl", "Lokhttp3/HttpUrl;") => {
            Some("retrofit_base_url_configuration")
        }
        _ => None,
    }
}
fn table_roles(symbols: &DexSymbols, work: &mut Work<'_>) -> Result<Option<TableRoles>> {
    let mut found = TableRoles::default();
    for (index, &(owner, proto, name)) in symbols.methods.iter().enumerate() {
        if !work.take(1)? {
            return Ok(None);
        }
        let owner = symbols
            .types
            .get(owner as usize)
            .context("Invalid method owner")?;
        if !matches!(
            owner.as_ref(),
            "Lokhttp3/OkHttpClient$Builder;"
                | "Lokhttp3/CertificatePinner$Builder;"
                | "Lokhttp3/OkHttpClient;"
                | "Lretrofit2/Retrofit$Builder;"
                | "Lretrofit2/Retrofit;"
        ) {
            continue;
        }
        let (ret, params) = symbols
            .protos
            .get(proto as usize)
            .context("Invalid method prototype")?;
        let name = symbols
            .strings
            .get(name as usize)
            .context("Invalid method name")?;
        if let Some(role) = call_role(owner, name, params, ret) {
            found.methods.insert(index, role);
        }
    }
    for (index, &(owner, ty, name)) in symbols.fields.iter().enumerate() {
        if !work.take(1)? {
            return Ok(None);
        }
        let owner = symbols
            .types
            .get(owner as usize)
            .context("Invalid field owner")?;
        if !matches!(
            owner.as_ref(),
            "Lokhttp3/OkHttpClient$Builder;" | "Lretrofit2/Retrofit$Builder;"
        ) {
            continue;
        }
        let ty = symbols
            .types
            .get(ty as usize)
            .context("Invalid field type")?;
        let name = symbols
            .strings
            .get(name as usize)
            .context("Invalid field name")?;
        if let Some(role) = field_role(owner, name, ty) {
            found.fields.insert(index, role);
        }
    }
    Ok(Some(found))
}

struct Page {
    offset: usize,
    limit: usize,
    total: usize,
    items: Vec<Value>,
    bytes: usize,
    full: bool,
}
impl Page {
    fn add(&mut self, item: Value) -> Result<()> {
        let position = self.total;
        self.total += 1;
        if position < self.offset || self.full {
            return Ok(());
        }
        if self.items.len() == self.limit {
            self.full = true;
            return Ok(());
        }
        let bytes = serde_json::to_vec(&item)?.len();
        ensure!(
            bytes <= 150_000,
            "Network usage evidence exceeds response budget"
        );
        if self.bytes + bytes > 850_000 {
            self.full = true;
            return Ok(());
        }
        self.bytes += bytes;
        self.items.push(item);
        Ok(())
    }
}
fn annotation_role(ty: &str, parameter: bool) -> Option<&'static str> {
    let name = ty.strip_prefix("Lretrofit2/http/")?.strip_suffix(';')?;
    if parameter {
        match name {
            "Url" => Some("url_parameter"),
            "HeaderMap" => Some("header_map_parameter"),
            "Header" | "Query" | "QueryMap" | "QueryName" | "Path" | "Field" | "FieldMap"
            | "Part" | "PartMap" | "Body" | "Tag" => Some("request_parameter"),
            _ => None,
        }
    } else if [
        "GET",
        "POST",
        "PUT",
        "PATCH",
        "DELETE",
        "HEAD",
        "OPTIONS",
        "HTTP",
        "Headers",
        "Multipart",
        "FormUrlEncoded",
        "Streaming",
        "SkipCallbackExecutor",
    ]
    .contains(&name)
    {
        Some("service_method_annotation")
    } else {
        None
    }
}
fn recovered_annotation_role(semantic: &str, parameter: bool) -> Option<&'static str> {
    annotation_role(&format!("Lretrofit2/http/{semantic};"), parameter)
}
fn dynamic_url_value(found: bool, unknown: bool, recovery_complete: bool) -> Value {
    if found {
        json!(true)
    } else if unknown || !recovery_complete {
        Value::Null
    } else {
        json!(false)
    }
}
struct Scan<'a> {
    work: Work<'a>,
    page: Page,
    errors: Vec<Value>,
    error_count: usize,
    classes: usize,
}
impl Scan<'_> {
    fn error(&mut self, owner: &str, error: impl std::fmt::Display) {
        self.error_count += 1;
        if self.errors.len() < 20 {
            self.errors
                .push(json!({"class_id":owner,"error":error.to_string()}));
        }
    }
    fn annotations(
        &mut self,
        c: &DexClass,
        m: &DexMethod,
        set: &DexAnnotationSet,
        parameter: Option<usize>,
        mappings: &retrofit::Mappings,
    ) -> Result<bool> {
        for a in set.iter() {
            if !self.work.take(1)? {
                return Ok(false);
            }
            if a.visibility != 1 {
                continue;
            }
            let Some(ty) = c.symbols.types.get(a.type_idx as usize) else {
                self.error(&type_name(&c.descriptor), "Invalid annotation type");
                continue;
            };
            let mapped = mappings.get(ty);
            let mapped_role =
                mapped.and_then(|m| recovered_annotation_role(m.name, parameter.is_some()));
            if let Some(role) = annotation_role(ty, parameter.is_some()).or(mapped_role) {
                self.page.add(json!({"kind":"api_usage","identification":"exact_descriptor_evidence","detector":"retrofit_annotations","family":"retrofit2",
                    "class_id":type_name(&c.descriptor),"method_id":method_id(m),"class_is_interface":c.access_flags&0x200!=0,
                    "parameter_index":parameter,"parameter_index_base":"zero-based; receiver excluded","annotation_type":type_name(ty),"role":role,
                    "semantic_annotation":mapped.map(|m|m.name).or_else(||ty.strip_prefix("Lretrofit2/http/").and_then(|s|s.strip_suffix(';'))),
                    "mapping_evidence":mapped.map(|m|&m.evidence),
                    "dynamic_input":match role {"url_parameter"=>Some("URL supplied as a method argument"),"header_map_parameter"=>Some("Header map supplied as a method argument"),_=>None},
                    "external_control":"not_analyzed","dex_entry":c.symbols.origin.get(),"runtime_verified":false}))?;
            }
        }
        Ok(true)
    }
    fn service(
        &mut self,
        c: &DexClass,
        m: &DexMethod,
        method_index: usize,
        mappings: &retrofit::Mappings,
        links: &retrofit::CreationScan,
    ) -> Result<bool> {
        if c.access_flags & 0x200 == 0 {
            return Ok(true);
        }
        let Some(directory) = c.symbols.annotations.get(&c.annotations_offset) else {
            return Ok(true);
        };
        let Some(Some(method_set)) = directory.methods.get(method_index) else {
            return Ok(true);
        };
        let known_http = method_set.iter().any(|a| {
            if a.visibility != 1 {
                return false;
            }
            c.symbols.types.get(a.type_idx as usize).is_some_and(|ty| {
                let role = mappings.get(ty).map(|r| r.name).or_else(|| {
                    ty.strip_prefix("Lretrofit2/http/")
                        .and_then(|s| s.strip_suffix(';'))
                });
                role.is_some_and(|r| {
                    [
                        "GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS", "HTTP",
                    ]
                    .contains(&r)
                })
            })
        });
        if !known_http {
            return Ok(true);
        }
        let annotation_count = method_set.len()
            + directory.parameters.get(method_index).map_or(0, |params| {
                params
                    .iter()
                    .filter_map(Option::as_ref)
                    .map(|set| set.len())
                    .sum::<usize>()
            });
        if !self.work.take(annotation_count)? {
            return Ok(false);
        }
        let mut value_budget = 120_000usize;
        let mut method_annotations = Vec::new();
        let mut parameter_annotations = Vec::new();
        let mut verbs = Vec::new();
        let mut dynamic_url = false;
        let mut header_parameters = Vec::new();
        let mut query_parameters = Vec::new();
        let mut unknown_parameters = Vec::new();
        let mut unresolved_parameter_values = false;
        let mut unknown_runtime_method_annotations = 0usize;
        let mut collect = |a: &crate::native_dex::DexAnnotation,
                           parameter: Option<usize>|
         -> Result<()> {
            let ty = c
                .symbols
                .types
                .get(a.type_idx as usize)
                .context("Invalid annotation type")?;
            let semantic = mappings
                .get(ty)
                .filter(|r| recovered_annotation_role(r.name, parameter.is_some()).is_some())
                .map(|r| r.name)
                .or_else(|| {
                    ty.strip_prefix("Lretrofit2/http/")
                        .and_then(|s| s.strip_suffix(';'))
                        .filter(|role| {
                            recovered_annotation_role(role, parameter.is_some()).is_some()
                        })
                });
            let decoded = crate::annotation_values::elements_value(
                &a.elements,
                &c.symbols,
                &mut value_budget,
            );
            let (values, value_status) = match decoded {
                Ok(v) => (v, "explicit_decoded"),
                Err(_) => (Value::Null, "unresolved_value_budget_or_invalid_symbol"),
            };
            let item = json!({"original_descriptor":ty.as_ref(),"semantic_role":semantic,"runtime_visible":a.visibility==1,
                "values":values,"value_status":value_status,"mapping_evidence":mappings.get(ty).map(|r|&r.evidence),
                "visibility":a.visibility,"parameter_index":parameter});
            if let Some(index) = parameter {
                if a.visibility == 1 {
                    if value_status != "explicit_decoded" {
                        unresolved_parameter_values = true;
                    }
                    match semantic {
                        Some("Url") => dynamic_url = true,
                        Some("Header") | Some("HeaderMap") => {
                            if semantic == Some("Header") && !values["value"]["value"].is_string() {
                                unresolved_parameter_values = true;
                            }
                            header_parameters.push(json!({"index":index,"name":values["value"]["value"],"role":semantic}));
                        }
                        Some("Query") | Some("QueryMap") | Some("QueryName") => {
                            if semantic == Some("Query") && !values["value"]["value"].is_string() {
                                unresolved_parameter_values = true;
                            }
                            query_parameters.push(json!({"index":index,"name":values["value"]["value"],"role":semantic}));
                        }
                        None => unknown_parameters.push(index),
                        _ => {}
                    }
                }
                parameter_annotations.push(item);
            } else {
                if a.visibility == 1 && semantic.is_none() {
                    unknown_runtime_method_annotations += 1;
                }
                if let Some(role) = semantic.filter(|r| {
                    a.visibility == 1 && {
                        [
                            "GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS", "HTTP",
                        ]
                        .contains(r)
                    }
                }) {
                    let verb = if role == "HTTP" {
                        values["method"]["value"].as_str().map(str::to_owned)
                    } else {
                        Some(role.to_owned())
                    };
                    let path_key = if role == "HTTP" { "path" } else { "value" };
                    verbs.push((
                        verb,
                        values[path_key]["value"].as_str().map(str::to_owned),
                        ty.to_string(),
                    ));
                }
                method_annotations.push(item);
            }
            Ok(())
        };
        if let Some(Some(set)) = directory.methods.get(method_index) {
            for a in set.iter() {
                collect(a, None)?;
            }
        }
        if let Some(params) = directory.parameters.get(method_index) {
            for (pi, set) in params.iter().enumerate() {
                if let Some(set) = set {
                    for a in set.iter() {
                        collect(a, Some(pi))?;
                    }
                }
            }
        }
        let unique = verbs.len() == 1;
        let verb = if unique { verbs[0].0.clone() } else { None };
        let path = if unique { verbs[0].1.clone() } else { None };
        let sites = links
            .items
            .get(c.descriptor.as_ref())
            .cloned()
            .unwrap_or_default();
        let link_status = if sites
            .iter()
            .any(|site| site["base_url_candidate"].is_string())
        {
            "callsite_same_object_library_model_conditional"
        } else if sites.is_empty() {
            if !links.model_resolved {
                "creation_model_unresolved"
            } else if links.budget_exhausted || links.sites_truncated > 0 {
                "creation_search_incomplete"
            } else {
                "no_supported_creation_site_observed"
            }
        } else {
            "creation_site_base_url_unresolved"
        };
        self.page.add(json!({"kind":"retrofit_service_method","detector":"retrofit_service_inventory",
            "family":"retrofit2","class_id":type_name(&c.descriptor),"method_id":method_id(m),
            "http_verb":verb,"declared_path":path,"dynamic_url_parameter":dynamic_url_value(dynamic_url,!unknown_parameters.is_empty(),mappings.recovery_complete),
            "path_status":if !unique {"ambiguous_multiple_http_annotations"} else if path.is_some() {"explicit_annotation_value"} else {"unresolved_annotation_value_or_default"},
            "header_parameters":header_parameters,"query_parameters":query_parameters,
            "header_parameter_status":if unknown_parameters.is_empty() && !unresolved_parameter_values && mappings.recovery_complete {"explicit_annotations_resolved"}else{"partial_unresolved_annotations_or_values"},
            "query_parameter_status":if unknown_parameters.is_empty() && !unresolved_parameter_values && mappings.recovery_complete {"explicit_annotations_resolved"}else{"partial_unresolved_annotations_or_values"},
            "http_method_annotation_status":if unknown_runtime_method_annotations==0 {"recognized_http_annotation"}else{"known_http_annotation_with_unresolved_runtime_annotations"},
            "unknown_parameter_indices":unknown_parameters,"method_annotations":method_annotations,
            "parameter_annotations":parameter_annotations,"original_http_annotations":verbs.iter().map(|v|&v.2).collect::<Vec<_>>(),
            "service_creation_sites":sites,"base_url_link_status":link_status,
            "creation_sites_truncated_globally":links.sites_truncated>0,
            "dex_entry":c.symbols.origin.get(),"runtime_verified":false,"external_control":"not_analyzed"}))?;
        Ok(true)
    }
}
pub(super) fn scan(
    classes: &[&DexClass],
    args: &Value,
    cancel: &AtomicBool,
    budget: usize,
) -> Result<Value> {
    let prefix = args["package_prefix"]
        .as_str()
        .unwrap_or("")
        .trim_end_matches('.');
    let by_type: BTreeMap<_, _> = classes
        .iter()
        .map(|c| (c.descriptor.as_ref(), *c))
        .collect();
    let scoped: Vec<_> = classes
        .iter()
        .copied()
        .filter(|c| {
            let name = type_name(&c.descriptor);
            prefix.is_empty()
                || name == prefix
                || name
                    .strip_prefix(prefix)
                    .is_some_and(|s| s.starts_with('.'))
        })
        .collect();
    let mut scan = Scan {
        work: Work {
            remaining: budget,
            cancel,
        },
        page: Page {
            offset: args["offset"].as_u64().unwrap_or(0) as usize,
            limit: args["limit"].as_u64().unwrap_or(25) as usize,
            total: 0,
            items: vec![],
            bytes: 0,
            full: false,
        },
        errors: vec![],
        error_count: 0,
        classes: 0,
    };
    let mappings = retrofit::discover(classes, cancel)?;
    let links = retrofit::creation_links(classes, &mappings, cancel)?;
    let mut tables = HashMap::new();
    let mut complete = true;
    'classes: for c in &scoped {
        if !scan.work.take(1)? {
            complete = false;
            break;
        }
        scan.classes += 1;
        let name = type_name(&c.descriptor);
        let mut queue = Vec::new();
        let mut seen = BTreeSet::new();
        for ty in c.interfaces.iter().chain(c.superclass.iter()) {
            if !scan.work.take(1)? {
                complete = false;
                break 'classes;
            }
            if seen.len() >= 256 {
                scan.error(
                    &name,
                    "Hierarchy node limit; additional contracts may exist",
                );
                break;
            }
            if seen.insert(ty.as_ref()) {
                queue.push((ty.as_ref(), vec![c.descriptor.as_ref(), ty.as_ref()]));
            }
        }
        while let Some((ty, path)) = queue.pop() {
            if !scan.work.take(1)? {
                complete = false;
                break 'classes;
            }
            if matches!(ty, "Lokhttp3/Interceptor;" | "Lokhttp3/Authenticator;") {
                if c.access_flags & 0x200 == 0 {
                    scan.page.add(json!({"kind":"api_usage","detector":"okhttp_contracts","family":"okhttp3","identification":"exact_descriptor_evidence", "class_id":name,
                    "contract":type_name(ty),"hierarchy_path":path.iter().map(|t|type_name(t)).collect::<Vec<_>>(),"abstract_class":c.access_flags&0x400!=0,"dex_entry":c.symbols.origin.get(),"runtime_verified":false}))?;
                }
                continue;
            }
            if path.len() >= 64 {
                scan.error(
                    &name,
                    "Hierarchy depth limit; additional contracts may exist",
                );
                continue;
            }
            if let Some(parent) = by_type.get(ty) {
                for next in parent.interfaces.iter().chain(parent.superclass.iter()) {
                    if !scan.work.take(1)? {
                        complete = false;
                        break 'classes;
                    }
                    if seen.contains(next.as_ref()) {
                        continue;
                    }
                    if seen.len() >= 256 {
                        scan.error(
                            &name,
                            "Hierarchy node limit; additional contracts may exist",
                        );
                        break;
                    }
                    seen.insert(next.as_ref());
                    let mut chain = path.clone();
                    chain.push(next.as_ref());
                    queue.push((next.as_ref(), chain));
                }
            }
        }
        let key = Arc::as_ptr(&c.symbols);
        if let std::collections::hash_map::Entry::Vacant(entry) = tables.entry(key) {
            match table_roles(&c.symbols, &mut scan.work) {
                Ok(Some(roles)) => {
                    entry.insert(roles);
                }
                Ok(None) => {
                    complete = false;
                    break;
                }
                Err(error) => {
                    scan.error(&name, error);
                    entry.insert(TableRoles::default());
                }
            }
        }
        let roles = &tables[&key];
        let annotations = c.symbols.annotations.get(&c.annotations_offset);
        for (mi, m) in c.methods.iter().enumerate() {
            if !scan.work.take(1)? {
                complete = false;
                break 'classes;
            }
            if let Some(directory) = annotations {
                if let Some(Some(set)) = directory.methods.get(mi)
                    && !scan.annotations(c, m, set, None, &mappings)?
                {
                    complete = false;
                    break 'classes;
                }
                if let Some(parameters) = directory.parameters.get(mi) {
                    for (parameter, set) in parameters.iter().enumerate() {
                        if let Some(set) = set
                            && !scan.annotations(c, m, set, Some(parameter), &mappings)?
                        {
                            complete = false;
                            break 'classes;
                        }
                    }
                }
            }
            if !scan.service(c, m, mi, &mappings, &links)? {
                complete = false;
                break 'classes;
            }
            if roles.methods.is_empty() && roles.fields.is_empty() {
                continue;
            }
            let Some(code) = &m.code else {
                continue;
            };
            let mut pc = 0;
            while pc < code.instructions.len() {
                if !scan.work.take(1)? {
                    complete = false;
                    break 'classes;
                }
                let (width, payload) = match instruction_width(&code.instructions, pc) {
                    Ok((width, payload)) if width > 0 && pc + width <= code.instructions.len() => {
                        (width, payload)
                    }
                    _ => {
                        scan.error(
                            &name,
                            format!("Malformed instruction in {} at {pc}", method_id(m)),
                        );
                        break;
                    }
                };
                if !payload
                    && matches!(code.instructions[pc] as u8,0x6e..=0x72|0x74..=0x78)
                    && let Some(role) = roles.methods.get(&(code.instructions[pc + 1] as usize))
                {
                    match native_xrefs::call_site_at(c,m,pc) {
                        Ok(Some(site))=>scan.page.add(json!({"kind":"api_usage","detector":"named_network_calls","identification":"exact_descriptor_evidence",
                            "family":if site.callee.starts_with("retrofit2.") {"retrofit2"} else {"okhttp3"},"class_id":name,"method_id":site.caller,
                            "callee_method_id":site.callee,"role":role,"code_unit_offset":pc,"dex_entry":site.dex_entry,"method_index":site.method_index,
                            "runtime_verified":false,"client_identity":"not_analyzed","execution_order":"not_analyzed","effective_pinning":"not_analyzed"}))?,
                        Err(error)=>scan.error(&name,error),_=>{},
                    }
                }
                if !payload
                    && code.instructions[pc] as u8 == 0x5b
                    && let Some(role) = roles.fields.get(&(code.instructions[pc + 1] as usize))
                {
                    let index = code.instructions[pc + 1] as usize;
                    let (owner, ty, field) = c.symbols.fields[index];
                    let field_id = format!(
                        "{}.{}:{}",
                        type_name(&c.symbols.types[owner as usize]),
                        c.symbols.strings[field as usize],
                        c.symbols.types[ty as usize]
                    );
                    scan.page.add(json!({"kind":"api_usage","detector":"named_network_fields","identification":"exact_descriptor_evidence",
                        "family":if field_id.starts_with("retrofit2.") {"retrofit2"} else {"okhttp3"},"class_id":name,"method_id":method_id(m),
                        "field_id":field_id,"access":"write","opcode":"iput-object","role":role,"code_unit_offset":pc,"dex_entry":c.symbols.origin.get(),"field_index":index,
                        "runtime_verified":false,"client_identity":"not_analyzed","execution_order":"not_analyzed","effective_pinning":"not_analyzed"}))?;
                }
                pc += width;
            }
        }
    }
    complete &= scan.error_count == 0
        && mappings.recovery_complete
        && !links.budget_exhausted
        && links.sites_truncated == 0;
    Ok(
        json!({"items":scan.page.items,"matching_candidates_so_far":scan.page.total,
        "coverage":{"detector":"known_network_usage","complete":complete,"selected_classes":scoped.len(),"visited_classes":scan.classes,
            "detectors_run":["okhttp_contracts","named_network_calls","named_network_fields","retrofit_annotations","retrofit_service_inventory"],"retrofit_parser_candidates":mappings.parser_candidates,"retrofit_parser_ambiguous":mappings.ambiguous,"retrofit_recovery_complete":mappings.recovery_complete,"retrofit_recovery_status":mappings.status(),"retrofit_recovered_roles":mappings.recovered_roles(),"retrofit_creation":{"model_resolved":links.model_resolved,"candidate_methods":links.candidate_methods,"scanned_methods":links.scanned_methods,"unsupported_methods":links.unsupported_methods,"budget_exhausted":links.budget_exhausted,"sites_seen":links.sites_seen,"sites_retained":links.sites_seen-links.sites_truncated,"sites_truncated":links.sites_truncated,"supported_scope":"branch-free local methods up to 2048 code units, no try regions; exact create(Class) reference, class constant, tracked builder receiver and literal base URL when available"},"work_budget_exhausted":scan.work.remaining==0 || links.budget_exhausted,
            "errors":scan.errors,"error_count":scan.error_count,"scope":"Exact named OkHttp/Retrofit descriptors and conservatively mapped Retrofit annotations in selected classes. Service methods are static declarations; creation and base URL links require separate object dataflow evidence. Reflection and unsupported flows remain unresolved."}}),
    )
}
pub(super) fn services(
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
    let scoped: Vec<_> = classes
        .iter()
        .copied()
        .filter(|c| {
            let name = type_name(&c.descriptor);
            prefix.is_empty()
                || name == prefix
                || name
                    .strip_prefix(prefix)
                    .is_some_and(|s| s.starts_with('.'))
        })
        .collect();
    let mappings = retrofit::discover(classes, cancel)?;
    let links = retrofit::creation_links(classes, &mappings, cancel)?;
    let mut scan = Scan {
        work: Work {
            remaining: budget,
            cancel,
        },
        page: Page {
            offset,
            limit,
            total: 0,
            items: vec![],
            bytes: 0,
            full: false,
        },
        errors: vec![],
        error_count: 0,
        classes: 0,
    };
    let mut complete = true;
    'classes: for c in &scoped {
        if !scan.work.take(1)? {
            complete = false;
            break;
        }
        scan.classes += 1;
        if c.access_flags & 0x200 == 0 {
            continue;
        }
        for (mi, m) in c.methods.iter().enumerate() {
            if !scan.work.take(1)? {
                complete = false;
                break 'classes;
            }
            if !scan.service(c, m, mi, &mappings, &links)? {
                complete = false;
                break 'classes;
            }
        }
    }
    complete &= scan.error_count == 0
        && mappings.recovery_complete
        && !links.budget_exhausted
        && links.sites_truncated == 0;
    if complete {
        ensure!(offset <= scan.page.total, "Invalid service row offset");
    }
    let end = offset + scan.page.items.len();
    Ok(
        json!({"projection":"services","items":scan.page.items,"offset":offset,"limit":limit,
        "total":complete.then_some(scan.page.total),"matching_candidates_so_far":scan.page.total,
        "next_offset":(end<scan.page.total).then_some(end),"coverage":{"detector":"retrofit_service_inventory","complete":complete,
            "selected_classes":scoped.len(),"visited_classes":scan.classes,"retrofit_parser_candidates":mappings.parser_candidates,
            "retrofit_parser_ambiguous":mappings.ambiguous,"retrofit_recovery_complete":mappings.recovery_complete,"retrofit_recovery_status":mappings.status(),"retrofit_recovered_roles":mappings.recovered_roles(),
            "creation_model_resolved":links.model_resolved,"creation_candidate_methods":links.candidate_methods,
            "creation_scanned_methods":links.scanned_methods,"creation_unsupported_methods":links.unsupported_methods,
            "creation_budget_exhausted":links.budget_exhausted,"creation_sites_seen":links.sites_seen,
            "creation_sites_truncated":links.sites_truncated,"work_budget_exhausted":scan.work.remaining==0,
            "scope":"Runtime-visible interface method annotations with exact or conservatively recovered Retrofit roles. Rows are static declarations. Creation links cover branch-free local methods only; model assumptions, dynamic values and external control remain explicit."},
        "empty_result_note":if scan.page.total==0 {Some("No recognized Retrofit service-method annotations in this scope; renamed or unsupported parser variants remain possible.")} else {None::<&str>}}),
    )
}
#[cfg(test)]
mod focused_tests {
    use super::*;
    #[test]
    fn recovered_roles_obey_location_and_url_unknown_precedence() {
        assert_eq!(recovered_annotation_role("Url", false), None);
        assert_eq!(recovered_annotation_role("GET", true), None);
        assert_eq!(
            recovered_annotation_role("Url", true),
            Some("url_parameter")
        );
        assert_eq!(
            recovered_annotation_role("GET", false),
            Some("service_method_annotation")
        );
        assert_eq!(dynamic_url_value(true, true, true), json!(true));
        assert_eq!(dynamic_url_value(false, true, true), Value::Null);
        assert_eq!(dynamic_url_value(false, false, true), json!(false));
    }
}
