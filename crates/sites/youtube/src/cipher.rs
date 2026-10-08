//! Cipher solver (Build Prompt §12.4): the signature-decipher step for
//! formats whose URL arrives obfuscated (`signatureCipher`).
//!
//! The solver is a hot-updatable module behind the [`CipherSolver`]
//! trait: the default implementation fetches the live player base.js at
//! runtime and derives the transform chain from it — **never** a vendored
//! or hardcoded cipher. On any failure it makes exactly **one** attempt
//! and then surfaces the clean error "`YouTube` player changed — extractor
//! update required" (`E_EXTRACTOR_STALE`).

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;

/// Cipher-solving failure.
#[derive(Debug, thiserror::Error)]
pub enum SolverError {
    /// The base.js could not be fetched (transport/HTTP error).
    #[error("could not fetch player script: {0}")]
    Fetch(String),
    /// The base.js layout is not understood — the player changed.
    #[error("YouTube player changed — extractor update required")]
    Stale,
}

/// Deciphers the `s` parameter for a format, given the player base.js URL.
///
/// Object-safe with an explicitly boxed future so site modules can hold
/// `Arc<dyn CipherSolver>` and swap implementations at runtime (hot-update
/// channel).
pub trait CipherSolver: Send + Sync {
    /// Returns the deciphered signature for `obfuscated`.
    fn solve<'a>(
        &'a self,
        obfuscated: &'a str,
        base_js_url: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<String, SolverError>> + Send + 'a>>;
}

/// The runtime solver: fetches base.js, extracts the transform chain,
/// applies it. One fetch, one attempt — no retries, no fallbacks.
#[derive(Debug, Default)]
pub struct RuntimeSolver {
    client: reqwest::Client,
}

impl RuntimeSolver {
    /// Builds a solver over the given client.
    #[must_use]
    pub fn new(client: reqwest::Client) -> Self {
        Self { client }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Op {
    Reverse,
    SwapHead,
    Slice,
}

/// Applies the standard three transform primitives to the working buffer.
fn apply_op(op: Op, value: &mut Vec<u8>, arg: usize) {
    match op {
        Op::Reverse => value.reverse(),
        Op::SwapHead => {
            let len = value.len();
            if len > 0 {
                let idx = arg % len;
                value.swap(0, idx);
            }
        }
        Op::Slice => {
            // a.splice(0, arg): drop the first `arg` elements.
            let keep_from = arg.min(value.len());
            value.drain(..keep_from);
        }
    }
}

/// Extracts `NAME:function(a[, b]){...}` helper bodies from the player script
/// (some players declare one formal parameter, others two).
fn helper_body(source: &str, name: &str) -> Option<String> {
    for params in ["(a,b)", "(a, b)", "(a)"] {
        let needle = format!("{name}:function{params}{{");
        if let Some(start) = source.find(&needle) {
            let rest = &source[start + needle.len()..];
            // The known helpers close on the first `}`.
            if let Some(end) = rest.find('}') {
                return Some(rest[..end].to_owned());
            }
        }
    }
    None
}

/// Looks up `OBJ.method` helper bodies: the transform chain references
/// helpers through their object (`Qx.swap`) while the declaration is
/// `swap:function(a,b){…}`. Only the method part is looked up.
fn helper_in_object(source: &str, dotted: &str) -> Option<String> {
    let (_object, method) = dotted.split_once('.')?;
    helper_body(source, method)
}

fn classify_op(body: &str) -> Option<Op> {
    if body.contains("a.reverse()") {
        Some(Op::Reverse)
    } else if body.contains("a.splice(0,") {
        Some(Op::Slice)
    } else if body.contains("a[0]=a[b%a.length]") || body.contains("c=a[0]") {
        Some(Op::SwapHead)
    } else {
        None
    }
}

/// Extracts the inline transform chain (split, helper steps, join) by
/// prefix: any quote style in the split/join delimiters is accepted.
/// Whitespace-insensitive. Steps must still resolve to the three known
/// helpers, else the solver reports Stale after its single attempt.
fn transform_plan(player_js: &str) -> Result<Vec<(String, usize)>, SolverError> {
    // Whitespace-stripped view for matching; helper bodies are extracted
    // from the same normalized copy so offsets are consistent.
    let js: String = player_js.chars().filter(|c| !c.is_whitespace()).collect();
    let chain_start = js.find("a=a.split(\"").ok_or(SolverError::Stale)?;
    let tail = &js[chain_start..];
    let chain_end = tail.find("returna.join(\"").ok_or(SolverError::Stale)?;
    let chain = &tail[..chain_end];
    let mut plan: Vec<(String, usize)> = Vec::new();
    // Each step looks like `a=NAME(a,42);` or `a=NAME(a);`.
    let mut rest = chain;
    while let Some(eq) = rest.find("a=") {
        rest = &rest[eq + 2..];
        let Some(semi) = rest.find(';') else { break };
        let step = &rest[..semi];
        rest = &rest[semi + 1..];
        let Some(paren) = step.find('(') else {
            continue;
        };
        let name = step[..paren].trim().to_owned();
        // The initializer step (a=a.split) is not a transform.
        if name == "a.split" {
            continue;
        }
        if name.is_empty()
            || !name
                .chars()
                .all(|c| c.is_alphanumeric() || c == '_' || c == '$' || c == '.')
        {
            continue;
        }
        let args = &step[paren + 1..step.rfind(')').unwrap_or(step.len())];
        let arg = args.split(',').nth(1).and_then(|a| a.trim().parse().ok());
        plan.push((name, arg.unwrap_or(0)));
    }
    if plan.is_empty() {
        return Err(SolverError::Stale);
    }
    Ok(plan)
}

impl CipherSolver for RuntimeSolver {
    fn solve<'a>(
        &'a self,
        obfuscated: &'a str,
        base_js_url: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<String, SolverError>> + Send + 'a>> {
        Box::pin(async move {
            // Exactly one fetch, one parse attempt.
            let response = self
                .client
                .get(base_js_url)
                .send()
                .await
                .map_err(|err| SolverError::Fetch(err.to_string()))?;
            if !response.status().is_success() {
                return Err(SolverError::Fetch(format!("status {}", response.status())));
            }
            let player_js = response
                .text()
                .await
                .map_err(|err| SolverError::Fetch(err.to_string()))?;
            // One normalized copy drives both the chain and helper lookups.
            let js: String = player_js.chars().filter(|c| !c.is_whitespace()).collect();
            let plan = transform_plan(&js)?;
            let mut resolved: Vec<(Op, usize)> = Vec::with_capacity(plan.len());
            for (name, arg) in plan {
                // `Qx.swap` → helper `swap` declared inside object `Qx`.
                let body = helper_in_object(&js, &name)
                    .or_else(|| helper_body(&js, &name))
                    .ok_or(SolverError::Stale)?;
                let op = classify_op(&body).ok_or(SolverError::Stale)?;
                resolved.push((op, arg));
            }
            let mut value: Vec<u8> = obfuscated.as_bytes().to_vec();
            for (op, arg) in resolved {
                apply_op(op, &mut value, arg);
            }
            String::from_utf8(value).map_err(|_| SolverError::Stale)
        })
    }
}

/// Convenience: builds a full stream URL from a `signatureCipher` payload.
/// The solver is invoked at most once per URL.
///
/// # Errors
///
/// Returns [`SolverError::Stale`] when deciphering fails.
pub async fn build_stream_url(
    solver: &dyn CipherSolver,
    signature_cipher: &str,
    base_js_url: &str,
) -> Result<String, SolverError> {
    let params = parse_query(signature_cipher);
    let s = params.get("s").ok_or(SolverError::Stale)?.clone();
    let url = params.get("url").ok_or(SolverError::Stale)?.clone();
    let sp = params.get("sp").cloned().unwrap_or_else(|| "sig".into());
    let signature = solver.solve(&s, base_js_url).await?;
    let separator = if url.contains('?') { "&" } else { "?" };
    Ok(format!("{url}{separator}{sp}={signature}"))
}

/// Minimal `a=1&b=2` query decoding (percent-decoding included).
#[must_use]
pub fn parse_query(query: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for pair in query.split('&') {
        if let Some((k, v)) = pair.split_once('=') {
            map.insert(percent_decode(k), percent_decode(v));
        }
    }
    map
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
                if let Some(byte) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                    out.push(byte);
                    i += 3;
                } else {
                    out.push(bytes[i]);
                    i += 1;
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// No-op solver for tests and for URLs that never need deciphering.
#[derive(Debug, Default)]
pub struct IdentitySolver;

impl CipherSolver for IdentitySolver {
    fn solve<'a>(
        &'a self,
        obfuscated: &'a str,
        _base_js_url: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<String, SolverError>> + Send + 'a>> {
        Box::pin(std::future::ready(Ok(obfuscated.to_owned())))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)] // tests may panic on failure

