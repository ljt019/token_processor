use token_processor::{Tag, TokenProcessorBuilder};

/// This is a simple example of how to use the `TokenProcessor` with a raw tokens handler.
///
/// The raw tokens handler is called for each chunk of text that is not enclosed by any registered tags.
/// And in the case of there being no tags, it will just be called with the entire text.
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Create a token processor with a streaming tag handler
    let mut processor = TokenProcessorBuilder::new(1024)
        .raw_tokens(|chunk: &str| print!("{}", chunk))
        .build()?;

    // Usually this would be a stream of text chunks from an LLM
    let example_text_1 = "This has no tags.";
    let example_text_2 = "<think> Thoughts </think> This has tags.";

    // Process the text chunks
    println!("Processing example text 1:");
    processor.process(example_text_1).await?;

    // This is shown here just as an example for what would happen if you didn't set up any tag handlers,
    // and the text you fed contained tags.
    // If you wanted to handle <think>...</think> tags, look at the streaming_tags.rs example
    println!("\n\nProcessing example text 2:");
    processor.process(example_text_2).await?;

    // Flush any remaining text from the scanner as raw tokens (almost always empty, but good practice)
    processor.flush().await?;

    Ok(())
}
