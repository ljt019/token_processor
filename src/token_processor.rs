use aho_corasick::{AhoCorasick, AhoCorasickBuilder, MatchKind, PatternID};
use std::collections::HashMap;

/// A streaming-style callback sees one of these three events
pub enum TagStreamEvent {
    Open,         // saw `<tag>`
    Data(String), // saw some text inside the tag
    Close,        // saw `</tag>`
}

type StreamingHandler = Box<dyn FnMut(TagStreamEvent) -> Vec<String> + 'static>;

/// Represents an active tag being processed
struct Frame {
    name: String,
}

// Intermediate representation of events found in the buffer
#[derive(Debug)]
enum Event {
    Text(String),
    OpenTag(String),  // tag_name
    CloseTag(String), // tag_name
}

/// Processes a stream of string tokens, handling embedded tags like <tag>...</tag>.
pub struct TokenProcessor {
    handlers: HashMap<String, StreamingHandler>,
    stack: Vec<Frame>,
    matcher: AhoCorasick,
    buffer: String,
    patterns: Vec<(PatternID, String, bool)>, // (pattern_id, tag_name, is_opening)
}

impl TokenProcessor {
    pub fn new() -> Self {
        let matcher = AhoCorasickBuilder::new()
            .match_kind(MatchKind::LeftmostFirst)
            .build(Vec::<String>::new())
            .unwrap();

        TokenProcessor {
            handlers: HashMap::new(),
            stack: Vec::new(),
            matcher,
            buffer: String::new(),
            patterns: Vec::new(),
        }
    }

    /// Registers a streaming handler for a specific tag
    pub fn on_tag<F>(&mut self, tag: &str, handler: F)
    where
        F: FnMut(TagStreamEvent) -> Vec<String> + 'static,
    {
        self.handlers.insert(tag.to_string(), Box::new(handler));
        self.rebuild_matcher(tag);
    }

    /// Rebuilds the Aho-Corasick matcher when a new tag is added
    fn rebuild_matcher(&mut self, new_tag: &str) {
        let open_marker = format!("<{}>", new_tag);
        let close_marker = format!("</{}>", new_tag);

        let mut patterns_strs = Vec::new();
        patterns_strs.push(open_marker);
        patterns_strs.push(close_marker);

        // Add existing patterns
        for (_, tag, is_opening) in &self.patterns {
            let marker = if *is_opening {
                format!("<{}>", tag)
            } else {
                format!("</{}>", tag)
            };
            if !patterns_strs.contains(&marker) {
                patterns_strs.push(marker);
            }
        }

        // Rebuild the matcher
        self.matcher = AhoCorasickBuilder::new()
            .match_kind(MatchKind::LeftmostFirst)
            .build(&patterns_strs)
            .unwrap();

        // Reset patterns map
        self.patterns = Vec::new();
        for (idx, pattern) in patterns_strs.iter().enumerate() {
            let pattern_id = PatternID::new(idx).unwrap();
            if pattern.starts_with("</") {
                let tag_name = pattern[2..pattern.len() - 1].to_string();
                self.patterns.push((pattern_id, tag_name, false));
            } else {
                let tag_name = pattern[1..pattern.len() - 1].to_string();
                self.patterns.push((pattern_id, tag_name, true));
            }
        }
    }

