mod scanner;

pub use scanner::{Event, Scanner, ScannerError, Tag};

use std::future::Future;
use std::pin::Pin;
use thiserror::Error;

// --- TYPE ALIASES ---
type StreamingOpenHandler = Box<dyn FnMut() + Send>;
type StreamingDataHandler = Box<dyn FnMut(&str) + Send>;
type StreamingCloseHandler = Box<dyn FnMut() + Send>;
type BufferedCloseHandler =
    Box<dyn FnMut(String) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send>;
type RawTokensHandler = Box<dyn FnMut(&str) + Send>;

/// A streaming‐mode tag handler:
struct StreamingTag {
    tag: Tag,
    on_open: StreamingOpenHandler,
    on_data: StreamingDataHandler,
    on_close: StreamingCloseHandler,
}

/// A buffered‐mode tag handler:
struct BufferedTag {
    tag: Tag,
    on_close: BufferedCloseHandler,
}

/// Union of the two registration types
enum RegisteredTag {
    Streaming(StreamingTag),
    Buffered(BufferedTag),
}

#[derive(Debug, Error)]
pub enum ProcessorError {
    #[error("Scanner error: {0}")]
    Scanner(#[from] ScannerError),

    #[error("Mandatory raw tokens handler was not provided.")]
    MissingRawTokensHandler,
}

/// Builder for a token processor.
///
/// Configure how different token events are handled before building:
/// - `.raw_tokens(...)`: *mandatory* handler for text outside of any tags.
/// - `.streaming_tag(tag, on_open, on_data, on_close)`: optional streaming-mode handlers; provide three separate callbacks: one when the tag opens, one for each chunk of text inside the tag, and one when the tag closes.
/// - `.buffered_tag(...)`: optional buffered-mode handler that accumulates all text inside a tag and invokes an async callback once on close.
///
/// To use, start with [`TokenProcessorBuilder::new`], register your handlers, and finalize with `.build()`.
pub struct TokenProcessorBuilder {
    tags: Vec<RegisteredTag>,
    on_raw_tokens: Option<RawTokensHandler>,
    max_buffer: usize,
}

impl Default for TokenProcessorBuilder {
    fn default() -> Self {
        Self::new(1024)
    }
}

impl TokenProcessorBuilder {
    /// Creates a new `TokenProcessorBuilder` with the given maximum buffer size.
    ///
    /// `max_buffer` determines the maximum accumulated text for buffered tags.
    pub fn new(max_buffer: usize) -> Self {
        Self {
            tags: Vec::new(),
            on_raw_tokens: None,
            max_buffer,
        }
    }

    /// Register a streaming-mode handler for a specific `tag`.
    ///
    /// - `on_open` is called when the tag opens.
    /// - `on_data` is called for each chunk of text inside the tag.
    /// - `on_close` is called when the tag closes.
    pub fn streaming_tag(
        mut self,
        tag: Tag,
        on_open: impl FnMut() + Send + 'static,
        on_data: impl FnMut(&str) + Send + 'static,
        on_close: impl FnMut() + Send + 'static,
    ) -> Self {
        self.tags.push(RegisteredTag::Streaming(StreamingTag {
            tag,
            on_open: Box::new(on_open),
            on_data: Box::new(on_data),
            on_close: Box::new(on_close),
        }));
        self
    }

    /// Register a buffered-mode handler for a specific `tag`.
    ///
    /// Accumulates all text between the opening and closing tag and invokes the
    /// asynchronous `on_close` callback with the complete content when the tag closes.
    pub fn buffered_tag<Fut>(
        mut self,
        tag: Tag,
        mut on_close: impl FnMut(String) -> Fut + Send + 'static,
    ) -> Self
    where
        Fut: Future<Output = ()> + Send + 'static,
    {
        let cb: BufferedCloseHandler = Box::new(
            move |payload: String| -> Pin<Box<dyn Future<Output = ()> + Send>> {
                Box::pin(on_close(payload))
            },
        );
        self.tags
            .push(RegisteredTag::Buffered(BufferedTag { tag, on_close: cb }));
        self
    }

