/// io.sdk's `spotter.json`: `{"models": [{"type": ..., "path": ...}, ...]}`.
pub fn model_dir(spotter_json: &str, kind: &str) -> Result<String, String> {
    let mut p = Parser {
        s: spotter_json.as_bytes(),
        at: 0,
    };
    let root = p.document()?;
    let models = root
        .get("models")
        .and_then(Value::array)
        .ok_or("no \"models\" array")?;
    models
        .iter()
        .find(|m| m.get("type").and_then(Value::str) == Some(kind))
        .ok_or_else(|| format!("no {kind} model"))?
        .get("path")
        .and_then(Value::str)
        .map(str::to_owned)
        .ok_or_else(|| format!("{kind} model has no path"))
}

#[derive(Debug, PartialEq)]
enum Value {
    Scalar,
    Str(String),
    Array(Vec<Value>),
    Object(Vec<(String, Value)>),
}

impl Value {
    fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Object(fields) => fields.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    fn array(&self) -> Option<&[Value]> {
        match self {
            Value::Array(items) => Some(items),
            _ => None,
        }
    }

    fn str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }
}

const MAX_DEPTH: usize = 32;

struct Parser<'a> {
    s: &'a [u8],
    at: usize,
}

impl Parser<'_> {
    fn document(&mut self) -> Result<Value, String> {
        let v = self.value(0)?;
        self.skip_ws();
        if self.at != self.s.len() {
            return Err(self.error("trailing data"));
        }
        Ok(v)
    }

    fn error(&self, what: &str) -> String {
        format!("spotter.json: {what} at byte {}", self.at)
    }

    fn skip_ws(&mut self) {
        while self.s.get(self.at).is_some_and(u8::is_ascii_whitespace) {
            self.at += 1;
        }
    }

    fn eat(&mut self, b: u8) -> bool {
        self.skip_ws();
        let hit = self.s.get(self.at) == Some(&b);
        if hit {
            self.at += 1;
        }
        hit
    }

    fn expect(&mut self, b: u8) -> Result<(), String> {
        if self.eat(b) {
            Ok(())
        } else {
            Err(self.error(&format!("expected '{}'", char::from(b))))
        }
    }

    fn value(&mut self, depth: usize) -> Result<Value, String> {
        if depth > MAX_DEPTH {
            return Err(self.error("nesting too deep"));
        }
        self.skip_ws();
        match self.s.get(self.at) {
            Some(b'{') => self.object(depth),
            Some(b'[') => self.array(depth),
            Some(b'"') => self.string().map(Value::Str),
            Some(b'-' | b'0'..=b'9' | b't' | b'f' | b'n') => {
                let start = self.at;
                while self
                    .s
                    .get(self.at)
                    .is_some_and(|b| b.is_ascii_alphanumeric() || b"+-.".contains(b))
                {
                    self.at += 1;
                }
                let word = &self.s[start..self.at];
                let number = word[0] == b'-' || word[0].is_ascii_digit();
                if number || matches!(word, b"true" | b"false" | b"null") {
                    Ok(Value::Scalar)
                } else {
                    Err(self.error("bad literal"))
                }
            }
            _ => Err(self.error("expected a value")),
        }
    }

    fn object(&mut self, depth: usize) -> Result<Value, String> {
        self.at += 1;
        let mut fields = Vec::new();
        if self.eat(b'}') {
            return Ok(Value::Object(fields));
        }
        loop {
            self.skip_ws();
            let key = self.string()?;
            self.expect(b':')?;
            fields.push((key, self.value(depth + 1)?));
            if self.eat(b'}') {
                return Ok(Value::Object(fields));
            }
            self.expect(b',')?;
        }
    }

    fn array(&mut self, depth: usize) -> Result<Value, String> {
        self.at += 1;
        let mut items = Vec::new();
        if self.eat(b']') {
            return Ok(Value::Array(items));
        }
        loop {
            items.push(self.value(depth + 1)?);
            if self.eat(b']') {
                return Ok(Value::Array(items));
            }
            self.expect(b',')?;
        }
    }

    fn string(&mut self) -> Result<String, String> {
        if self.s.get(self.at) != Some(&b'"') {
            return Err(self.error("expected a string"));
        }
        self.at += 1;
        let mut out = Vec::new();
        loop {
            let Some(&b) = self.s.get(self.at) else {
                return Err(self.error("unterminated string"));
            };
            self.at += 1;
            match b {
                b'"' => break,
                b'\\' => {
                    let Some(&e) = self.s.get(self.at) else {
                        return Err(self.error("unterminated string"));
                    };
                    self.at += 1;
                    match e {
                        b'"' | b'\\' | b'/' => out.push(e),
                        b'b' => out.push(0x08),
                        b'f' => out.push(0x0c),
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'u' => {
                            let c = self.unicode_escape()?;
                            out.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes());
                        }
                        _ => return Err(self.error("bad escape")),
                    }
                }
                _ => out.push(b),
            }
        }
        String::from_utf8(out).map_err(|_| self.error("invalid UTF-8"))
    }

    fn unicode_escape(&mut self) -> Result<char, String> {
        let first = self.hex4()?;
        let code = if (0xd800..0xdc00).contains(&first) {
            if self.s.get(self.at..self.at + 2) != Some(b"\\u") {
                return Err(self.error("lone surrogate"));
            }
            self.at += 2;
            let low = self.hex4()?;
            if !(0xdc00..0xe000).contains(&low) {
                return Err(self.error("lone surrogate"));
            }
            0x10000 + ((first - 0xd800) << 10) + (low - 0xdc00)
        } else {
            first
        };
        char::from_u32(code).ok_or_else(|| self.error("bad \\u escape"))
    }

    fn hex4(&mut self) -> Result<u32, String> {
        let digits = self
            .s
            .get(self.at..self.at + 4)
            .and_then(|d| std::str::from_utf8(d).ok())
            .and_then(|d| u32::from_str_radix(d, 16).ok())
            .ok_or_else(|| self.error("bad \\u escape"))?;
        self.at += 4;
        Ok(digits)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CATALOG: &str = r#"{"models":[
        {"category":"spotter","crc":"1","path":"/data/m/a1","type":"activation/alisa","url":"https://x/a.zip"},
        {"category":"spotter","crc":"2","path":"/data/m/c2","type":"command"},
        {"category":"spotter","crc":3,"path":"/data/m/v3","type":"vad","on":true,"off":false,"n":null,"x":-1.5e3}
    ]}"#;

    fn parse(text: &str) -> Result<Value, String> {
        Parser {
            s: text.as_bytes(),
            at: 0,
        }
        .document()
    }

    #[test]
    fn finds_the_directory_of_a_model_type() {
        assert_eq!(
            model_dir(CATALOG, "activation/alisa").unwrap(),
            "/data/m/a1"
        );
        assert_eq!(model_dir(CATALOG, "vad").unwrap(), "/data/m/v3");
    }

    #[test]
    fn missing_pieces_are_errors() {
        assert!(model_dir(CATALOG, "counteractivation").is_err());
        assert!(model_dir(r#"{"models":[{"type":"vad"}]}"#, "vad").is_err());
        assert!(model_dir(r#"{"models":[{"type":"vad","path":7}]}"#, "vad").is_err());
        assert!(model_dir(r#"{"spotters":[]}"#, "vad").is_err());
        assert!(model_dir(r#"{"models":{}}"#, "vad").is_err());
        assert!(model_dir("[]", "vad").is_err());
        assert!(model_dir(r#"{"models":[1,"a"]}"#, "vad").is_err());
    }

    #[test]
    fn strings_decode_escapes() {
        let v = parse(r#"{"k":"a\"b\\c\/d\b\f\n\r\t\u0416\ud83d\ude00"}"#).unwrap();
        assert_eq!(
            v.get("k").and_then(Value::str),
            Some("a\"b\\c/d\u{8}\u{c}\n\r\tЖ😀")
        );
        assert_eq!(parse(r#""Алиса""#).unwrap(), Value::Str("Алиса".into()));
    }

    #[test]
    fn empty_containers_and_whitespace_parse() {
        assert_eq!(
            parse(" { \"a\" : [ ] , \"b\" : { } } \n").unwrap(),
            Value::Object(vec![
                ("a".into(), Value::Array(vec![])),
                ("b".into(), Value::Object(vec![])),
            ])
        );
    }

    #[test]
    fn malformed_documents_are_rejected() {
        for bad in [
            "",
            "{",
            "{\"a\"}",
            "{\"a\":1,}",
            "{a:1}",
            "[1 2]",
            "[1,]",
            "\"abc",
            "\"a\\",
            "\"\\x\"",
            "\"\\u12\"",
            "\"\\ud800\"",
            "\"\\ud800\\u0041\"",
            "\"\\udc00\"",
            "\"\u{1}\\uZZZZ\"",
            "nope",
            "{} {}",
            "}",
        ] {
            assert!(parse(bad).is_err(), "{bad:?}");
        }
        assert!(
            Parser {
                s: b"\"\xff\"",
                at: 0
            }
            .document()
            .is_err()
        );
    }

    #[test]
    fn deep_nesting_is_rejected_instead_of_overflowing_the_stack() {
        let deep = "[".repeat(10_000);
        assert!(parse(&deep).unwrap_err().contains("too deep"));
        let nest = |n| format!("{}{}", "[".repeat(n), "]".repeat(n));
        assert!(parse(&nest(MAX_DEPTH + 1)).is_ok());
        assert!(parse(&nest(MAX_DEPTH + 2)).is_err());
    }
}