    /// Processes the next incoming token string; returns any "outside-tag" text immediately
    pub fn feed<S: Into<String>>(&mut self, raw: S) -> Vec<String> {
        self.buffer.push_str(&raw.into());

        let mut events = Vec::new();
        let mut cursor = 0;
        let mut safe_cursor = 0; // Track the position up to which we can safely consume

        // --- Phase 1: Find matches and collect events ---
        for mat in self.matcher.find_iter(&self.buffer) {
            let start = mat.start();
            let end = mat.end();
            let pattern_id = mat.pattern();

            if start < cursor {
                continue; // Skip overlapping matches already covered
            }

            // Text before the match
            if start > cursor {
                let text_slice = &self.buffer[cursor..start];
                if !text_slice.is_empty() {
                    events.push(Event::Text(text_slice.to_string()));
                }
            }

            // The match itself (Open or Close tag)
            if let Some((_, tag_name, is_opening)) =
                self.patterns.iter().find(|(id, _, _)| *id == pattern_id)
            {
                if *is_opening {
                    events.push(Event::OpenTag(tag_name.clone()));
                } else {
                    events.push(Event::CloseTag(tag_name.clone()));
                }
            } else {
                eprintln!(
                    "Internal error: Pattern ID {:?} not found during event collection",
                    pattern_id
                );
                let marker_slice = &self.buffer[start..end];
                if !marker_slice.is_empty() {
                    events.push(Event::Text(marker_slice.to_string()));
                }
            }

            cursor = end; // Advance cursor past the processed match
            safe_cursor = end; // This tag is completely processed, so it's safe to consume up to here
        }

        // NEW BLOCK: inside a tag, stream out any leading text up to the next '<'
        if !self.stack.is_empty() {
            // keep emitting until the buffer either starts with '<' or is empty
            loop {
                if self.buffer.starts_with('<') {
                    break;
                }
                if self.buffer.is_empty() {
                    break;
                }
                // find the next '<' (or end of buffer)
                let pos = self.buffer.find('<').unwrap_or(self.buffer.len());
                let txt = self.buffer.drain(..pos).collect::<String>();
                if !txt.is_empty() {
                    events.push(Event::Text(txt));
                }
            }
        }

        // Fast-path for plain text outside any tag (and no '<' to avoid partial-tag leaks)
        if events.is_empty() && self.stack.is_empty() && !self.buffer.contains('<') {
            // Take ownership of the entire buffer and return it
            let all_text = std::mem::take(&mut self.buffer);
            if !all_text.is_empty() {
                return vec![all_text];
            }
            return Vec::new();
        }

        // Only trim the buffer up to safe_cursor, preserving any potential partial tags
        if safe_cursor > 0 {
            self.buffer = self.buffer[safe_cursor..].to_string();
        }

        // --- Phase 2: Process collected events and dispatch to handlers ---
        let mut result = Vec::new();
        for event in events {
            match event {
                Event::Text(text) => {
                    if let Some(frame) = self.stack.last() {
                        // Inside a tag -> stream Data event to the appropriate handler
                        let handler = self.handlers.get_mut(&frame.name).unwrap();
                        let output = handler(TagStreamEvent::Data(text));
                        // Collect any output tokens the handler produces (usually empty)
                        if !output.is_empty() {
                            if self.stack.len() > 1 {
                                // If we're in a nested tag, the output should be passed
                                // as Data to the parent tag's handler
                                let parent_idx = self.stack.len() - 2;
                                let parent_name = &self.stack[parent_idx].name;
                                let parent_handler = self.handlers.get_mut(parent_name).unwrap();
                                for token in output {
                                    parent_handler(TagStreamEvent::Data(token));
                                }
                            } else {
                                // If this is the outermost tag, output goes to result
                                result.extend(output);
                            }
                        }
                    } else {
                        // Outside any tag, this is plain output
                        result.push(text);
                    }
                }
                Event::OpenTag(tag_name) => {
                    // Create a new frame and push it to the stack
                    self.stack.push(Frame {
                        name: tag_name.clone(),
                    });

                    // Emit the Open event to the handler
                    let handler = self.handlers.get_mut(&tag_name).unwrap();
                    let output = handler(TagStreamEvent::Open);

                    // Handle any tokens the handler produces (usually empty)
                    if !output.is_empty() {
                        if self.stack.len() > 1 {
                            // If we're in a nested tag, output goes to parent
                            let parent_idx = self.stack.len() - 2;
                            let parent_name = &self.stack[parent_idx].name;
                            let parent_handler = self.handlers.get_mut(parent_name).unwrap();
                            for token in output {
                                parent_handler(TagStreamEvent::Data(token));
                            }
                        } else {
                            // If this is the outermost tag, output goes to result
                            result.extend(output);
                        }
                    }
                }
                Event::CloseTag(tag_name) => {
                    if let Some(current_frame) = self.stack.last() {
                        if current_frame.name == tag_name {
                            // Pop the frame since we're closing this tag
                            self.stack.pop();

                            // Emit the Close event to the handler
                            let handler = self.handlers.get_mut(&tag_name).unwrap();
                            let output = handler(TagStreamEvent::Close);

                            // Handle any tokens the handler produces (usually empty)
                            if !output.is_empty() {
                                if !self.stack.is_empty() {
                                    // If we're in a nested tag, output goes to parent
                                    let parent_name = &self.stack.last().unwrap().name;
                                    let parent_handler =
                                        self.handlers.get_mut(parent_name).unwrap();
                                    for token in output {
                                        parent_handler(TagStreamEvent::Data(token));
                                    }
                                } else {
                                    // If this was the outermost tag, output goes to result
                                    result.extend(output);
                                }
                            }
                        } else {
                            // Mismatched closing tag, treat it as plain text
                            let placeholder_text = format!("</{}>", tag_name);
                            if !self.stack.is_empty() {
                                let frame_name = &self.stack.last().unwrap().name;
                                let handler = self.handlers.get_mut(frame_name).unwrap();
                                handler(TagStreamEvent::Data(placeholder_text));
                            } else {
                                result.push(placeholder_text);
                            }
                        }
                    } else {
                        // Closing tag without an opening tag, treat as plain text
                        let placeholder_text = format!("</{}>", tag_name);
                        result.push(placeholder_text);
                    }
                }
            }
        }

        result
    }

