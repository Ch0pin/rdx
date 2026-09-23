//! Optional JavaScript execution check; uses a mock bridge, never a device.
use rdx::frida_snippet::for_method;
use std::{
    io::Write,
    process::{Command, Stdio},
};

#[test]
#[ignore = "Requires Node.js to execute snippets against a mock Frida bridge"]
fn generated_hooks_preserve_overloads_receivers_results_and_exceptions() {
    let symbols = [
        "app.Outer$Client.load([ILjava/lang/String;)I",
        "app.Widget.<init>()V",
        "app.Widget.run()V",
        "app.Widget.fail()I",
        "app.class.weird\"name()I",
    ];
    let snippets: Vec<_> = symbols.iter().map(|s| for_method(s).unwrap()).collect();
    let script = format!(
        r#"
const assert = require('node:assert/strict');
const hooks = [];
const logs = [];
const console = {{log: (...args) => logs.push(args)}};
const Java = {{
  perform: fn => fn(),
  use: owner => new Proxy({{}}, {{get: (_, name) => ({{overload: (...types) => {{
    const hook = {{owner, name, types, calls: [], call(receiver, ...args) {{
      this.calls.push([receiver, ...args]);
      if (name === 'fail') throw failure;
      return sentinel;
    }}}};
    hooks.push(hook);
    return hook;
  }}}})}})
}};
const failure = new Error('original failure');
const sentinel = {{value: 42}};
const receiver = {{id: 'receiver'}};
for (const snippet of {}) eval(snippet);
assert.equal(hooks.length, 5);
assert.deepEqual(hooks[0].types, ['[I', 'java.lang.String']);
assert.equal(hooks[0].owner, 'app.Outer$Client');
assert.equal(hooks[0].implementation.call(receiver, [1, 2], 'value'), sentinel);
assert.deepEqual(hooks[0].calls, [[receiver, [1, 2], 'value']]);
assert.equal(hooks[1].name, '$init');
assert.equal(hooks[1].implementation.call(receiver), undefined);
assert.equal(hooks[2].implementation.call(receiver), undefined);
assert.throws(() => hooks[3].implementation.call(receiver), error => error === failure);
assert.equal(hooks[4].implementation.call(receiver), sentinel);
for (const hook of hooks) assert.equal(hook.calls.length, 1);
assert.equal(logs.length, 7);
"#,
        serde_json::to_string(&snippets).unwrap()
    );
    let mut child = Command::new("node")
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(script.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
