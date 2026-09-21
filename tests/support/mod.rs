pub fn authorization() -> serde_json::Value {
    serde_json::json!({"availability":"available","latest_user_message":{
        "id":"fixture-user","role":"user","source":"test","text":"Run the requested development task"},
        "relevant_prior_messages":[]})
}
