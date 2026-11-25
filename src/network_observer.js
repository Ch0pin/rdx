// Optional local observer for the exact selectors in this map.
// Header names and methods are limited to the literal allowlists below.
Java.perform(function () {
    "use strict";
    const methods = new Set(["GET", "HEAD", "POST", "PUT", "DELETE", "PATCH", "OPTIONS", "CONNECT", "TRACE"]);
    const credentials = ["authorization", "proxy-authorization", "cookie", "x-api-key"];
    const headerNames = new Set([
        ...credentials, "accept", "accept-encoding", "accept-language", "cache-control",
        "content-length", "content-type", "host", "origin", "referer", "user-agent"
    ]);
    function exact(owner, member, params, returned) {
        const matches = Java.use(owner)[member].overloads.filter(function (candidate) {
            return candidate.returnType.className === returned
                && candidate.argumentTypes.length === params.length
                && candidate.argumentTypes.every(function (argument, i) {
                    return argument.className === params[i];
                });
        });
        if (matches.length !== 1) throw new Error("Selector unavailable");
        return matches[0];
    }
    function emit(event) {
        try {
            console.log(JSON.stringify(event));
        } catch (_) {}
    }
    try {
        const original = exact(selectors.owner, selectors.member, [selectors.request], "okhttp3.Response");
        const requestUrl = selectors.url && exact(selectors.request, selectors.url[0], [], "okhttp3.HttpUrl");
        const convertUrl = selectors.url && exact("okhttp3.HttpUrl", selectors.url[1], [], "java.net.URL");
        const requestMethod = selectors.method && exact(selectors.request, selectors.method, [], "java.lang.String");
        const requestHeaders = selectors.headers && exact(selectors.request, selectors.headers[0], [], "okhttp3.Headers");
        const headerName = selectors.headers && exact("okhttp3.Headers", selectors.headers[1], ["int"], "java.lang.String");
        const headerPairs = selectors.headers && Java.use("okhttp3.Headers").class.getDeclaredField(selectors.headers[2]);
        const array = selectors.headers && Java.use("java.lang.reflect.Array");
        if (headerPairs) {
            if (String(headerPairs.getType().getName()) !== "[Ljava.lang.String;") throw new Error("Header storage unavailable");
            headerPairs.setAccessible(true);
        }
        original.implementation = function (request) {
            const event = {};
            try {
                if (requestUrl) {
                    const url = convertUrl.call(requestUrl.call(request));
                    const scheme = String(url.getProtocol()).toLowerCase();
                    const host = String(url.getHost());
                    const explicitPort = Number(url.getPort());
                    const port = explicitPort < 0 ? Number(url.getDefaultPort()) : explicitPort;
                    if ((scheme === "https" || scheme === "http")
                        && /^[A-Za-z0-9.:%_\[\]-]{1,253}$/.test(host)
                        && Number.isInteger(port) && port > 0 && port <= 65535) {
                        event.origin = { scheme: scheme, host: host, port: port };
                    }
                }
            } catch (_) {}
            try {
                if (requestMethod) {
                    const method = String(requestMethod.call(request));
                    if (methods.has(method)) event.http_method = method;
                }
            } catch (_) {}
            try {
                if (requestHeaders) {
                    event.headers_complete = false;
                    event.header_names_scope = "allowlisted";
                    const headers = requestHeaders.call(request);
                    const length = Number(array.getLength(headerPairs.get(headers)));
                    const names = new Set();
                    const count = Number.isSafeInteger(length) && length >= 0 && length % 2 === 0 ? length / 2 : null;
                    let observed = 0;
                    let omitted = 0;
                    for (let index = 0; count !== null && index < Math.min(count, 256); index++) {
                        let name;
                        try {
                            name = headerName.call(headers, index);
                        } catch (_) {
                            break;
                        }
                        if (name === null) {
                            break;
                        }
                        const lower = String(name).toLowerCase();
                        if (!/^[!#$%&'*+.^_`|~0-9a-z-]{1,256}$/.test(lower)) break;
                        if (headerNames.has(lower)) names.add(lower);
                        else omitted++;
                        observed++;
                    }
                    const complete = count !== null && observed === count;
                    event.headers_complete = complete;
                    event.header_names = Array.from(names).sort();
                    event.omitted_header_name_count = omitted;
                    const presence = {};
                    for (const name of credentials) {
                        if (names.has(name)) presence[name] = true;
                        else if (complete) presence[name] = false;
                    }
                    event.credential_presence = presence;
                }
            } catch (_) {}
            if (Object.keys(event).length) emit(event);
            return original.call(this, request);
        };
    } catch (_) {
        emit({ observer_status: "unavailable", phase: "install" });
    }
});
