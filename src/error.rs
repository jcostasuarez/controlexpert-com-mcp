use serde::Serialize;

#[derive(Debug, Clone, Serialize, thiserror::Error)]
#[error("{message}")]
pub struct Error {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hresult: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

impl Error {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            hresult: None,
            details: None,
        }
    }
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new("invalid_arguments", message)
    }
    pub fn unsupported(message: impl Into<String>) -> Self {
        Self::new("unsupported_operation", message)
    }

    /// Add recovery guidance without changing the original COM error or
    /// claiming to know why the external server failed to start.
    #[cfg(any(windows, test))]
    pub(crate) fn with_activation_guidance(mut self) -> Self {
        let server_failed = self.code == "com_error"
            && (self.hresult.as_deref() == Some("0x80080005")
                || self
                    .details
                    .as_ref()
                    .and_then(|d| d["exception_scode"].as_str())
                    == Some("0x80080005"));
        if !server_failed {
            return self;
        }
        let mut details = match self.details.take() {
            Some(serde_json::Value::Object(fields)) => serde_json::Value::Object(fields),
            None => serde_json::json!({}),
            Some(original) => serde_json::json!({"original_details": original}),
        };
        details["activation_diagnostic"] = serde_json::json!({
            "code": "com_server_start_failed",
            "summary": "The external COM server could not start or register in time. The specific cause is not confirmed.",
            "root_cause_confirmed": false,
            "automatic_retry": false,
            "probe_scope": "ce_probe checks the installation and version of pserver.dll; compatible=true does not guarantee that Control Expert can start.",
            "recovery_steps": [
                "Query ce_runtime_status and wait until active_operation is null; do not start another open while an operation is active.",
                "Open Control Expert Classic manually and wait for its initialization to finish. Review any errors or dialogs it shows; complete the DTM catalog update if it asks for one.",
                "After the manual startup completes, retry the requested open operation once and check the response. A test with Classic already open does not confirm a cold start.",
                "If the manual start works but COM fails, compare the same MCP executable in the usual environment and outside a sandbox, with authorization. A failure in an isolated environment does not prove a damaged installation.",
                "If it persists, review the DistributedCOM events and the local Control Expert logs. Do not change DCOM permissions or terminate processes as automatic recovery."
            ],
            "vendor_reference": "https://www.se.com/au/en/faqs/FAQ000234251/",
            "reference_scope": "Schneider documents a possible 15.1 cause related to DTM catalog initialization; it does not prove that it caused this failure."
        });
        self.details = Some(details);
        self
    }
}
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(windows)]
impl From<windows::core::Error> for Error {
    fn from(e: windows::core::Error) -> Self {
        Self {
            code: "com_error".into(),
            message: e.message(),
            hresult: Some(format!("0x{:08X}", e.code().0 as u32)),
            details: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn activation_guidance_recognizes_nested_server_failure_and_preserves_evidence() {
        let mut error = Error::new("com_error", "Exception occurred.");
        error.hresult = Some("0x80020009".into());
        let original = json!({
            "exception_scode": "0x80080005", "source": "PSBroker.PServerBroker.1",
            "runtime": {"phase": "application_create", "elapsed_ms": 93031}
        });
        error.details = Some(original.clone());
        let error = error.with_activation_guidance();
        assert_eq!(error.code, "com_error");
        assert_eq!(error.hresult.as_deref(), Some("0x80020009"));
        assert_eq!(error.message, "Exception occurred.");
        let details = error.details.unwrap();
        for (key, value) in original.as_object().unwrap() {
            assert_eq!(&details[key], value);
        }
        assert_eq!(details["activation_diagnostic"]["automatic_retry"], false);
        assert_eq!(
            details["activation_diagnostic"]["root_cause_confirmed"],
            false
        );
    }

    #[test]
    fn activation_guidance_handles_direct_hresult_without_details() {
        let mut error = Error::new("com_error", "Server execution failed");
        error.hresult = Some("0x80080005".into());
        let error = error.with_activation_guidance();
        assert_eq!(
            error.details.unwrap()["activation_diagnostic"]["code"],
            "com_server_start_failed"
        );
    }

    #[test]
    fn unrelated_errors_do_not_get_activation_guidance() {
        for code in ["com_error", "invalid_arguments"] {
            let mut error = Error::new(code, "Unrelated error");
            error.hresult = Some("0x80020009".into());
            error.details = Some(json!({"exception_scode": "0x80020005"}));
            let before = serde_json::to_value(&error).unwrap();
            assert_eq!(
                serde_json::to_value(error.with_activation_guidance()).unwrap(),
                before
            );
        }
    }
}
