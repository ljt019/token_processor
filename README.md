# token_processor

<!-- CI / Workflow Badges -->
[![Doc Tests](https://github.com/<USERNAME>/<REPO>/actions/workflows/doc-tests.yml/badge.svg)](https://github.com/<USERNAME>/<REPO>/actions/workflows/doc-tests.yml)
[![Tests](https://github.com/<USERNAME>/<REPO>/actions/workflows/tests.yml/badge.svg)](https://github.com/<USERNAME>/<REPO>/actions/workflows/tests.yml)
[![docs.rs](https://docs.rs/token_processor/badge.svg)](https://docs.rs/token_processor)

A fast, streaming‐oriented token processor for tagged text in Rust.

## Features

- **Streaming Handlers**: Callbacks on tag open, data chunks, and close events in real time.
- **Buffered Handlers**: Collect full payload between tags and invoke an async callback on close.
- **High Performance**: Uses `aho-corasick` for efficient multi-pattern scanning, including cross‐chunk matches.

## Installation

Add this to your `Cargo.toml`:
```toml
[dependencies]
token_processor = "0.1.0"
```

## Quickstart

```rust
use token_processor::{Tag, TokenProcessorBuilder};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut processor = TokenProcessorBuilder::new(1024)
        .streaming_tag(
            Tag::new("<think>"),
            || print!("[open] "),
            |chunk: &str| print!("{}", chunk),
            || print!(" [close]"),
        )
        .buffered_tag(Tag::new("<tool>"), |payload: String| async move {
            println!("[tool payload] {}", payload);
        })
        .raw_tokens(|chunk: &str| print!("{}", chunk))
        .build()?;

    processor.process("Hello <think>world</think> <tool>data</tool>!").await?;
    processor.flush().await?;
    Ok(())
}
```

## Examples

Explore the `examples/` folder for more usage scenarios:
- `simple.rs` – raw tokens only
- `streaming_tags.rs` – streaming‐mode tag handling
- `buffered_tags.rs` – buffered‐mode tag handling

## Testing

Run the full test suite:
```bash
cargo test
```

## License

Licensed under MIT OR Apache‐2.0. See [LICENSE](LICENSE) for details.