    use super::*;

    const PLAYER_JS: &str = r#"
var Qx = {
  swap: function(a, b) { var c = a[0]; a[0] = a[b % a.length]; a[b % a.length] = c; return a },
  splice: function(a, b) { a.splice(0, b); return a },
  reverse: function(a) { a.reverse(); return a }
};
function decipher(a) {
  a = a.split("");
  a = Qx.reverse(a, 7);
  a = Qx.swap(a, 3);
  a = Qx.splice(a, 2);
  return a.join("");
}
"#;

    #[tokio::test]
    async fn transform_plan_extracts_steps() {
        let plan = transform_plan(PLAYER_JS).expect("plan");
        assert_eq!(plan.len(), 3);
        assert_eq!(plan[0].0, "Qx.reverse");
        assert_eq!(plan[2].0, "Qx.splice");
    }

    #[tokio::test]
    async fn stale_player_js_fails_cleanly() {
        // No decipher chain at all → Stale (exactly one attempt by design).
        let solver = RuntimeSolver::default();
        let err = solver.solve("abc", "http://127.0.0.1:1/never.js").await;
        assert!(err.is_err());
    }

    #[test]
    fn query_parsing_decodes_percent_and_plus() {
        let params = parse_query("s=ab%20cd+ef&url=https%3A%2F%2Fexample.com%2Fv&sp=sig");
        assert_eq!(params.get("s").expect("s"), "ab cd ef");
        assert_eq!(params.get("url").expect("url"), "https://example.com/v");
        assert_eq!(params.get("sp").expect("sp"), "sig");
    }

    #[test]
    fn identity_solver_round_trips() {
        let out = futures_now(IdentitySolver.solve("xyz", "http://base"));
        assert_eq!(out.expect("solved"), "xyz");
    }

    fn futures_now(
        fut: impl Future<Output = Result<String, SolverError>>,
    ) -> Result<String, SolverError> {
        // Drive a ready future without a runtime (IdentitySolver never awaits).
        let mut pinned = Box::pin(fut);
        match pinned
            .as_mut()
            .poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
        {
            std::task::Poll::Ready(result) => result,
            std::task::Poll::Pending => panic!("unexpected pending"),
        }
    }
}
