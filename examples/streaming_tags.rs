use token_processor::{Tag, TokenProcessorBuilder};

/// This is an example of how to use the `TokenProcessor` with a streaming tag handler.
///
/// The streaming tag handler is called for each chunk of text that is enclosed by a registered tag.
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Create a token processor with a streaming tag handler
    let mut processor = TokenProcessorBuilder::new(1024)
        .streaming_tag(
            Tag::new("<think>"),
            || print!("[think start] "),
            |chunk: &str| print!("{}", chunk),
            || print!(" [think end]"),
        )
        .raw_tokens(|chunk: &str| print!("{}", chunk))
        .build()?;

    // Usually this would be a stream of text chunks from an LLM
    let example_text_1 = "This has no tags.";
    let example_text_2 = "<think> Thoughts </think> This has tags.";

    // Process the text chunks
    println!("Processing example text 1:");
    processor.process(example_text_1).await?;

    println!("\n\nProcessing example text 2:");
    processor.process(example_text_2).await?;

    // Flush any remaining text from the scanner as raw tokens (almost always empty, but good practice)
    processor.flush().await?;

    Ok(())
}
