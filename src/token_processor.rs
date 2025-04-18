use aho_corasick::{AhoCorasick, AhoCorasickBuilder, MatchKind, PatternID};
use std::collections::HashMap;
use std::error::Error;
use std::fmt;

/// A streaming-style callback sees one of these three events
pub enum TagStreamEvent {
    Open,         // saw `<tag>`
    Data(String), // saw some text inside the tag
    Close,        // saw `</tag>`
}

/// Result from a tag handler that provides more control over output
pub enum HandlerResult {
    /// Emit text as output
    Emit(String),
    /// Emit multiple strings as output
    #[allow(dead_code)]
    EmitMultiple(Vec<String>),
    /// Don't emit anything
    Drop,
}

impl HandlerResult {
    fn to_vec(self) -> Vec<String> {
        match self {
            HandlerResult::Emit(s) => vec![s],
            HandlerResult::EmitMultiple(v) => v,
            HandlerResult::Drop => Vec::new(),
        }
    }
}

/// Error types that can occur during token processing
#[derive(Debug)]
pub enum TokenProcessorError {
    /// A pattern ID was found that wasn't in the patterns map
    UnknownPatternId(PatternID),
    /// Error in the Aho-Corasick matcher
    MatcherError(String),
    /// Unclosed tags at the end of the stream
    UnclosedTags(Vec<String>),
}

impl fmt::Display for TokenProcessorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TokenProcessorError::UnknownPatternId(id) => {
                write!(f, "Unknown pattern ID: {:?}", id)
            }
            TokenProcessorError::MatcherError(msg) => {
                write!(f, "Matcher error: {}", msg)
            }
            TokenProcessorError::UnclosedTags(tags) => {
                write!(f, "Stream ended with unclosed tags: {:?}", tags)
            }
        }
    }
}

impl Error for TokenProcessorError {}

type StreamingHandler = Box<dyn FnMut(TagStreamEvent) -> HandlerResult + 'static>;
type PlainTextHandler = Box<dyn FnMut(&str) -> HandlerResult + 'static>;

/// Represents an active tag being processed
#[derive(Clone)]
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
///
/// # Notes on Tag Processing
///
/// ## Handler Output Constraints
/// When handlers emit strings using `HandlerResult::Emit` or `HandlerResult::EmitMultiple`,
/// be aware that those strings will be processed as normal input. This means any tag markers
/// in the emitted strings (`<tag>`, `</tag>`) will be interpreted as actual tags if those
/// tags have been registered. If you want to emit literal tag markers, you'll need to
/// escape them in your handler.
///
/// ## Overlapping Tag Names
/// When registering tags with overlapping names (e.g., "foo" and "foobar"), be aware that
/// the pattern matching is based on a leftmost-first approach. The actual tag matched will
/// depend on the order of registration and internal pattern organization. If you need
/// consistent longest-match-first behavior, consider sorting your tags by descending length
/// before registration.
///
/// ## Nested Same-Name Tags
/// The processor supports nested tags with the same name (e.g., `<tag>...<tag>...</tag>...</tag>`).
/// Each opening tag pushes a new frame onto the stack regardless of whether the same tag name
/// is already on the stack. If you don't want to allow nested same-name tags, you'll need to
/// implement that logic in your tag handler.
///
/// ## Multithreading
/// The current implementation does not implement `Send` for handlers, which means the processor
/// cannot be safely sent across thread boundaries. If you need to use it with async/multi-threaded
/// code, consider adding a `+ Send` bound to the handler trait objects.
pub struct TokenProcessor {
    handlers: HashMap<String, StreamingHandler>,
    plain_text_handler: Option<PlainTextHandler>,
    stack: Vec<Frame>,
    matcher: AhoCorasick,
    buffer: String,
    patterns: Vec<(PatternID, String, bool)>, // (pattern_id, tag_name, is_opening)
    pattern_strings: Vec<String>,             // Cached pattern strings
}

impl TokenProcessor {
    pub fn new() -> Self {
        let matcher = AhoCorasickBuilder::new()
            .match_kind(MatchKind::LeftmostFirst)
            .build(Vec::<String>::new())
            .unwrap();

        TokenProcessor {
            handlers: HashMap::new(),
            plain_text_handler: None,
            stack: Vec::new(),
            matcher,
            buffer: String::new(),
            patterns: Vec::new(),
            pattern_strings: Vec::new(),
        }
    }