    /// Processes any remaining buffered text after the stream ends.
    /// Also clears the stack and warns about unclosed tags.
    pub fn finish(&mut self) -> Vec<String> {
        let mut result = Vec::new();

        if !self.buffer.is_empty() {
            // Any remaining text in the buffer is treated as plain text
            if !self.stack.is_empty() {
                // If we're inside a tag, send as Data to the innermost handler
                let frame_name = &self.stack.last().unwrap().name;
                let handler = self.handlers.get_mut(frame_name).unwrap();
                let output = handler(TagStreamEvent::Data(self.buffer.clone()));
                if !output.is_empty() {
                    result.extend(output);
                }
            } else {
                // If we're not inside a tag, add to result
                result.push(std::mem::take(&mut self.buffer));
            }
        }

        // Warn about unclosed tags and clear the stack
        if !self.stack.is_empty() {
            eprintln!(
                "Warning: Stream ended with unclosed tags: {:?}",
                self.stack.iter().map(|f| &f.name).collect::<Vec<_>>()
            );

            // Emit Close events for all unclosed tags, from innermost to outermost
            let mut unclosed_tags = Vec::new();
            while let Some(frame) = self.stack.pop() {
                unclosed_tags.push(frame.name);
            }

            // Process unclosed tags in reverse order (innermost to outermost)
            for tag_name in unclosed_tags {
                let handler = self.handlers.get_mut(&tag_name).unwrap();
                let output = handler(TagStreamEvent::Close);
                if !output.is_empty() {
                    result.extend(output);
                }
            }
        }

        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    #[test]
    fn test_basic_streaming() {
        let mut processor = TokenProcessor::new();
        let collected = Rc::new(RefCell::new(Vec::new()));
        let collected_clone = collected.clone();

        processor.on_tag("test", move |evt| {
            match evt {
                TagStreamEvent::Open => {
                    collected_clone.borrow_mut().push("Open".to_string());
                }
                TagStreamEvent::Data(s) => {
                    collected_clone.borrow_mut().push(format!("Data: {}", s));
                }
                TagStreamEvent::Close => {
                    collected_clone.borrow_mut().push("Close".to_string());
                }
            }
            Vec::new()
        });

        let output = processor.feed("<test>hello world</test>");
        assert_eq!(output, Vec::<String>::new());
        assert_eq!(
            collected.borrow().join(", "),
            "Open, Data: hello world, Close"
        );
    }

    #[test]
    fn test_nested_tags() {
        let mut processor = TokenProcessor::new();
        let outer_events = Rc::new(RefCell::new(Vec::new()));
        let inner_events = Rc::new(RefCell::new(Vec::new()));

        let outer_clone = outer_events.clone();
        processor.on_tag("outer", move |evt| {
            match evt {
                TagStreamEvent::Open => outer_clone.borrow_mut().push("Open".to_string()),
                TagStreamEvent::Data(s) => outer_clone.borrow_mut().push(format!("Data: {}", s)),
                TagStreamEvent::Close => outer_clone.borrow_mut().push("Close".to_string()),
            }
            Vec::new()
        });

        let inner_clone = inner_events.clone();
        processor.on_tag("inner", move |evt| {
            match evt {
                TagStreamEvent::Open => inner_clone.borrow_mut().push("Open".to_string()),
                TagStreamEvent::Data(s) => inner_clone.borrow_mut().push(format!("Data: {}", s)),
                TagStreamEvent::Close => inner_clone.borrow_mut().push("Close".to_string()),
            }
            Vec::new()
        });

        processor.feed("<outer>begin <inner>nested</inner> end</outer>");

        assert_eq!(
            outer_events.borrow().join(", "),
            "Open, Data: begin , Data:  end, Close"
        );
        assert_eq!(
            inner_events.borrow().join(", "),
            "Open, Data: nested, Close"
        );
    }
}
