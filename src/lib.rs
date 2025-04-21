mod scanner;

pub use scanner::{Event, Scanner, ScannerError, Tag};

pub struct TokenProcessor {
    scanner: Scanner,
}

impl TokenProcessor {
    pub fn new(tags: &[Tag]) -> Result<Self, ScannerError> {
        let scanner = Scanner::new(tags, 1024)?;
        Ok(Self { scanner })
    }

    pub fn process(&mut self, text: &str) -> Result<Vec<Event>, ScannerError> {
        self.scanner.feed(text)
    }
}
