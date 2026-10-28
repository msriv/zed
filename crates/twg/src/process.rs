use anyhow::{Result, anyhow, bail, ensure};
use serde_json::Value;

#[cfg(test)]
fn decode(success: bool, stdout: &[u8]) -> Result<Value> {
    let envelope = decode_envelope(success, stdout)?;
    Ok(envelope["data"].clone())
}

pub(crate) fn decode_envelope(success: bool, stdout: &[u8]) -> Result<Value> {
    let envelope = decode_native(success, stdout)?;
    ensure!(
        envelope.get("data").is_some(),
        "TWG response is missing data"
    );
    Ok(envelope)
}

pub(crate) fn decode_native(success: bool, stdout: &[u8]) -> Result<Value> {
    let envelope: Value = serde_json::from_slice(stdout).map_err(|_| anyhow!("TWG returned an invalid JSON response; check the executable and authentication in TWG settings"))?;
    if envelope.get("error").is_some_and(|error| !error.is_null())
        || envelope.get("ok") == Some(&Value::Bool(false))
    {
        let error = envelope.get("error").unwrap_or(&Value::Null);
        let code = error["code"].as_str().unwrap_or("COMMAND_FAILED");
        let message = error["message"].as_str().unwrap_or("TWG command failed");
        bail!("TWG {}: {}", bounded(code, 80), bounded(message, 600));
    }
    ensure!(
        success,
        "TWG exited unsuccessfully; check authentication and permissions in TWG settings"
    );
    Ok(envelope)
}

fn bounded(text: &str, limit: usize) -> String {
    text.chars()
        .filter(|character| !character.is_control())
        .take(limit)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_bitbucket_payloads_keep_paging_and_require_success() {
        let query = br#"{"values":[],"page":1,"hasNextPage":true}"#;
        assert_eq!(
            decode_native(true, query).expect("native")["hasNextPage"],
            true
        );
        assert!(decode_envelope(true, query).is_err());
        assert!(decode_native(false, query).is_err());
        assert!(
            decode_native(
                true,
                br#"{"ok":false,"error":{"code":"DENIED","message":"Not permitted"}}"#
            )
            .is_err()
        );
        assert_eq!(
            decode_native(true, b"[]").expect("native"),
            serde_json::json!([])
        );
    }

    #[test]
    fn unwraps_success_and_respects_exit_status() {
        let output = br#"{"apiVersion":"v2","data":{"issues":[]}}"#;
        assert_eq!(
            decode(true, output).expect("response"),
            serde_json::json!({"issues":[]})
        );
        assert!(decode(false, output).is_err());
    }

    #[test]
    fn structured_errors_do_not_dump_payloads() {
        let error = decode(false, br#"{"ok":false,"error":{"code":"AUTH_REQUIRED","message":"Sign in required"},"private":"secret ticket text"}"#).expect_err("error").to_string();
        assert!(error.contains("AUTH_REQUIRED"));
        assert!(!error.contains("secret ticket text"));
        assert!(decode(true, br#"{"ok":false,"error":{"message":"denied"}}"#).is_err());
        assert!(decode(true, b"not JSON private content").is_err());
        assert!(decode(true, b"{}").is_err());
    }
}