    /// Creates a new TokenProcessor with a predefined set of tags.
    /// This avoids multiple rebuilds of the Aho-Corasick matcher when registering multiple tags.
    pub fn new_with_tags<S, F>(tags_and_handlers: Vec<(S, F)>) -> Self
    where
        S: Into<String>,
        F: FnMut(TagStreamEvent) -> HandlerResult + 'static,
    {
        let mut processor = Self::new();

        // Register all tags first without rebuilding the matcher each time
        let mut handlers = HashMap::new();
        let mut tag_names = Vec::new();

        for (tag, handler) in tags_and_handlers {
            let tag_string = tag.into();
            handlers.insert(tag_string.clone(), Box::new(handler) as StreamingHandler);
            tag_names.push(tag_string);
        }

        processor.handlers = handlers;

        // Now build the matcher once for all tags
        processor.rebuild_matcher_bulk(&tag_names);

        processor
    }

    /// Registers multiple tag handlers at once, rebuilding the matcher only once.
    /// This is more efficient than calling on_tag() multiple times.
    pub fn register_tags<S, F>(&mut self, tags_and_handlers: Vec<(S, F)>)
    where
        S: Into<String>,
        F: FnMut(TagStreamEvent) -> HandlerResult + 'static,
    {
        let mut tag_names = Vec::new();

        for (tag, handler) in tags_and_handlers {
            let tag_string = tag.into();
            self.handlers
                .insert(tag_string.clone(), Box::new(handler) as StreamingHandler);
            tag_names.push(tag_string);
        }

        // Rebuild matcher once for all new tags
        self.rebuild_matcher_bulk(&tag_names);
    }

    /// Registers a handler for plain text outside of any tags
    ///
    /// # Note
    /// The plain text handler will be called for text outside of any tags.
    /// It will be called either through the fast-path (when there are no tags in the input)
    /// or through the general event processing flow, but never both for the same text.
    pub fn on_plain_text<F>(&mut self, handler: F)
    where
        F: FnMut(&str) -> HandlerResult + 'static,
    {
        self.plain_text_handler = Some(Box::new(handler));
    }

    /// Registers a streaming handler for a specific tag
    ///
    /// # Note
    /// When registering multiple tags, be aware of potential overlapping tag names.
    /// For example, if you register both "foo" and "foobar", the matcher might prioritize
    /// one over the other based on internal pattern organization.
    pub fn on_tag<F>(&mut self, tag: &str, handler: F)
    where
        F: FnMut(TagStreamEvent) -> HandlerResult + 'static,
    {
        self.handlers.insert(tag.to_string(), Box::new(handler));
        self.rebuild_matcher(tag);
    }

    /// Rebuilds the Aho-Corasick matcher when a new tag is added
    fn rebuild_matcher(&mut self, new_tag: &str) {
        let open_marker = format!("<{}>", new_tag);
        let close_marker = format!("</{}>", new_tag);

        // Check if the patterns already exist before adding
        if !self.pattern_strings.contains(&open_marker) {
            self.pattern_strings.push(open_marker);
        }
        if !self.pattern_strings.contains(&close_marker) {
            self.pattern_strings.push(close_marker);
        }

        // Rebuild the matcher with cached pattern strings
        self.matcher = AhoCorasickBuilder::new()
            .match_kind(MatchKind::LeftmostFirst)
            .build(&self.pattern_strings)
            .expect("Failed to build Aho-Corasick matcher");

        // Reset patterns map
        self.patterns = Vec::new();
        for (idx, pattern) in self.pattern_strings.iter().enumerate() {
            let pattern_id = PatternID::new(idx).expect("Invalid pattern ID");
            if pattern.starts_with("</") {
                let tag_name = pattern[2..pattern.len() - 1].to_string();
                self.patterns.push((pattern_id, tag_name, false));
            } else {
                let tag_name = pattern[1..pattern.len() - 1].to_string();
                self.patterns.push((pattern_id, tag_name, true));
            }
        }
    }