    /// Set the handler for raw text outside of any tags.
    ///
    /// The `handler` is called with each chunk of text not enclosed by any registered tag.
    pub fn raw_tokens(mut self, handler: impl FnMut(&str) + Send + 'static) -> Self {
        self.on_raw_tokens = Some(Box::new(handler));
        self
    }

    /// Finalize configuration and build a [`TokenProcessor`].
    ///
    /// Returns a `ProcessorError` if no raw token handler was provided or
    /// if scanner initialization fails.
    pub fn build(self) -> Result<TokenProcessor, ProcessorError> {
        let tags: Vec<Tag> = self
            .tags
            .iter()
            .map(|rt| match rt {
                RegisteredTag::Streaming(s) => s.tag.clone(),
                RegisteredTag::Buffered(b) => b.tag.clone(),
            })
            .collect();

        let scanner = Scanner::new(&tags, self.max_buffer)?;

        let on_raw_tokens = self
            .on_raw_tokens
            .ok_or(ProcessorError::MissingRawTokensHandler)?;

        Ok(TokenProcessor {
            scanner,
            handlers: self.tags,
            on_raw_tokens,
            active_streaming: Vec::new(),
            active_buffered: None,
        })
    }
}

/// Processor that feeds input chunks to the scanner and dispatches events.
///
/// Created via [`TokenProcessorBuilder`].
pub struct TokenProcessor {
    scanner: Scanner,
    handlers: Vec<RegisteredTag>,
    on_raw_tokens: RawTokensHandler,

    // runtime state:
    active_streaming: Vec<usize>,
    active_buffered: Option<(usize, String)>,
}

impl TokenProcessor {
    /// Process a chunk of text, emitting events to registered handlers.
    ///
    /// This method is asynchronous to accommodate buffered tag close handlers.
    pub async fn process(&mut self, chunk: &str) -> Result<(), ProcessorError> {
        let events = self.scanner.feed(chunk)?;

        for ev in events {
            match ev {
                Event::Raw(text) => {
                    if let Some((_idx, buf)) = &mut self.active_buffered {
                        buf.push_str(&text);
                    }
                    for &stream_idx in &self.active_streaming {
                        if let RegisteredTag::Streaming(st) = &mut self.handlers[stream_idx] {
                            (st.on_data)(&text);
                        }
                    }
                    if self.active_streaming.is_empty() && self.active_buffered.is_none() {
                        (self.on_raw_tokens)(&text);
                    }
                }

                Event::Open(tag) => {
                    if let Some((idx, handler)) =
                        self.handlers.iter_mut().enumerate().find(|(_, h)| match h {
                            RegisteredTag::Streaming(s) => s.tag == tag,
                            RegisteredTag::Buffered(b) => b.tag == tag,
                        })
                    {
                        match handler {
                            RegisteredTag::Streaming(st) => {
                                (st.on_open)();
                                self.active_streaming.push(idx);
                            }
                            RegisteredTag::Buffered(_) => {
                                self.active_buffered = Some((idx, String::new()));
                            }
                        }
                    }
                }

                Event::Close(tag) => {
                    if let Some((idx, handler)) =
                        self.handlers.iter_mut().enumerate().find(|(_, h)| match h {
                            RegisteredTag::Streaming(s) => s.tag == tag,
                            RegisteredTag::Buffered(b) => b.tag == tag,
                        })
                    {
                        match handler {
                            RegisteredTag::Streaming(st) => {
                                (st.on_close)();
                                self.active_streaming.retain(|&i| i != idx);
                            }
                            RegisteredTag::Buffered(bt) => {
                                if let Some((bidx, buf)) = self.active_buffered.take() {
                                    debug_assert_eq!(bidx, idx);
                                    (bt.on_close)(buf).await;
                                }
                            }
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// Flush any remaining text from the scanner as raw tokens.
    ///
    /// Should be called after the final input chunk to process trailing content.
    pub async fn flush(&mut self) -> Result<(), ProcessorError> {
        let tail = self.scanner.finish();
        for ev in tail {
            if let Event::Raw(txt) = ev {
                (self.on_raw_tokens)(&txt);
            }
        }
        Ok(())
    }
}
