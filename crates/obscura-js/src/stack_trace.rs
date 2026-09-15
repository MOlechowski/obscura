//! Page-visible `Error.stack` formatting that matches Chrome.
//!
//! Obscura implements much of the platform in JavaScript, so frames from its
//! own snapshot bootstrap and deno_core `ext:` modules leak into page stacks —
//! the masked `Function.prototype.toString`, for example, adds
//! `at Object.toString (<obscura:bootstrap>:164:26)`. Chrome shows only page
//! frames and native `(<anonymous>)` builtins.
//!
//! deno_core's own formatter also diverges from V8 on the frame body: for a
//! call whose receiver is a plain function it prints that function's name as a
//! type prefix (`at max.[Symbol.hasInstance]`) where V8 prints none
//! (`at [Symbol.hasInstance]`). CreepJS reads exactly that line to decide
//! whether `Function.prototype.toString` is proxied. So rather than reformat
//! each frame, this builds the stack from V8's own `CallSite.toString()`, which
//! is byte-for-byte what Chrome produces, after dropping the internal frames.

use std::fmt::Write as _;

use deno_core::v8;

/// Script names of Obscura-internal JavaScript that pages must not see.
const INTERNAL_SCRIPT_PREFIXES: [&str; 2] = ["<obscura:", "ext:"];
const GENERIC_ERROR_NAME: &str = "Error";

pub fn prepare_stack_trace_callback<'s>(
    scope: &mut v8::HandleScope<'s>,
    error: v8::Local<'s, v8::Value>,
    callsites: v8::Local<'s, v8::Array>,
) -> v8::Local<'s, v8::Value> {
    let mut result = error_header(scope, error);
    for index in 0..callsites.length() {
        let Some(callsite) = callsites.get_index(scope, index) else {
            continue;
        };
        let Ok(callsite) = v8::Local::<v8::Object>::try_from(callsite) else {
            continue;
        };
        if is_internal_frame(scope, callsite) {
            continue;
        }
        if let Some(frame) = formatted_frame(scope, callsite) {
            let _ = write!(result, "\n    at {frame}");
        }
    }
    v8::String::new(scope, &result).map_or_else(|| v8::undefined(scope).into(), Into::into)
}

/// The header line V8 puts above the frames: `Name: message`, or just one side
/// when the other is empty. Reads the properties directly, as deno_core does,
/// so a page's own `toString` override cannot run during formatting.
fn error_header<'s>(scope: &mut v8::HandleScope<'s>, error: v8::Local<'s, v8::Value>) -> String {
    let Ok(error) = v8::Local::<v8::Object>::try_from(error) else {
        return String::new();
    };
    let message = string_property(scope, error, "message").unwrap_or_default();
    let name = string_property(scope, error, "name")
        .unwrap_or_else(|| GENERIC_ERROR_NAME.to_string());
    match (name.is_empty(), message.is_empty()) {
        (false, false) => format!("{name}: {message}"),
        (false, true) => name,
        (true, false) => message,
        (true, true) => String::new(),
    }
}

fn is_internal_frame<'s>(
    scope: &mut v8::HandleScope<'s>,
    callsite: v8::Local<'s, v8::Object>,
) -> bool {
    call_string_method(scope, callsite, "getFileName").is_some_and(|file_name| {
        INTERNAL_SCRIPT_PREFIXES
            .iter()
            .any(|prefix| file_name.starts_with(prefix))
    })
}

/// The frame body exactly as V8 serializes it, with the `async ` prefix and the
/// `Promise.all` form V8 adds itself (`CallSite.toString()` omits both).
fn formatted_frame<'s>(
    scope: &mut v8::HandleScope<'s>,
    callsite: v8::Local<'s, v8::Object>,
) -> Option<String> {
    let mut frame = String::new();
    if call_bool_method(scope, callsite, "isAsync") {
        frame += "async ";
    }
    if call_bool_method(scope, callsite, "isPromiseAll") {
        let index = call_callsite_method(scope, callsite, "getPromiseIndex")
            .and_then(|value| value.to_integer(scope))
            .map_or(0, |value| value.value());
        let _ = write!(frame, "Promise.all (index {index})");
        return Some(frame);
    }
    frame += &call_string_method(scope, callsite, "toString")?;
    Some(frame)
}

fn string_property<'s>(
    scope: &mut v8::HandleScope<'s>,
    object: v8::Local<'s, v8::Object>,
    name: &str,
) -> Option<String> {
    let key = v8::String::new(scope, name)?;
    let value = object.get(scope, key.into())?;
    value
        .is_string()
        .then(|| value.to_rust_string_lossy(scope))
}

fn call_bool_method<'s>(
    scope: &mut v8::HandleScope<'s>,
    callsite: v8::Local<'s, v8::Object>,
    method_name: &str,
) -> bool {
    call_callsite_method(scope, callsite, method_name).is_some_and(|value| value.is_true())
}

fn call_string_method<'s>(
    scope: &mut v8::HandleScope<'s>,
    callsite: v8::Local<'s, v8::Object>,
    method_name: &str,
) -> Option<String> {
    let value = call_callsite_method(scope, callsite, method_name)?;
    value
        .is_string()
        .then(|| value.to_rust_string_lossy(scope))
}

fn call_callsite_method<'s>(
    scope: &mut v8::HandleScope<'s>,
    callsite: v8::Local<'s, v8::Object>,
    method_name: &str,
) -> Option<v8::Local<'s, v8::Value>> {
    let key = v8::String::new(scope, method_name)?;
    let method = callsite.get(scope, key.into())?;
    let method = v8::Local::<v8::Function>::try_from(method).ok()?;
    let scope = &mut v8::TryCatch::new(scope);
    method.call(scope, callsite.into(), &[])
}
