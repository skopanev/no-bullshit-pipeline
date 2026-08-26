use nbp_lib::config::TranscriptionConfig;

#[test]
fn keep_model_ready_defaults_off() {
    assert!(!TranscriptionConfig::default().keep_model_ready);
}

#[test]
fn missing_keep_model_ready_deserializes_off() {
    let mut value = serde_json::to_value(TranscriptionConfig::default()).unwrap();
    value.as_object_mut().unwrap().remove("keep_model_ready");
    let restored: TranscriptionConfig = serde_json::from_value(value).unwrap();
    assert!(!restored.keep_model_ready);
}
