use aho_corasick::AhoCorasick;
use std::fmt::Display;
use thiserror::Error;

/// Represents a pair of opening and closing tags.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tag {
    name: String,
    open: String,
    close: String,
}

impl Display for Tag {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.open)
    }
}

impl Tag {
    pub fn new(open: &str) -> Self {
        let open_string: String = open.into();
        let text_between_angle_brackets = open_string.trim_start_matches('<').trim_end_matches('>');
        let close = format!("</{}>", text_between_angle_brackets);

        Self {
            name: text_between_angle_brackets.into(),
            open: open_string,
            close,
        }
    }
}

/// An event emitted by the `Scanner`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Open(Tag),
    Close(Tag),
    Raw(String),
}

/// Errors that can occur when using `Scanner`.
#[derive(Debug, Error)]
pub enum ScannerError {
    /// Error during Aho-Corasick automaton construction.
    #[error("invalid patterns: {0}")]
    AhoCorasickBuildError(#[from] aho_corasick::BuildError),

    /// Buffer exceeded max_buffer size without finding a match end.
    #[error("buffer overflow: no closing tag within {0} bytes")]
    BufferOverflow(usize),
}

/// A streaming tag‐scanner that can handle cross‐chunk matches.
/// Tags are treated as literal strings defined in `Tag` pairs.
///
/// # Examples
///
/// ```
/// use token_processor::{Scanner, Event, Tag, ScannerError};
///
/// fn main() -> Result<(), ScannerError> {
///     let tags = &[
///         Tag::new("<think>"),
///     ];
///     let mut s = Scanner::new(tags, 128)?;
///
///     let text = "Text before tags <think>text inside of tags</think> then text after tags.";
///     let ev = s.feed(text)?;
///
///     assert_eq!(
///         ev,
///         vec![
///             Event::Raw("Text before tags ".into()),
///             Event::Open(Tag::new("<think>")),
///             Event::Raw("text inside of tags".into()),
///             Event::Close(Tag::new("<think>")),
///             Event::Raw(" then text after tags.".into()), // Should now be emitted by feed
///         ]
///     );
///
///     let final_ev = s.finish();
///     assert!(final_ev.is_empty()); // finish() should be empty
///     Ok(())
/// }
/// ```
pub struct Scanner {
    ac: AhoCorasick,
    tags: Vec<Tag>,
    buf: String,        // Buffer for handling cross-chunk matches
    split_index: usize, // Index separating open from close in patterns_flat
    max_buffer: usize,  // Max buffer size in bytes
    pending: usize,
}

impl Scanner {
    /// Create a new scanner.
    ///
    /// `tags`: List of `Tag` pairs defining the open/close literals.
    /// `max_buffer`: Maximum bytes to buffer waiting for a match end.
    pub fn new(tags: &[Tag], max_buffer: usize) -> Result<Self, ScannerError> {
        let patterns_flat: Vec<String> = tags
            .iter()
            .flat_map(|tag| vec![tag.open.clone(), tag.close.clone()])
            .collect();

        let ac = AhoCorasick::builder()
            .match_kind(aho_corasick::MatchKind::LeftmostFirst) // Ensure consistent match order
            .build(&patterns_flat)
            .map_err(ScannerError::AhoCorasickBuildError)?;

        let split_index = tags.len();

        Ok(Scanner {
            ac,
            tags: tags.to_vec(),
            buf: String::new(),
            split_index,
            max_buffer,
            pending: 0,
        })
    }

    /// Feed the scanner a new chunk of text.
    ///
    /// Returns a list of `Event`s found within the chunk and potentially
    /// carried over from previous chunks. May return
    /// `Err(ScannerError::BufferOverflow)` if no match completes within
    /// `max_buffer` bytes.
    #[must_use]
    pub fn feed(&mut self, chunk: &str) -> Result<Vec<Event>, ScannerError> {
        // If we're already inside <think>…</think> (pending>0)
        // and this chunk has no '<', it can't open/close anything:
        if self.pending > 0 && !chunk.contains('<') {
            if chunk.len() > self.max_buffer {
                return Err(ScannerError::BufferOverflow(self.max_buffer));
            }
            // emit as soon as it arrives:
            return Ok(vec![Event::Raw(chunk.to_string())]);
        }

        // fast path for pure raw when outside tags
        if self.buf.is_empty() && !chunk.contains('<') && self.pending == 0 {
            if chunk.len() > self.max_buffer {
                return Err(ScannerError::BufferOverflow(self.max_buffer));
            }
            // Return early if the chunk is empty to avoid emitting empty Raw events.
            if chunk.is_empty() {
                return Ok(Vec::new());
            }
            return Ok(vec![Event::Raw(chunk.to_string())]);
        }

        self.buf.push_str(chunk);

        // Check for buffer overflow after appending
        if self.buf.len() > self.max_buffer {
            return Err(ScannerError::BufferOverflow(self.max_buffer));
        }

        let mut out = Vec::new();
        let mut last_match_end = 0;

        for mat in self.ac.find_iter(&self.buf) {
            let start = mat.start();
            let end = mat.end();
            let pattern_id = mat.pattern().as_usize();

            if start > last_match_end {
                out.push(Event::Raw(self.buf[last_match_end..start].to_string()));
            }

            // Determine if it's an open or close tag based on pattern_id
            let tag_index = pattern_id / 2;
            let tag = &self.tags[tag_index];

            if pattern_id < self.split_index * 2 && pattern_id % 2 == 0 {
                out.push(Event::Open(tag.clone()));
                self.pending += 1;
            } else {
                out.push(Event::Close(tag.clone()));
                self.pending = self.pending.saturating_sub(1);
            }

            last_match_end = end;
        }

        let remaining_suffix = &self.buf[last_match_end..];

        if !remaining_suffix.is_empty() {
            if self.pending == 0 && !remaining_suffix.contains('<') {
                // Outside a tag and suffix has no '<': emit immediately and clear buffer.
                out.push(Event::Raw(remaining_suffix.to_string()));
                self.buf.clear(); // Clear buffer as everything was processed
            } else {
                // Inside a tag OR suffix contains '<': Need to buffer the suffix.
                // Drain the part *before* the suffix.
                self.buf.drain(..last_match_end);
            }
        } else {
            // No suffix remaining after last match, just drain processed part
            self.buf.drain(..last_match_end);
        }

        Ok(out)
    }

    /// Call once when the input stream is finished to flush any remaining
    /// buffered raw text (no tags will be emitted here).
    #[must_use]
    #[inline]
    pub fn finish(&mut self) -> Vec<Event> {
        if self.buf.is_empty() {
            Vec::new() // No remaining text
        } else {
            // Take the remaining buffer content and emit as Raw
            let rem = std::mem::take(&mut self.buf);
            vec![Event::Raw(rem)]
        }
    }

    /// Reset the buffer to empty
    pub fn reset_buffer(&mut self) {
        self.buf.clear();
    }
}
