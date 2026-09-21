//! Native example of RDX's optional external-plugin protocol.
use serde_json::{Value, json};
use std::io::{self, BufRead, Write};

const MAX_REQUEST: usize = 64 * 1024 * 1024;

fn response(request: &Value) -> Value {
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let result = if request.get("protocol").and_then(Value::as_u64) == Some(1)
        && request.get("method").and_then(Value::as_str) == Some("source.analyze")
    {
        match (
            request.get("class").and_then(Value::as_str),
            request.get("source").and_then(Value::as_str),
        ) {
            (Some(class), Some(source)) => Ok(json!({
                "class": class,
                "lines": source.lines().count(),
                "characters": source.chars().count(),
            })),
            _ => Err("Class and source must be strings"),
        }
    } else {
        Err("Unsupported request")
    };
    match result {
        Ok(result) => json!({"protocol": 1, "id": id, "result": result}),
        Err(error) => json!({"protocol": 1, "id": id, "error": error}),
    }
}

fn main() -> anyhow::Result<()> {
    let stdin = io::stdin();
    let mut input = stdin.lock();
    let stdout = io::stdout();
    let mut output = stdout.lock();
    loop {
        let mut line = Vec::new();
        loop {
            let buffer = input.fill_buf()?;
            if buffer.is_empty() {
                break;
            }
            let count = buffer
                .iter()
                .position(|&b| b == b'\n')
                .map_or(buffer.len(), |n| n + 1);
            anyhow::ensure!(
                line.len() + count <= MAX_REQUEST,
                "Plugin request exceeds 64 MiB"
            );
            let newline = buffer[count - 1] == b'\n';
            line.extend_from_slice(&buffer[..count]);
            input.consume(count);
            if newline {
                break;
            }
        }
        if line.is_empty() {
            break;
        }
        let request: Value = serde_json::from_slice(&line)?;
        serde_json::to_writer(&mut output, &response(&request))?;
        writeln!(output)?;
        output.flush()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unicode_counts_and_request_ids() {
        let result = response(
            &json!({"protocol":1,"id":91,"method":"source.analyze","class":"C","source":"α\n😀\n"}),
        );
        assert_eq!(
            result,
            json!({"protocol":1,"id":91,"result":{"class":"C","lines":2,"characters":4}})
        );
    }
    #[test]
    fn invalid_request_is_explicit() {
        let result = response(&json!({"protocol":1,"id":8,"method":"other"}));
        assert_eq!(result["id"], 8);
        assert!(result.get("error").is_some());
    }
}
