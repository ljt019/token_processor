use token_processor::{Tag, TokenProcessorBuilder};

/// This is an example of how to use the `TokenProcessor` with a buffered tag handler.
///
/// The buffered tag handler accumulates all text between the opening and closing tag and invokes the
/// asynchronous `on_close` callback with the complete content when the tag closes.
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Create a token processor with a buffered tag handler
    let mut processor = TokenProcessorBuilder::new(1024)
        .buffered_tag(Tag::new("<tool_call>"), |payload: String| async move {
            println!("[tool_call buffered]: {}", payload);
        })
        .raw_tokens(|chunk: &str| print!("{}", chunk))
        .build()?;

    let example_text1 = "This has no tags.";
    // usually the text between the tags is a JSON object that describes the tool call
    // for this example, we'll just use a simple string
    let example_text2 = "<tool_call>echo hello world</tool_call> This is after.";

    println!("Processing example text 1:");
    processor.process(example_text1).await?;

    println!("\n\nProcessing example text 2:");
    processor.process(example_text2).await?;

    // Flush any remaining tokens
    processor.flush().await?;
    Ok(())
}
