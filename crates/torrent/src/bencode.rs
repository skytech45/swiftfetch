//! Minimal strict bencode parser and encoder (BEP 3).
//!
//! Used for `.torrent` metainfo and as a fuzz target for the M6 hardening
//! pass. The parser is total: every input yields either a value or a
//! position-annotated error — it never panics, however adversarial the
//! bytes.

use std::collections::BTreeMap;

/// A bencoded value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    /// Integer (`i<digits>e`).
    Int(i64),
    /// Byte string (`<len>:<bytes>`).
    Bytes(Vec<u8>),
    /// List (`l<items>e`).
    List(Vec<Value>),
    /// Dictionary (`d<key/value pairs>e`, keys are byte strings).
    Dict(BTreeMap<Vec<u8>, Value>),
}

/// Bencode parse failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("bencode error at byte {offset}: {message}")]
pub struct ParseError {
    /// Byte offset where parsing failed.
    pub offset: usize,
    /// Human-readable reason.
    pub message: String,
}

struct Parser<'a> {
    input: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn new(input: &'a [u8]) -> Self {
        Self { input, pos: 0 }
    }

    fn fail<T>(&self, message: impl Into<String>) -> Result<T, ParseError> {
        Err(ParseError {
            offset: self.pos.min(self.input.len()),
            message: message.into(),
        })
    }

    fn peek(&self) -> Option<u8> {
        self.input.get(self.pos).copied()
    }

    fn parse_value(&mut self, depth: usize) -> Result<Value, ParseError> {
        if depth > 64 {
            return self.fail("nesting too deep");
        }
        match self.peek() {
            Some(b'i') => self.parse_int(),
            Some(b'l') => {
                self.pos += 1;
                let mut items = Vec::new();
                while self.peek() != Some(b'e') {
                    if self.peek().is_none() {
                        return self.fail("unterminated list");
                    }
                    items.push(self.parse_value(depth + 1)?);
                }
                self.pos += 1;
                Ok(Value::List(items))
            }
            Some(b'd') => {
                self.pos += 1;
                let mut map = BTreeMap::new();
                while self.peek() != Some(b'e') {
                    if self.peek().is_none() {
                        return self.fail("unterminated dict");
                    }
                    let Value::Bytes(key) = self.parse_value(depth + 1)? else {
                        return self.fail("dict key is not a byte string");
                    };
                    if map.contains_key(&key) {
                        return self.fail("duplicate dict key");
                    }
                    if self.peek().is_none() {
                        return self.fail("dict value missing");
                    }
                    let value = self.parse_value(depth + 1)?;
                    map.insert(key, value);
                }
                self.pos += 1;
                Ok(Value::Dict(map))
            }
            Some(b'0'..=b'9') => Ok(Value::Bytes(self.parse_bytes()?)),
            Some(other) => self.fail(format!("unexpected byte `{other}`")),
            None => self.fail("unexpected end of input"),
        }
    }

    fn parse_int(&mut self) -> Result<Value, ParseError> {
        self.pos += 1; // consume 'i'
        let start = self.pos;
        while self.peek().is_some_and(|b| b != b'e') {
            self.pos += 1;
        }
        if self.peek().is_none() {
            return self.fail("unterminated integer");
        }
        let text = self.input.get(start..self.pos).unwrap_or_default();
        self.pos += 1; // consume 'e'
        let text = str::from_utf8(text).map_err(|_| ParseError {
            offset: start,
            message: "integer is not ASCII".to_owned(),
        })?;
        if text.is_empty() {
            return self.fail("empty integer");
        }
        if text.len() > 1 && (text.starts_with('0') || text.starts_with("-0")) {
            return self.fail("non-canonical integer (leading zero)");
        }
        if text.len() > 20 {
            return self.fail("integer too long");
        }
        text.parse::<i64>().map(Value::Int).map_err(|_| ParseError {
            offset: start,
            message: "integer out of range".to_owned(),
        })
    }

    fn parse_bytes(&mut self) -> Result<Vec<u8>, ParseError> {
        let start = self.pos;
        while self.peek().is_some_and(|b| b.is_ascii_digit()) {
            self.pos += 1;
        }
        if self.peek() != Some(b':') {
            return self.fail("missing `:` in byte string");
        }
        let len_text =
            str::from_utf8(self.input.get(start..self.pos).unwrap_or_default()).unwrap_or("");
        self.pos += 1; // consume ':'
        let len: usize = len_text.parse().map_err(|_| ParseError {
            offset: start,
            message: "bad string length".to_owned(),
        })?;
        if len > 64 * 1024 * 1024 {
            return self.fail("byte string too long (> 64 MiB)");
        }
        let end = self.pos.saturating_add(len);
        let bytes = self.input.get(self.pos..end).ok_or_else(|| ParseError {
            offset: self.pos,
            message: "byte string overruns input".to_owned(),
        })?;
        self.pos = end;
        Ok(bytes.to_vec())
    }
}

