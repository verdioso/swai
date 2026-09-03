//! SWAI — Council execution backend abstraction.
//!
//! The [`Executor`] trait abstracts the LLM inference backend used by each
//! debate stage. The pipeline drives `execute_stream`, which the default
//! implementation backs with the non-streaming `execute` (chunking its output
//! into `TokenChunk` events for backends without live streaming).
//!
//! Keeping the trait in its own module keeps `pipeline.rs` focused on the
//! orchestration logic while the backend contract stays small and testable.

use crate::council::events::CouncilEvent;
use crate::council::types::PipelineStage;

/// Buffered characters before a streaming token chunk is emitted.
pub const STREAM_CHUNK_BUDGET: usize = 16;

/// Abstraction over the LLM inference backend used by each debate stage.
pub trait Executor: Send + Sync {
    fn execute(&self, stage: &PipelineStage, input: &str) -> Result<String, String>;

    /// Check and consume pending human guidance injected during live debate.
    fn take_human_feedback(&self) -> Option<String> {
        None
    }

    /// Stream a stage's output, emitting one `CouncilEvent::TokenChunk` per
    /// delta as it is produced. The default implementation runs the
    /// non-streaming `execute` and splits its output into word-sized chunks,
    /// which is sufficient for backends that do not expose live streaming.
    fn execute_stream(
        &self,
        stage: &PipelineStage,
        input: &str,
        stage_index: usize,
        emit: &dyn Fn(&CouncilEvent),
    ) -> Result<String, String> {
        let output = self.execute(stage, input)?;
        for chunk in chunk_output(&output) {
            emit(&CouncilEvent::TokenChunk {
                stage_index,
                text: chunk,
            });
        }
        Ok(output)
    }
}

/// Split generated text into streaming token chunks.
///
/// Chunks are bounded by `STREAM_CHUNK_BUDGET` characters or whitespace, and
/// concatenating them in order reconstructs the original text exactly. An
/// empty input yields a single empty chunk so a stage always emits at least
/// one token event.
pub fn chunk_output(text: &str) -> Vec<String> {
    let mut chunks: Vec<String> = Vec::new();
    let mut buf = String::new();
    for ch in text.chars() {
        buf.push(ch);
        if ch.is_whitespace() || buf.len() >= STREAM_CHUNK_BUDGET {
            chunks.push(std::mem::take(&mut buf));
        }
    }
    if !buf.is_empty() {
        chunks.push(buf);
    }
    if chunks.is_empty() {
        chunks.push(text.to_string());
    }
    chunks
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::council::types::{CouncilRole, PipelineStage};

    struct FixedExecutor(String);

    impl Executor for FixedExecutor {
        fn execute(&self, _stage: &PipelineStage, _input: &str) -> Result<String, String> {
            Ok(self.0.clone())
        }
    }

    #[test]
    fn test_chunk_output_splits_on_whitespace_and_budget() {
        // Whitespace splitting: concatenating the chunks reconstructs the
        // original text exactly.
        let chunks = chunk_output("one two three four five six seven");
        let joined: String = chunks.iter().cloned().collect();
        assert_eq!(joined, "one two three four five six seven");

        // Budget splitting: a word longer than STREAM_CHUNK_BUDGET is split
        // into bounded chunks that still reconstruct the original text.
        let long_word = "abcdefghijABCDEFGHIJ012345"; // 28 chars > 16
        let chunks = chunk_output(long_word);
        let joined: String = chunks.iter().cloned().collect();
        assert_eq!(joined, long_word);
        for chunk in &chunks {
            assert!(chunk.len() <= STREAM_CHUNK_BUDGET, "chunk too long: {}", chunk.len());
        }
    }

    #[test]
    fn test_chunk_output_empty_input_emits_single_chunk() {
        let chunks = chunk_output("");
        assert_eq!(chunks.len(), 1);
    }

    #[test]
    fn test_execute_stream_emits_chunks() {
        use std::cell::RefCell;
        use std::rc::Rc;

        let emitted: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
        let stage = PipelineStage {
            model_id: "m".into(),
            role: CouncilRole::Generator,
            prompt_template: String::new(),
            temperature: 0.7,
            top_p: 0.9,
            system_prompt: None,
        };
        let executor = FixedExecutor("hello world".into());
        let out = executor
            .execute_stream(&stage, "in", 0, &|e| match e {
                CouncilEvent::TokenChunk { text, .. } => {
                    emitted.borrow_mut().push(text.clone())
                }
                _ => {}
            })
            .unwrap();
        assert_eq!(out, "hello world");
        let joined: String = emitted.borrow().iter().cloned().collect();
        assert_eq!(joined, "hello world");
    }
}