    /// Rebuilds the matcher for multiple tags at once
    fn rebuild_matcher_bulk(&mut self, new_tags: &[String]) {
        // Sort tags by length in descending order to prioritize longer matches
        // when tags overlap (e.g. "foo" and "foobar")
        let mut sorted_tags = new_tags.to_vec();
        sorted_tags.sort_by(|a, b| b.len().cmp(&a.len()));

        for tag in sorted_tags {
            let open_marker = format!("<{}>", tag);
            let close_marker = format!("</{}>", tag);

            // Check if the patterns already exist before adding
            if !self.pattern_strings.contains(&open_marker) {
                self.pattern_strings.push(open_marker);
            }
            if !self.pattern_strings.contains(&close_marker) {
                self.pattern_strings.push(close_marker);
            }
        }

        // Rebuild the matcher with all pattern strings
        self.matcher = AhoCorasickBuilder::new()
            .match_kind(MatchKind::LeftmostFirst)
            .build(&self.pattern_strings)
            .expect("Failed to build Aho-Corasick matcher");

        // Reset patterns map
        self.patterns = Vec::new();
        for (idx, pattern) in self.pattern_strings.iter().enumerate() {
            let pattern_id = PatternID::new(idx).expect("Invalid pattern ID");
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
    pub fn feed<S: Into<String>>(&mut self, raw: S) -> Result<Vec<String>, TokenProcessorError> {
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
                // Return an error instead of using eprintln
                return Err(TokenProcessorError::UnknownPatternId(pattern_id));
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
            // Take ownership of the entire buffer
            let text = std::mem::take(&mut self.buffer);
            if text.is_empty() {
                return Ok(Vec::new());
            }

            // If we have a plain text handler, use it
            if let Some(handler) = &mut self.plain_text_handler {
                return Ok(handler(&text).to_vec());
            }

            // Otherwise just return the text
            return Ok(vec![text]);
        }

        // Trim the buffer using drain to avoid reallocation
        if safe_cursor > 0 {
            let _ = self.buffer.drain(..safe_cursor);
        }

        // --- Phase 2: Process collected events and dispatch to handlers ---
        let mut result = Vec::new();
        for event in events {
            match event {
                Event::Text(text) => {
                    if let Some(frame) = self.stack.last() {
                        // Inside a tag -> stream Data event to the appropriate handler
                        let handler = self.handlers.get_mut(&frame.name).unwrap();
                        let output = handler(TagStreamEvent::Data(text)).to_vec();
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
                    } else if let Some(handler) = &mut self.plain_text_handler {
                        // Not in any tag and we have a plain text handler
                        result.extend(handler(&text).to_vec());
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
                    let output = handler(TagStreamEvent::Open).to_vec();

                    // Handle any tokens the handler produces (usually empty)
                    if !output.is_empty() {
                        if self.stack.len() > 1 {
                            // If we're in a nested tag, output goes to parent as Data
                            let parent_idx = self.stack.len() - 2;
                            let parent_name = &self.stack[parent_idx].name;
                            let parent_handler = self.handlers.get_mut(parent_name).unwrap();
                            for token in output {
                                parent_handler(TagStreamEvent::Data(token));
                            }
                        } else {
                            // If this is the outermost tag, re-inject the output
                            for token in output {
                                let sub_result = self.feed(token)?;
                                result.extend(sub_result);
                            }
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
                            let output = handler(TagStreamEvent::Close).to_vec();

                            // Handle any tokens the handler produces (usually empty)
                            if !output.is_empty() {
                                if !self.stack.is_empty() {
                                    // If we're still in a nested tag, output goes to parent as Data
                                    let parent_name = &self.stack.last().unwrap().name;
                                    let parent_handler =
                                        self.handlers.get_mut(parent_name).unwrap();
                                    for token in output {
                                        parent_handler(TagStreamEvent::Data(token));
                                    }
                                } else {
                                    // If this was the outermost tag, re-inject the output
                                    for token in output {
                                        let sub_result = self.feed(token)?;
                                        result.extend(sub_result);
                                    }
                                }
                            }
                        } else {
                            // Mismatched closing tag, treat it as plain text
                            let placeholder_text = format!("</{}>", tag_name);
                            if !self.stack.is_empty() {
                                let frame_name = &self.stack.last().unwrap().name;
                                let handler = self.handlers.get_mut(frame_name).unwrap();
                                handler(TagStreamEvent::Data(placeholder_text));
                            } else if let Some(handler) = &mut self.plain_text_handler {
                                result.extend(handler(&placeholder_text).to_vec());
                            } else {
                                result.push(placeholder_text);
                            }
                        }
                    } else if let Some(handler) = &mut self.plain_text_handler {
                        // Closing tag without an opening tag
                        let placeholder_text = format!("</{}>", tag_name);
                        result.extend(handler(&placeholder_text).to_vec());
                    } else {
                        // Closing tag without an opening tag, treat as plain text
                        let placeholder_text = format!("</{}>", tag_name);
                        result.push(placeholder_text);
                    }
                }
            }
        }

        Ok(result)
    }

    /// Processes any remaining buffered text after the stream ends.
    /// Also clears the stack and warns about unclosed tags.
    pub fn finish(&mut self) -> Result<Vec<String>, TokenProcessorError> {
        let mut result = Vec::new();

        if !self.buffer.is_empty() {
            // Any remaining text in the buffer is treated as plain text
            if !self.stack.is_empty() {
                // If we're inside a tag, send as Data to the innermost handler
                let frame_name = &self.stack.last().unwrap().name;
                let handler = self.handlers.get_mut(frame_name).unwrap();
                let output = handler(TagStreamEvent::Data(self.buffer.clone())).to_vec();
                if !output.is_empty() {
                    result.extend(output);
                }
                self.buffer.clear();
            } else if let Some(handler) = &mut self.plain_text_handler {
                // Not in any tag, use the plain text handler
                let buffer_clone = self.buffer.clone();
                result.extend(handler(&buffer_clone).to_vec());
                self.buffer.clear();
            } else {
                // If we're not inside a tag, add to result
                result.push(std::mem::take(&mut self.buffer));
            }
        }

        // Warn about unclosed tags and clear the stack
        if !self.stack.is_empty() {
            // Get unclosed tag names
            let unclosed_tags: Vec<String> = self.stack.iter().map(|f| f.name.clone()).collect();

            // Emit Close events for all unclosed tags, from innermost to outermost
            let mut tags_to_close = self.stack.clone();
            self.stack.clear(); // Clear the stack first

            // Process unclosed tags in reverse order (innermost to outermost)
            while let Some(frame) = tags_to_close.pop() {
                let handler = self.handlers.get_mut(&frame.name).unwrap();
                let output = handler(TagStreamEvent::Close).to_vec();
                if !output.is_empty() {
                    result.extend(output);
                }
            }

            // Return an error along with the result
            return Err(TokenProcessorError::UnclosedTags(unclosed_tags));
        }

        Ok(result)
    }

    /// Detects if a tag would be nested within another tag of the same name.
    /// Returns true if the tag would be nested inside itself.
    pub fn would_be_nested_same_tag(&self, tag_name: &str) -> bool {
        for frame in &self.stack {
            if frame.name == tag_name {
                return true;
            }
        }
        false
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
            HandlerResult::Drop
        });

        let output = processor.feed("<test>hello world</test>").unwrap();
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
            HandlerResult::Drop
        });

        let inner_clone = inner_events.clone();
        processor.on_tag("inner", move |evt| {
            match evt {
                TagStreamEvent::Open => inner_clone.borrow_mut().push("Open".to_string()),
                TagStreamEvent::Data(s) => inner_clone.borrow_mut().push(format!("Data: {}", s)),
                TagStreamEvent::Close => inner_clone.borrow_mut().push("Close".to_string()),
            }
            HandlerResult::Drop
        });

        processor
            .feed("<outer>begin <inner>nested</inner> end</outer>")
            .unwrap();

        assert_eq!(
            outer_events.borrow().join(", "),
            "Open, Data: begin , Data:  end, Close"
        );
        assert_eq!(
            inner_events.borrow().join(", "),
            "Open, Data: nested, Close"
        );
    }

    #[test]
    fn test_plain_text_handler() {
        let mut processor = TokenProcessor::new();
        let collected = Rc::new(RefCell::new(Vec::new()));
        let collected_clone = collected.clone();

        processor.on_plain_text(move |text| {
            collected_clone
                .borrow_mut()
                .push(format!("Plain: {}", text));
            HandlerResult::Emit(format!("Modified: {}", text))
        });

        let output = processor.feed("Some plain text").unwrap();
        assert_eq!(output, vec!["Modified: Some plain text"]);
        assert_eq!(collected.borrow().join(", "), "Plain: Some plain text");
    }

    #[test]
    fn test_partial_tag_splitting() {
        let mut processor = TokenProcessor::new();
        let collected = Rc::new(RefCell::new(Vec::new()));
        let collected_clone = collected.clone();

        processor.on_tag("think", move |evt| {
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
            HandlerResult::Drop
        });

        // First feed only part of the opening tag
        let output1 = processor.feed("<thi").unwrap();
        assert_eq!(output1, Vec::<String>::new());

        // Then feed the rest of the tag and content
        let output2 = processor.feed("nk>foo</think>").unwrap();
        assert_eq!(output2, Vec::<String>::new());

        assert_eq!(collected.borrow().join(", "), "Open, Data: foo, Close");
    }

    #[test]
    fn test_handler_tag_reinjection() {
        let mut processor = TokenProcessor::new();

        // Create a handler that emits a tag in its output
        processor.on_tag("outer", |evt| match evt {
            TagStreamEvent::Open => HandlerResult::Emit("<inner>reinjected</inner>".to_string()),
            TagStreamEvent::Data(_) => HandlerResult::Drop,
            TagStreamEvent::Close => HandlerResult::Drop,
        });

        // Register the inner tag too
        let inner_events = Rc::new(RefCell::new(Vec::new()));
        let inner_clone = inner_events.clone();
        processor.on_tag("inner", move |evt| {
            match evt {
                TagStreamEvent::Open => inner_clone.borrow_mut().push("Open".to_string()),
                TagStreamEvent::Data(s) => inner_clone.borrow_mut().push(format!("Data: {}", s)),
                TagStreamEvent::Close => inner_clone.borrow_mut().push("Close".to_string()),
            }
            HandlerResult::Drop
        });

        // The outer tag handler emits an inner tag, which gets re-processed
        let output = processor.feed("<outer></outer>").unwrap();
        assert_eq!(output, Vec::<String>::new());

        // The inner tag should have been processed
        assert_eq!(
            inner_events.borrow().join(", "),
            "Open, Data: reinjected, Close"
        );
    }

    #[test]
    fn test_nested_same_tag() {
        let mut processor = TokenProcessor::new();
        let events = Rc::new(RefCell::new(Vec::new()));
        let events_clone = events.clone();

        processor.on_tag("same", move |evt| {
            match evt {
                TagStreamEvent::Open => {
                    events_clone.borrow_mut().push("Open".to_string());
                }
                TagStreamEvent::Data(s) => {
                    events_clone.borrow_mut().push(format!("Data: {}", s));
                }
                TagStreamEvent::Close => {
                    events_clone.borrow_mut().push("Close".to_string());
                }
            }
            HandlerResult::Drop
        });

        // Feed nested same-name tags
        processor
            .feed("<same>outer<same>inner</same>outer</same>")
            .unwrap();

        // The events should show that both tags were processed
        assert_eq!(
            events.borrow().join(", "),
            "Open, Data: outer, Open, Data: inner, Close, Data: outer, Close"
        );
    }

    #[test]
    fn test_overlapping_tag_names() {
        let mut processor = TokenProcessor::new();
        let foo_events = Rc::new(RefCell::new(Vec::new()));
        let foobar_events = Rc::new(RefCell::new(Vec::new()));

        let foo_clone = foo_events.clone();
        processor.on_tag("foo", move |evt| {
            match evt {
                TagStreamEvent::Open => foo_clone.borrow_mut().push("Open".to_string()),
                TagStreamEvent::Data(s) => foo_clone.borrow_mut().push(format!("Data: {}", s)),
                TagStreamEvent::Close => foo_clone.borrow_mut().push("Close".to_string()),
            }
            HandlerResult::Drop
        });

        let foobar_clone = foobar_events.clone();
        processor.on_tag("foobar", move |evt| {
            match evt {
                TagStreamEvent::Open => foobar_clone.borrow_mut().push("Open".to_string()),
                TagStreamEvent::Data(s) => foobar_clone.borrow_mut().push(format!("Data: {}", s)),
                TagStreamEvent::Close => foobar_clone.borrow_mut().push("Close".to_string()),
            }
            HandlerResult::Drop
        });

        // This should match <foo> instead of <foobar> due to leftmost-first matching
        processor.feed("<foobar>test</foobar>").unwrap();

        // The shorter tag "foo" should NOT be matched due to the way we rebuild the matcher
        assert_eq!(foo_events.borrow().len(), 0);
        assert_eq!(foobar_events.borrow().join(", "), "Open, Data: test, Close");
    }

    #[test]
    fn test_bulk_tag_registration() {
        let outer_events = Rc::new(RefCell::new(Vec::new()));
        let inner_events = Rc::new(RefCell::new(Vec::new()));

        let outer_clone = outer_events.clone();
        let inner_clone = inner_events.clone();

        let mut processor = TokenProcessor::new();

        // Register tags and handlers separately instead of in new_with_tags
        processor.register_tags(vec![
            (
                "outer",
                Box::new(move |evt: TagStreamEvent| {
                    match evt {
                        TagStreamEvent::Open => outer_clone.borrow_mut().push("Open".to_string()),
                        TagStreamEvent::Data(s) => {
                            outer_clone.borrow_mut().push(format!("Data: {}", s))
                        }
                        TagStreamEvent::Close => outer_clone.borrow_mut().push("Close".to_string()),
                    }
                    HandlerResult::Drop
                }) as StreamingHandler,
            ),
            (
                "inner",
                Box::new(move |evt: TagStreamEvent| {
                    match evt {
                        TagStreamEvent::Open => inner_clone.borrow_mut().push("Open".to_string()),
                        TagStreamEvent::Data(s) => {
                            inner_clone.borrow_mut().push(format!("Data: {}", s))
                        }
                        TagStreamEvent::Close => inner_clone.borrow_mut().push("Close".to_string()),
                    }
                    HandlerResult::Drop
                }) as StreamingHandler,
            ),
        ]);

        processor
            .feed("<outer>begin <inner>nested</inner> end</outer>")
            .unwrap();

        assert_eq!(
            outer_events.borrow().join(", "),
            "Open, Data: begin , Data:  end, Close"
        );
        assert_eq!(
            inner_events.borrow().join(", "),
            "Open, Data: nested, Close"
        );
    }

    #[test]
    fn test_would_be_nested_same_tag() {
        let mut processor = TokenProcessor::new();
        let events = Rc::new(RefCell::new(Vec::new()));
        let events_clone = events.clone();

        processor.on_tag("same", move |evt| {
            match evt {
                TagStreamEvent::Open => {
                    events_clone.borrow_mut().push("Open".to_string());
                }
                TagStreamEvent::Data(s) => {
                    events_clone.borrow_mut().push(format!("Data: {}", s));
                }
                TagStreamEvent::Close => {
                    events_clone.borrow_mut().push("Close".to_string());
                }
            }
            HandlerResult::Drop
        });

        // First level tag isn't nested
        assert_eq!(processor.would_be_nested_same_tag("same"), false);

        // Feed opening tag
        processor.feed("<same>").unwrap();

        // Now it would be nested
        assert_eq!(processor.would_be_nested_same_tag("same"), true);

        // Different tag wouldn't be considered nested
        assert_eq!(processor.would_be_nested_same_tag("different"), false);

        // Close the tag
        processor.feed("</same>").unwrap();

        // After closing, it's no longer nested
        assert_eq!(processor.would_be_nested_same_tag("same"), false);
    }

    #[test]
    fn test_unclosed_tags() {
        let mut processor = TokenProcessor::new();
        let events = Rc::new(RefCell::new(Vec::new()));
        let events_clone = events.clone();

        processor.on_tag("test", move |evt| {
            match evt {
                TagStreamEvent::Open => {
                    events_clone.borrow_mut().push("Open".to_string());
                }
                TagStreamEvent::Data(s) => {
                    events_clone.borrow_mut().push(format!("Data: {}", s));
                }
                TagStreamEvent::Close => {
                    events_clone.borrow_mut().push("Close".to_string());
                }
            }
            HandlerResult::Drop
        });

        // Feed a tag without closing it
        processor.feed("<test>unclosed").unwrap();

        // The finish method should return an error for unclosed tags
        let result = processor.finish();
        assert!(result.is_err());

        match result {
            Err(TokenProcessorError::UnclosedTags(tags)) => {
                assert_eq!(tags, vec!["test"]);
            }
            _ => panic!("Expected UnclosedTags error"),
        }

        // The handler should still have received the Close event despite the error
        assert_eq!(events.borrow().join(", "), "Open, Data: unclosed, Close");
    }
}
