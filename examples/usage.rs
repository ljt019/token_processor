use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json;
use std::io::Write;
use token_processor::{Event, Scanner, Tag};

#[derive(Serialize)]
struct OllamaRequest {
    model: String,
    prompt: String,
    system: String,
    stream: bool,
}

#[derive(Deserialize, Debug)]
struct OllamaResponse {
    _model: String,
    _created_at: String,
    response: String,
    _done: bool,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let tags = vec![Tag::new("<think>")];

    let mut s = Scanner::new(&tags, 1024).unwrap();

    // Initialize reqwest client
    let client = reqwest::Client::new();

    // Create the request to Ollama API
    let request = OllamaRequest {
        model: "deepseek-r1:7b".to_string(),
        prompt: "What is 100+100?".to_string(),
        system: "You are a helpful assistant.".to_string(),
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

    while let Some(chunk_result) = stream.next().await {
        let chunk = chunk_result?;
        let json_text = String::from_utf8(chunk.to_vec())?;

        match serde_json::from_str::<OllamaResponse>(&json_text) {
            Ok(ollama_response) => {
                let events = s.feed(&ollama_response.response)?;

                for event in events {
                    match event {
                        Event::Raw(text) => print!("Raw: {}\n", text),
                        Event::Open(tag) => print!("Open: {}\n", tag),
                        Event::Close(tag) => print!("Close: {}\n", tag),
                    }
                }
                std::io::stdout().flush()?;
            }
            Err(e) => {
                eprintln!("\n[Failed to parse JSON chunk: {}]", e);
            }
        }
    }
    println!();

    Ok(())
}
