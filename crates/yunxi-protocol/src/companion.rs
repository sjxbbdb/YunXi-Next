//! Typed deterministic companion policy calls.

use serde::{Deserialize, Serialize};

use crate::MemoryContextRecord;

pub const COMPANION_DECIDE_OPERATION: &str = "decide";

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompanionTone {
    #[default]
    Neutral,
    Warm,
    Direct,
    Supportive,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompanionEmotionKind {
    #[default]
    None,
    Anxiety,
    Sadness,
    Fatigue,
    Frustration,
    Joy,
    Uncertainty,
    Loneliness,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CompanionDecisionRequest {
    prompt: String,
    profile_id: Option<String>,
    display_name: Option<String>,
    memories: Vec<MemoryContextRecord>,
    unfinished_task: Option<String>,
    topic_continuation: Option<String>,
}

impl CompanionDecisionRequest {
    pub fn new(prompt: impl Into<String>) -> Self {
        Self {
            prompt: prompt.into(),
            profile_id: None,
            display_name: None,
            memories: Vec::new(),
            unfinished_task: None,
            topic_continuation: None,
        }
    }

    pub fn with_persona(
        mut self,
        profile_id: impl Into<String>,
        display_name: impl Into<String>,
    ) -> Self {
        self.profile_id = Some(profile_id.into());
        self.display_name = Some(display_name.into());
        self
    }

    pub fn with_memories(mut self, memories: Vec<MemoryContextRecord>) -> Self {
        self.memories = memories;
        self
    }

    pub fn with_unfinished_task(mut self, task: impl Into<String>) -> Self {
        self.unfinished_task = Some(task.into());
        self
    }

    pub fn with_topic_continuation(mut self, topic: impl Into<String>) -> Self {
        self.topic_continuation = Some(topic.into());
        self
    }

    pub fn prompt(&self) -> &str {
        &self.prompt
    }

    pub fn profile_id(&self) -> Option<&str> {
        self.profile_id.as_deref()
    }

    pub fn display_name(&self) -> Option<&str> {
        self.display_name.as_deref()
    }

    pub fn memories(&self) -> &[MemoryContextRecord] {
        &self.memories
    }

    pub fn unfinished_task(&self) -> Option<&str> {
        self.unfinished_task.as_deref()
    }

    pub fn topic_continuation(&self) -> Option<&str> {
        self.topic_continuation.as_deref()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CompanionDecisionResult {
    tone: CompanionTone,
    emotion: CompanionEmotionKind,
    emotion_intensity: u8,
    proactive_care: bool,
    instruction: Option<String>,
    follow_up: Option<String>,
}

impl CompanionDecisionResult {
    pub fn new(
        tone: CompanionTone,
        emotion: CompanionEmotionKind,
        emotion_intensity: u8,
        proactive_care: bool,
        instruction: Option<String>,
        follow_up: Option<String>,
    ) -> Self {
        Self {
            tone,
            emotion,
            emotion_intensity: emotion_intensity.min(100),
            proactive_care,
            instruction,
            follow_up,
        }
    }

    pub fn tone(&self) -> CompanionTone {
        self.tone
    }

    pub fn emotion(&self) -> CompanionEmotionKind {
        self.emotion
    }

    pub fn emotion_intensity(&self) -> u8 {
        self.emotion_intensity
    }

    pub fn proactive_care(&self) -> bool {
        self.proactive_care
    }

    pub fn instruction(&self) -> Option<&str> {
        self.instruction.as_deref()
    }

    pub fn follow_up(&self) -> Option<&str> {
        self.follow_up.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn companion_result_round_trip_keeps_bounded_emotion() {
        let result = CompanionDecisionResult::new(
            CompanionTone::Supportive,
            CompanionEmotionKind::Anxiety,
            140,
            true,
            Some("be supportive".to_string()),
            None,
        );
        assert_eq!(result.emotion_intensity(), 100);
        let json = serde_json::to_string(&result).expect("serialize result");
        assert_eq!(
            serde_json::from_str::<CompanionDecisionResult>(&json).expect("deserialize result"),
            result
        );
    }
}
