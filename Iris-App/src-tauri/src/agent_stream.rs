use serde_json::Value;

#[derive(Default)]
struct AnswerFilter {
    pending: String,
    closing_tag: Option<&'static str>,
}

impl AnswerFilter {
    fn push(&mut self, text: &str) -> String {
        self.pending.push_str(text);
        let mut visible = String::new();
        let tags = [("<think>", "</think>"), ("<analysis>", "</analysis>"), ("<reasoning>", "</reasoning>")];
        loop {
            if let Some(closing) = self.closing_tag {
                if let Some(position) = self.pending.find(closing) {
                    self.pending.drain(..position + closing.len());
                    self.closing_tag = None;
                    continue;
                }
                let keep = (1..closing.len()).rev().find(|length| self.pending.ends_with(&closing[..*length])).unwrap_or(0);
                self.pending.drain(..self.pending.len() - keep);
                break;
            }
            let Some(position) = self.pending.find('<') else {
                visible.push_str(&self.pending);
                self.pending.clear();
                break;
            };
            visible.push_str(&self.pending[..position]);
            self.pending.drain(..position);
            if let Some((opening, closing)) = tags.iter().find(|(opening, _)| self.pending.starts_with(opening)) {
                self.pending.drain(..opening.len());
                self.closing_tag = Some(closing);
            } else if let Some((_, closing)) = tags.iter().find(|(_, closing)| self.pending.starts_with(closing)) {
                self.pending.drain(..closing.len());
            } else if tags.iter().any(|(opening, closing)| opening.starts_with(&self.pending) || closing.starts_with(&self.pending)) {
                break;
            } else {
                visible.push('<');
                self.pending.drain(..1);
            }
        }
        visible
    }
}

pub(super) fn visible_answer(text: &str) -> String {
    let mut filter = AnswerFilter::default();
    let mut visible = filter.push(text);
    if filter.closing_tag.is_none() && !["<think>","<analysis>","<reasoning>"].iter().any(|tag|tag.starts_with(&filter.pending)) {
        visible.push_str(&filter.pending);
    }
    visible
}

#[derive(Default)]
pub(super) struct ReplyStream {
    bytes: Vec<u8>,
    filter: AnswerFilter,
    pub content: String,
    pub done: bool,
    pub limit_reached: bool,
    emitted: usize,
}

fn string_prefix(raw: &str) -> Option<(String, usize, bool)> {
    if !raw.starts_with('"') { return None; }
    let mut escaped = false;
    let mut valid_end = 1;
    for (offset, character) in raw.char_indices().skip(1) {
        if character == '"' && !escaped {
            let value = serde_json::from_str::<String>(&raw[..offset + 1]).ok()?;
            return Some((value, offset + 1, true));
        }
        if character == '\\' && !escaped { escaped = true; }
        else {
            escaped = false;
            valid_end = offset + character.len_utf8();
        }
    }
    let mut end = valid_end;
    loop {
        if let Ok(value) = serde_json::from_str::<String>(&format!("{}\"", &raw[..end])) {
            return Some((value, raw.len(), false));
        }
        if end <= 1 { return Some((String::new(), raw.len(), false)); }
        end = raw[..end].char_indices().next_back()?.0;
    }
}

fn answer_prefix(raw: &str) -> Option<String> {
    let mut remaining = raw.trim_start().strip_prefix('{')?.trim_start();
    let mut is_answer = false;
    loop {
        let (key, consumed, complete) = string_prefix(remaining)?;
        if !complete { return None; }
        remaining = remaining[consumed..].trim_start().strip_prefix(':')?.trim_start();
        if key == "message" && is_answer {
            return string_prefix(remaining).map(|(value, _, _)| value);
        }
        let (value, consumed, complete) = string_prefix(remaining)?;
        if !complete { return None; }
        if key == "action" { is_answer = value == "finish"; }
        remaining = remaining[consumed..].trim_start().strip_prefix(',')?.trim_start();
    }
}