/// Parses exactly one bencoded value; trailing bytes are an error.
///
/// # Errors
///
/// Returns [`ParseError`] on any malformed input.
pub fn parse(input: &[u8]) -> Result<Value, ParseError> {
    let mut parser = Parser::new(input);
    let value = parser.parse_value(0)?;
    if parser.pos != input.len() {
        return Err(ParseError {
            offset: parser.pos,
            message: "trailing bytes after value".to_owned(),
        });
    }
    Ok(value)
}

/// Encodes a value in canonical bencode (dict keys sorted — [`BTreeMap`]
/// iteration order is already sorted).
pub fn encode(value: &Value, out: &mut Vec<u8>) {
    match value {
        Value::Int(n) => {
            out.extend_from_slice(b"i");
            out.extend_from_slice(n.to_string().as_bytes());
            out.extend_from_slice(b"e");
        }
        Value::Bytes(b) => {
            out.extend_from_slice(b.len().to_string().as_bytes());
            out.extend_from_slice(b":");
            out.extend_from_slice(b);
        }
        Value::List(items) => {
            out.extend_from_slice(b"l");
            for item in items {
                encode(item, out);
            }
            out.extend_from_slice(b"e");
        }
        Value::Dict(map) => {
            out.extend_from_slice(b"d");
            for (key, val) in map {
                out.extend_from_slice(key.len().to_string().as_bytes());
                out.extend_from_slice(b":");
                out.extend_from_slice(key);
                encode(val, out);
            }
            out.extend_from_slice(b"e");
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn round_trip() {
        let value = Value::Dict(BTreeMap::from([
            (b"announce".to_vec(), Value::Bytes(b"http://t/x".to_vec())),
            (b"n".to_vec(), Value::Int(-42)),
            (
                b"l".to_vec(),
                Value::List(vec![Value::Int(1), Value::Int(2)]),
            ),
        ]));
        let mut buf = Vec::new();
        encode(&value, &mut buf);
        assert_eq!(parse(&buf).expect("round trip"), value);
    }

    #[test]
    fn rejects_adversarial_inputs_without_panicking() {
        // Truncated, over-nested, duplicate keys, leading zeros, overruns.
        let mut deep = vec![b'l'; 70];
        deep.extend_from_slice(b"i1e");
        deep.extend_from_slice(&[b'e'; 70]);
        let cases: Vec<Vec<u8>> = vec![
            b"i".to_vec(),
            b"i12".to_vec(),
            b"i03e".to_vec(),
            b"i-0e".to_vec(),
            b"4:ab".to_vec(),
            b"999999999:".to_vec(),
            b"le".to_vec(),
            b"de".to_vec(),
            b"d3:foo3:bar3:foo3:baz".to_vec(),
            b"i1e trailing".to_vec(),
            b"".to_vec(),
            deep,
            b"d".to_vec(),
            b"l".to_vec(),
            b"10:x".to_vec(),
        ];
        for input in &cases {
            let _ = parse(input);
        }
        // And random-ish mutations of a valid value never panic either.
        let mut valid = Vec::new();
        encode(
            &Value::Dict(BTreeMap::from([(
                b"info".to_vec(),
                Value::Dict(BTreeMap::from([(b"length".to_vec(), Value::Int(12345))])),
            )])),
            &mut valid,
        );
        for i in 0..valid.len() {
            let mut mutated = valid.clone();
            mutated[i] = mutated[i].wrapping_add(1);
            let _ = parse(&mutated);
            let mut truncated = valid.clone();
            truncated.truncate(i);
            let _ = parse(&truncated);
        }
    }
}
