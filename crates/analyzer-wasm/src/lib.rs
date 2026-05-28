use analyzer_core::{analyze, AnalyzeInput};
use wasm_bindgen::prelude::*;

#[wasm_bindgen(start)]
pub fn start() {
    #[cfg(feature = "console_error_panic_hook")]
    console_error_panic_hook::set_once();
}

#[wasm_bindgen]
pub fn analyze_json(input: JsValue) -> Result<JsValue, JsValue> {
    let parsed: AnalyzeInput = serde_wasm_bindgen::from_value(input)
        .map_err(|e| JsValue::from_str(&format!("input: {e}")))?;
    let report = analyze(&parsed);
    serde_wasm_bindgen::to_value(&report).map_err(|e| JsValue::from_str(&format!("serialize: {e}")))
}

#[wasm_bindgen]
pub fn analyze_sql(sql: &str) -> Result<JsValue, JsValue> {
    let report = analyze(&AnalyzeInput { sql: Some(sql.to_string()), ..Default::default() });
    serde_wasm_bindgen::to_value(&report).map_err(|e| JsValue::from_str(&format!("serialize: {e}")))
}

#[wasm_bindgen]
pub fn analyze_plan_xml(xml: &str) -> Result<JsValue, JsValue> {
    let report = analyze(&AnalyzeInput { plan_xml: Some(xml.to_string()), ..Default::default() });
    serde_wasm_bindgen::to_value(&report).map_err(|e| JsValue::from_str(&format!("serialize: {e}")))
}
