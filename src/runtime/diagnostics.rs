//! Structured diagnostics: one JSON object per line on stderr, never argument
//! values. stdout belongs to the MCP transport.
use crate::error::Result;
use serde_json::{Value, json};
use std::{
    io::Write,
    sync::Mutex,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

pub(super) fn emit(event: &str, fields: Value) {
    // A disconnected client's stderr must not panic the COM worker during
    // reference cleanup.
    let mut line = json!({
        "event": event,
        "timestamp_ms": SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    });
    line["details"] = fields;
    let _ = writeln!(std::io::stderr().lock(), "{line}");
}

/// Progress of one queued operation, shared between the caller and the STA.
#[derive(Debug)]
pub(crate) struct Trace {
    id: u64,
    action: &'static str,
    api_id: Option<String>,
    queued_at: Instant,
    phase: Mutex<(&'static str, Instant)>,
}
impl Trace {
    pub(super) fn new(id: u64, action: &'static str, api_id: Option<String>) -> Self {
        let now = Instant::now();
        Self {
            id,
            action,
            api_id,
            queued_at: now,
            phase: Mutex::new(("queued", now)),
        }
    }
    pub(super) fn snapshot(&self) -> Value {
        let (phase, started) = *self.phase.lock().expect("trace lock poisoned");
        json!({"operation_id":self.id,"action":self.action,"api_id":self.api_id,"phase":phase,"elapsed_ms":self.queued_at.elapsed().as_millis(),"phase_elapsed_ms":started.elapsed().as_millis()})
    }
    pub(super) fn event(&self, event: &str) {
        emit(event, self.snapshot());
    }
    pub(super) fn finish(&self, result: &Result<Value>) {
        let mut fields = self.snapshot();
        fields["ok"] = json!(result.is_ok());
        if let Err(error) = result {
            fields["error_code"] = json!(error.code);
            fields["hresult"] = json!(error.hresult);
        }
        emit("operation_finished", fields);
    }
    pub(crate) fn run_phase<T>(
        &self,
        phase: &'static str,
        run: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        *self.phase.lock().expect("trace lock poisoned") = (phase, Instant::now());
        self.event("phase_started");
        let started = Instant::now();
        let result = run();
        let mut fields = self.snapshot();
        fields["phase"] = json!(phase);
        fields["phase_elapsed_ms"] = json!(started.elapsed().as_millis());
        fields["duration_ms"] = json!(started.elapsed().as_millis());
        fields["ok"] = json!(result.is_ok());
        if let Err(error) = &result {
            fields["error_code"] = json!(error.code);
            fields["hresult"] = json!(error.hresult);
        }
        emit("phase_finished", fields);
        result.map_err(|mut error| {
            let mut details = match error.details.take() {
                Some(Value::Object(fields)) => Value::Object(fields),
                None => json!({}),
                Some(original) => json!({"original_details":original}),
            };
            // Keep the innermost failing phase when spans are nested.
            if details.get("runtime").is_none() {
                details["runtime"] = self.snapshot();
            }
            error.details = Some(details);
            error
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Error;

    #[test]
    fn nested_error_preserves_failing_phase_and_original_details() {
        let trace = Trace::new(42, "session_start", None);
        let error = trace
            .run_phase("executing", || {
                trace.run_phase("application_open", || {
                    let mut error = Error::new("fixture", "open failed");
                    error.details = Some(json!({"exception_scode":"0x12345678"}));
                    Err::<(), _>(error)
                })
            })
            .unwrap_err();
        let details = error.details.unwrap();
        assert_eq!(details["exception_scode"], "0x12345678");
        assert_eq!(details["runtime"]["phase"], "application_open");
        assert_eq!(details["runtime"]["operation_id"], 42);
        assert!(details["runtime"]["elapsed_ms"].is_u64());
    }
}
