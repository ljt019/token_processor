mod token_processor;

use token_processor::{HandlerResult, TagStreamEvent, TokenProcessor};

use futures::StreamExt;
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::io::{stdout, Write};
use std::rc::Rc;

#[derive(Serialize)]
struct OllamaRequest {
    model: String,
    prompt: String,
    system: String,
    stream: bool,
}

#[derive(Deserialize, Debug)]
struct OllamaResponse {
    model: String,
    created_at: String,
    response: String,
    done: bool,
}

// Track if we're currently inside a thinking section
struct ThinkState {
    in_think_section: bool,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Track tokens for final summary
    let thinking_tokens = Rc::new(RefCell::new(Vec::new()));
    let main_tokens = Rc::new(RefCell::new(Vec::new()));

    // We don't need the think_state anymore since events are handled directly now
    let mut processor = TokenProcessor::new();

    let thinking_tokens_clone = thinking_tokens.clone();

    // Register the "think" tag with a streaming handler that processes events in real-time
    processor.on_tag("think", move |evt| {
        match evt {
            TagStreamEvent::Open => {
                // Print the opening marker when we see <think>
                println!("\n----THINK START----");
                stdout().flush().unwrap();
            }
            TagStreamEvent::Data(token) => {
                // Process and print each token in the thinking section
                print!("{}", token);
                // Flush after EACH token to see real-time streaming
                stdout().flush().unwrap();
                thinking_tokens_clone.borrow_mut().push(token);
            }
            TagStreamEvent::Close => {
                // Print the closing marker when we see </think>
                println!("\n----THINK END----");
                stdout().flush().unwrap();
            }
        }

        // Return empty to avoid sending thinking tokens to main output
        HandlerResult::Drop
    });

    // Optional: Add a plain text handler
    let main_tokens_clone1 = main_tokens.clone();
    processor.on_plain_text(move |text| {
        // For plain text, we just add it to the main tokens
        // We could do additional processing here if needed
        main_tokens_clone1.borrow_mut().push(text.to_string());
        HandlerResult::Emit(text.to_string())
    });

    // Initialize reqwest client
    let client = reqwest::Client::new();

    // Create the request to Ollama API
    let request = OllamaRequest {
        model: "cogito:14b".to_string(),
        prompt: "What do you think about dogs? Keep your response short and concise.".to_string(),
        system: "Enable Deepthinking subroutine.".to_string(),
        stream: true,
    };

    println!("Sending request to Ollama API...");
    println!("Waiting for stream chunks...");

    let mut stream = client
        .post("http://localhost:11434/api/generate")
        .json(&request)
        .timeout(std::time::Duration::from_secs(60))
        .send()
        .await?
        .bytes_stream();

    println!("Stream initialized, beginning processing...\n");

    // Set up a timeout for the entire stream processing
    let start_time = std::time::Instant::now();
    let timeout_duration = std::time::Duration::from_secs(30);

    let mut token_count = 0;
    let main_tokens_clone2 = main_tokens.clone();

    while let Some(chunk_result) = stream.next().await {
        // Check for timeout
        if start_time.elapsed() > timeout_duration {
            println!(
                "\nStream processing timeout after {} seconds",
                timeout_duration.as_secs()
            );
            break;
        }

        match chunk_result {
            Ok(chunk) => {
                let response_text = String::from_utf8_lossy(&chunk);

                for line in response_text.lines() {
                    if line.trim().is_empty() {
                        continue;
                    }

                    match serde_json::from_str::<OllamaResponse>(line) {
                        Ok(response) => {
                            token_count += 1;
                            let token = response.response.clone();

                            // Process the token - all tag events are now handled by the registered handler
                            match processor.feed(token) {
                                Ok(processed_tokens) => {
                                    // Print and track main output tokens immediately
                                    for main_token in &processed_tokens {
                                        print!("{}", main_token);
                                        // Crucial: flush after EACH token
                                        stdout().flush().unwrap();
                                        // Push the individual string token
                                        main_tokens_clone2.borrow_mut().push(main_token.clone());
                                    }
                                }
                                Err(e) => {
                                    eprintln!("Error processing token: {}", e);
                                    // Optionally decide how to proceed on error, e.g., break or continue
                                }
                            }

                            if response.done {
                                println!("\n\nGeneration complete.");
                                break;
                            }
                        }
                        Err(e) => {
                            eprintln!("Error parsing response JSON: {}", e);
                            eprintln!("Problematic line: {}", line);
                        }
                    }
                }
            }
            Err(e) => {
                eprintln!("Error receiving chunk: {}", e);
                continue;
            }
        }
    }

    // Process any remaining buffered text
    match processor.finish() {
        Ok(final_tokens) => {
            for main_token in &final_tokens {
                print!("{}", main_token);
                stdout().flush().unwrap();
                // Push the individual string token
                main_tokens_clone2.borrow_mut().push(main_token.clone());
            }
        }
        Err(e) => {
            eprintln!("Error finishing processing: {}", e);
            // Handle the unclosed tags error specifically if needed
            if let token_processor::TokenProcessorError::UnclosedTags(tags) = e {
                eprintln!("Warning: Stream ended with unclosed tags: {:?}", tags);
                // Decide if you want to push any remaining generated content despite the error
            }
        }
    }

    println!(
        "\n\nStream completed. Processed {} tokens total.",
        token_count
    );
    println!(
        "\nCollected thinking tokens: {} items",
        thinking_tokens.borrow().len()
    );
    println!(
        "Collected main tokens: {} items",
        main_tokens.borrow().len()
    );

    Ok(())
}