impl ReplyStream {
    pub fn push(&mut self, bytes: &[u8]) -> Result<String, String> {
        self.bytes.extend_from_slice(bytes);
        while let Some(end) = self.bytes.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = self.bytes.drain(..=end).collect();
            self.frame(&line)?;
        }
        Ok(self.delta())
    }

    fn frame(&mut self, bytes: &[u8]) -> Result<(), String> {
        if bytes.iter().all(u8::is_ascii_whitespace) { return Ok(()); }
        let frame: Value = serde_json::from_slice(bytes).map_err(|error| format!("Invalid Ollama stream: {error}"))?;
        if let Some(error) = frame.get("error").and_then(Value::as_str) { return Err(error.to_string()); }
        if self.done { return Err("Received content after the stream completed".into()); }
        if let Some(text) = frame.pointer("/message/content").and_then(Value::as_str) {
            if self.content.trim_start().starts_with('{') || (self.content.trim().is_empty() && text.trim_start().starts_with('{')) {
                self.content.push_str(text);
            } else {
                self.content.push_str(&self.filter.push(text));
            }
        }
        if frame.get("done").and_then(Value::as_bool) == Some(true) {
            self.done = true;
            self.limit_reached = frame.get("done_reason").and_then(Value::as_str) == Some("length");
        }
        Ok(())
    }

    fn delta(&mut self) -> String {
        let trimmed = self.content.trim_start();
        if trimmed.is_empty() { return String::new(); }
        if trimmed.starts_with('{') {
            let Some(answer) = answer_prefix(trimmed) else { return String::new(); };
            let mut filter = AnswerFilter::default();
            let answer = filter.push(&answer);
            if answer.len() < self.emitted { return String::new(); }
            let delta = answer[self.emitted..].to_string();
            self.emitted = answer.len();
            return delta;
        }
        if trimmed.starts_with('`') {
            if "```".starts_with(trimmed) { return String::new(); }
            if trimmed.starts_with("```") {
                let Some((_, body)) = trimmed.split_once('\n') else { return String::new(); };
                if body.trim_start().is_empty() || body.trim_start().starts_with('{') { return String::new(); }
            }
        }
        let mut visible_end = self.content.len();
        for (offset, _) in self.content.match_indices('{') {
            let tail = self.content[offset + 1..].trim_start();
            if tail.starts_with("\"action\"") || "\"action\"".starts_with(tail) {
                visible_end = offset;
                break;
            }
        }
        if visible_end < self.emitted { return String::new(); }
        let delta = self.content[self.emitted..visible_end].to_string();
        self.emitted = visible_end;
        delta
    }

    pub fn finish(&mut self) -> Result<String, String> {
        if !self.bytes.is_empty() {
            let tail = std::mem::take(&mut self.bytes);
            self.frame(&tail)?;
        }
        if !self.done { return Err("Ollama disconnected before completing the response".into()); }
        if self.filter.closing_tag.is_none() && !self.filter.pending.is_empty() {
            if !["<think>", "<analysis>", "<reasoning>"].iter().any(|tag| tag.starts_with(&self.filter.pending)) {
                self.content.push_str(&std::mem::take(&mut self.filter.pending));
            }
        }
        Ok(self.delta())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(content: &str) -> Vec<u8> {
        format!("{}\n", serde_json::json!({"message":{"content":content},"done":false})).into_bytes()
    }

    #[test]
    fn reply_stream_emits_text_before_generation_completes() {
        let mut stream = ReplyStream::default();
        assert_eq!(stream.push(&frame("I'm Iris. ")).unwrap(), "I'm Iris. ");
        assert!(!stream.done);
        assert_eq!(stream.push(&frame("I can help you explore ideas.")).unwrap(), "I can help you explore ideas.");
        stream.push(b"{\"done\":true}\n").unwrap();
        assert_eq!(stream.finish().unwrap(), "");
        assert_eq!(stream.content, "I'm Iris. I can help you explore ideas.");
    }

    #[test]
    fn reply_stream_preserves_split_unicode_and_hides_all_reasoning() {
        let content = format!("<think>private</think>Hello {}", '\u{1f30d}');
        let bytes = frame(&content);
        let mut stream = ReplyStream::default();
        let mut visible = String::new();
        for byte in bytes { visible.push_str(&stream.push(&[byte]).unwrap()); }
        visible.push_str(&stream.push(b"{\"message\":{\"thinking\":\"private native reasoning\",\"content\":\"\"},\"done\":true}\n").unwrap());
        visible.push_str(&stream.finish().unwrap());
        assert_eq!(visible, format!("Hello {}", '\u{1f30d}'));
    }

    #[test]
    fn reply_stream_hides_fragmented_inline_reasoning_and_control_actions() {
        let mut stream = ReplyStream::default();
        for text in ["<th", "ink>", "private", "</th", "ink>", "{\"action\":", "\"tool\",\"tool\":\"read\",\"arguments\":{}}"] {
            assert!(stream.push(&frame(text)).unwrap().is_empty());
        }
        stream.push(b"{\"done\":true}").unwrap();
        assert!(stream.finish().unwrap().is_empty());
        assert_eq!(stream.content, r#"{"action":"tool","tool":"read","arguments":{}}"#);
    }

    #[test]
    fn reply_stream_does_not_hide_ordinary_code() {
        let mut stream = ReplyStream::default();
        assert_eq!(stream.push(&frame("```rust\nlet value = 3;")).unwrap(), "```rust\nlet value = 3;");
        assert_eq!(stream.push(&frame("\n``` ")).unwrap(), "\n``` ");
    }

    #[test]
    fn reply_stream_decodes_answer_envelopes_before_the_json_is_finished() {
        let mut stream=ReplyStream::default();
        assert_eq!(stream.push(&frame("{\"action\":\"finish\",\"message\":\"Hello")).unwrap(),"Hello");
        assert_eq!(stream.push(&frame("\\nworld \\uD83C")).unwrap(),"\nworld ");
        assert_eq!(stream.push(&frame("\\uDF0D\"}")).unwrap(),"\u{1f30d}");
        stream.push(b"{\"done\":true}\n").unwrap();
        assert!(stream.finish().unwrap().is_empty());
    }

    #[test]
    fn reply_stream_does_not_modify_literal_tool_arguments() {
        let mut stream=ReplyStream::default();
        let payload=r#"{"action":"tool","tool":"write","arguments":{"text":"<think>literal markup</think>"}}"#;
        assert!(stream.push(&frame(payload)).unwrap().is_empty());
        assert_eq!(stream.content,payload);
        assert_eq!(visible_answer("<think>private</think>Visible"),"Visible");
        assert_eq!(visible_answer("A < B"),"A < B");
    }

    #[test]
    fn reply_stream_hides_control_json_after_a_prose_prefix() {
        let mut stream = ReplyStream::default();
        assert_eq!(stream.push(&frame("Let me check.\n")).unwrap(), "Let me check.\n");
        for text in ["{", "\"ac", "tion\":\"tool\",\"tool\":\"wikipedia_search\",\"arguments\":{}}", "\nThis will search."] {
            assert!(stream.push(&frame(text)).unwrap().is_empty());
        }
        stream.push(b"{\"done\":true}\n").unwrap();
        assert!(stream.finish().unwrap().is_empty());
    }

    #[test]
    fn reply_stream_rejects_errors_and_incomplete_transport() {
        let mut stream = ReplyStream::default();
        stream.push(&frame("Partial")).unwrap();
        assert!(stream.finish().is_err());
        assert!(stream.push(b"{\"error\":\"prediction aborted\"}\n").is_err());
        let mut limited = ReplyStream::default();
        limited.push(b"{\"done\":true,\"done_reason\":\"length\"}\n").unwrap();
        assert!(limited.limit_reached);
    }
}