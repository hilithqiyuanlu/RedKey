use crate::models::AppAction;

/// Future serial devices only need to translate their input into AppAction.
/// Task state and command behavior remain owned by the desktop application.
#[allow(dead_code)]
pub trait HardwareInputAdapter: Send + Sync {
    fn adapter_name(&self) -> &'static str;
    fn decode_line(&self, line: &str) -> anyhow::Result<Option<AppAction>>;
}

#[allow(dead_code)]
pub struct JsonLinesHardwareAdapter;

impl HardwareInputAdapter for JsonLinesHardwareAdapter {
    fn adapter_name(&self) -> &'static str {
        "json-lines-v1"
    }

    fn decode_line(&self, line: &str) -> anyhow::Result<Option<AppAction>> {
        let value: serde_json::Value = serde_json::from_str(line)?;
        let action = match value.get("event").and_then(|event| event.as_str()) {
            Some("slot") => Some(AppAction::ActivateSlot {
                slot: value
                    .get("slot")
                    .and_then(|slot| slot.as_i64())
                    .ok_or_else(|| anyhow::anyhow!("slot 事件缺少 slot"))?,
            }),
            Some("progress") => Some(AppAction::AdjustProgress {
                delta: value
                    .get("delta")
                    .and_then(|delta| delta.as_i64())
                    .unwrap_or(0),
            }),
            Some("complete") => Some(AppAction::CompleteCurrent),
            Some("rework") => Some(AppAction::StartRework),
            Some(_) | None => None,
        };
        Ok(action)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_future_hardware_messages() {
        let adapter = JsonLinesHardwareAdapter;
        assert_eq!(
            adapter.decode_line(r#"{"event":"slot","slot":2}"#).unwrap(),
            Some(AppAction::ActivateSlot { slot: 2 })
        );
        assert_eq!(
            adapter
                .decode_line(r#"{"event":"progress","delta":5}"#)
                .unwrap(),
            Some(AppAction::AdjustProgress { delta: 5 })
        );
    }
}
