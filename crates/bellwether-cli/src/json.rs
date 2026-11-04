//! Minimal JSON writing for `decode` and `probe --json`. (The CLI's dependency
//! budget doesn't include serde_json.)

use bellwether::{Report, Support};

/// A JSON string literal, quotes included.
pub fn string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c == '\u{7f}' => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn array(items: &[String]) -> String {
    let inner: Vec<String> = items.iter().map(|s| string(s)).collect();
    format!("[{}]", inner.join(","))
}

/// A report as one JSON object. Keys appear in wire order, and absent keys are
/// left out.
pub fn report(r: &Report) -> String {
    let mut fields = vec![format!("\"state\":{}", string(r.state().as_str()))];
    if let Some(k) = r.kind() {
        fields.push(format!("\"kind\":{}", string(k.as_str())));
    }
    if let Some(a) = r.app() {
        fields.push(format!("\"app\":{}", string(a.as_str())));
    }
    if let Some(i) = r.id() {
        fields.push(format!("\"id\":{}", string(i.as_str())));
    }
    if let Some(t) = r.title() {
        fields.push(format!("\"title\":{}", string(t)));
    }
    if let Some(p) = r.progress() {
        fields.push(format!("\"progress\":{}", p.get()));
    }
    if let Some(m) = r.msg() {
        fields.push(format!("\"msg\":{}", string(m)));
    }
    format!("{{{}}}", fields.join(","))
}

/// `"states":[…],"kinds":[…]`, without braces.
pub fn support_fields(s: &Support) -> String {
    format!(
        "\"states\":{},\"kinds\":{}",
        array(&s.states),
        array(&s.kinds)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes() {
        assert_eq!(string("a\"b\\c\n\u{1b}é"), "\"a\\\"b\\\\c\\n\\u001bé\"");
    }
}
