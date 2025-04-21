use std::sync::{Arc, Mutex};

use token_processor::{Event, Scanner, Tag, TokenProcessorBuilder};

// Basic unit tests for the `token_processor` crate.
//
// The goal is to ensure that the public API behaves as documented in the
// library‐level examples.

#[test]
fn scanner_emits_expected_events() {
    // Replicates the example from the crate documentation.
    let tag = Tag::new("<think>");
    let mut scanner = Scanner::new(&[tag.clone()], 128).expect("scanner should build");

    let text = "Text before tags <think>text inside of tags</think> then text after tags.";
    let events = scanner.feed(text).expect("scanner feed must succeed");

    assert_eq!(
        events,
        vec![
            Event::Raw("Text before tags ".into()),
            Event::Open(tag.clone()),
            Event::Raw("text inside of tags".into()),
            Event::Close(tag.clone()),
            Event::Raw(" then text after tags.".into()),
        ]
    );

    // After finishing the stream, the scanner should return no further events.
    assert!(scanner.finish().is_empty());
}

#[tokio::test]
async fn token_processor_handles_raw_tokens() {
    // Collects every raw chunk fed into the processor.
    let collected = Arc::new(Mutex::new(String::new()));
    let collector = collected.clone();

    let mut processor = TokenProcessorBuilder::new(128)
        .raw_tokens(move |chunk: &str| {
            let mut buf = collector.lock().unwrap();
            buf.push_str(chunk);
        })
        .build()
        .expect("builder succeeds");

    let sample = "just some plain text without any tags";
    processor
        .process(sample)
        .await
        .expect("processing succeeds");
    processor.flush().await.expect("flush succeeds");

    assert_eq!(&*collected.lock().unwrap(), sample);
}

#[tokio::test]
async fn token_processor_streaming_tag_callbacks() {
    let tag = Tag::new("<think>");
    let output = Arc::new(Mutex::new(String::new()));

    let open_out = output.clone();
    let data_out = output.clone();
    let close_out = output.clone();

    let mut processor = TokenProcessorBuilder::new(128)
        .streaming_tag(
            tag.clone(),
            move || {
                let mut buf = open_out.lock().unwrap();
                buf.push_str("[open]");
            },
            move |chunk: &str| {
                let mut buf = data_out.lock().unwrap();
                buf.push_str(chunk);
            },
            move || {
                let mut buf = close_out.lock().unwrap();
                buf.push_str("[close]");
            },
        )
        .raw_tokens(|_| {}) // raw tokens ignored for this test
        .build()
        .expect("builder succeeds");

    processor
        .process("prefix <think>Hello world</think> suffix")
        .await
        .expect("processing succeeds");
    processor.flush().await.expect("flush succeeds");

    // Only the content inside the tag (along with the open/close markers we inserted)
    // should be in `output`.
    assert_eq!(&*output.lock().unwrap(), "[open]Hello world[close]");
}

#[tokio::test]
async fn token_processor_buffered_tag_collects_payload() {
    let tag = Tag::new("<tool_call>");
    let payload_store = Arc::new(Mutex::new(None::<String>));
    let store_clone = payload_store.clone();

    let mut processor = TokenProcessorBuilder::new(128)
        .buffered_tag(tag.clone(), move |payload: String| {
            let store = store_clone.clone();
            async move {
                let mut slot = store.lock().unwrap();
                *slot = Some(payload);
            }
        })
        .raw_tokens(|_| {})
        .build()
        .expect("builder succeeds");

    let text = "<tool_call>echo hello world</tool_call> trailing";
    processor.process(text).await.expect("processing succeeds");
    processor.flush().await.expect("flush succeeds");

    let stored = payload_store.lock().unwrap().clone();
    assert_eq!(stored.as_deref(), Some("echo hello world"));
}
