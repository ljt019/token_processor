# token_processor

<!-- CI / Workflow Badges -->
[<img alt="crates.io" src="https://img.shields.io/crates/v/token_processor.svg?style=for-the-badge&color=fc8d62&logo=rust" height="19">](https://crates.io/crates/token_processor)
[<img alt="docs.rs" src="https://img.shields.io/badge/docs.rs-syn-66c2a5?style=for-the-badge&labelColor=555555&logo=docs.rs" height="19">](https://docs.rs/syn)
![Build](https://github.com/ljt019/token_processor/actions/workflows/build_and_release.yaml/badge.svg?branch=main)
![Tests](https://github.com/ljt019/token_processor/actions/workflows/tests.yaml/badge.svg?branch=main)
![Doc Tests](https://github.com/ljt019/token_processor/actions/workflows/doc_tests.yaml/badge.svg?branch=main)


A fast, streaming‐oriented token processor for Large Language Model output in Rust.

It's meant to be used with already decoded text tokens/chunks.

## Features

- **Streaming Handlers**: Callbacks on tag open, data chunks, and close events in real time.
- **Buffered Handlers**: Collect full payload between tags and invoke an async callback on close.
- **High Performance**: Uses `aho-corasick` for efficient multi-pattern scanning, including cross‐chunk matches.

## Installation

Add this to your `Cargo.toml`:
```toml
[dependencies]
token_processor = { path = "https://github.com/ljt019/token_processor"}
```

Or use cargo: 
```shell
cargo add token_processor
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