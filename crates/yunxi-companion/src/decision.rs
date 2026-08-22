//! Deterministic emotional signal and response-tone policy.

use yunxi_protocol::{
    CompanionDecisionRequest, CompanionDecisionResult, CompanionEmotionKind, CompanionTone,
};

pub fn decide(request: &CompanionDecisionRequest) -> CompanionDecisionResult {
    let (emotion, intensity) = classify_emotion(request.prompt());
    let has_context = request.profile_id().is_some()
        || request.display_name().is_some()
        || !request.memories().is_empty();
    let tone = if needs_supportive_tone(emotion) {
        CompanionTone::Supportive
    } else if emotion == CompanionEmotionKind::Frustration {
        CompanionTone::Direct
    } else if emotion != CompanionEmotionKind::None || has_context {
        CompanionTone::Warm
    } else {
        CompanionTone::Neutral
    };
    let proactive_care = emotion != CompanionEmotionKind::None
        || request.unfinished_task().is_some()
        || request.topic_continuation().is_some();
    let instruction = instruction_for(tone, emotion, has_context);
    let follow_up = if proactive_care {
        Some(follow_up_for(emotion).to_string())
    } else {
        None
    };
    CompanionDecisionResult::new(
        tone,
        emotion,
        intensity,
        proactive_care,
        instruction,
        follow_up,
    )
}

fn classify_emotion(value: &str) -> (CompanionEmotionKind, u8) {
    let mut best = (CompanionEmotionKind::None, 0_u8, 0_usize);
    let lower = value.to_ascii_lowercase();
    for (kind, markers) in EMOTION_PATTERNS {
        let matches = markers
            .iter()
            .filter(|marker| value.contains(*marker) || lower.contains(*marker))
            .count();
        if matches == 0 {
            continue;
        }
        let mut intensity =
            base_intensity(*kind).saturating_add(matches.saturating_sub(1).min(3) as u8 * 8);
        if [
            "很",
            "特别",
            "非常",
            "太",
            "真的",
            "崩溃",
            "撑不住",
            "受不了",
            "extremely",
            "very",
            "really",
        ]
        .iter()
        .any(|marker| value.contains(marker) || lower.contains(marker))
        {
            intensity = intensity.saturating_add(25);
        }
        if ["有点", "一点", "稍微", "somewhat", "a little", "kind of"]
            .iter()
            .any(|marker| value.contains(marker) || lower.contains(marker))
        {
            intensity = intensity.saturating_sub(15);
        }
        intensity = intensity.min(100);
        if intensity > best.1 || (intensity == best.1 && matches > best.2) {
            best = (*kind, intensity, matches);
        }
    }
    (best.0, best.1)
}

fn instruction_for(
    tone: CompanionTone,
    emotion: CompanionEmotionKind,
    has_context: bool,
) -> Option<String> {
    if tone == CompanionTone::Neutral && !has_context {
        return None;
    }
    let tone_rule = match tone {
        CompanionTone::Neutral => "Keep the response neutral and task-focused.",
        CompanionTone::Warm => "Respond warmly while remaining concrete and concise.",
        CompanionTone::Direct => {
            "Acknowledge the friction briefly, then give the clearest actionable next step."
        }
        CompanionTone::Supportive => {
            "Acknowledge the user's feeling without diagnosis, then offer one manageable next step."
        }
    };
    let emotion_rule = match emotion {
        CompanionEmotionKind::None => "",
        CompanionEmotionKind::Anxiety => " Avoid adding urgency or pressure.",
        CompanionEmotionKind::Sadness => " Do not minimize or prematurely reframe the feeling.",
        CompanionEmotionKind::Fatigue => " Reduce cognitive load and avoid a long task list.",
        CompanionEmotionKind::Frustration => " Do not be defensive or repeat failed advice.",
        CompanionEmotionKind::Joy => " Recognize the progress without exaggeration.",
        CompanionEmotionKind::Uncertainty => " Present a small number of concrete options.",
        CompanionEmotionKind::Loneliness => " Be present without implying human consciousness.",
    };
    Some(format!(
        "<yunxi_companion_policy>{tone_rule}{emotion_rule}</yunxi_companion_policy>"
    ))
}

fn follow_up_for(kind: CompanionEmotionKind) -> &'static str {
    match kind {
        CompanionEmotionKind::Anxiety => "要不要先把最压着你的点拆成一小步？",
        CompanionEmotionKind::Sadness => "你希望我先听你说完，还是一起整理下一步？",
        CompanionEmotionKind::Fatigue => "要不要先把当前任务收束成一个最小可做步骤？",
        CompanionEmotionKind::Frustration => "要不要我先帮你定位最卡住的具体点？",
        CompanionEmotionKind::Joy => "要不要顺手把这次有效的做法记录下来？",
        CompanionEmotionKind::Uncertainty => "要不要我给你两个可选方向，再一起取舍？",
        CompanionEmotionKind::Loneliness => "你希望我先陪你聊一会儿，还是一起做点轻量的事？",
        CompanionEmotionKind::None => "你希望我继续跟进这件事吗？",
    }
}

fn needs_supportive_tone(kind: CompanionEmotionKind) -> bool {
    matches!(
        kind,
        CompanionEmotionKind::Anxiety
            | CompanionEmotionKind::Sadness
            | CompanionEmotionKind::Fatigue
            | CompanionEmotionKind::Loneliness
    )
}

fn base_intensity(kind: CompanionEmotionKind) -> u8 {
    match kind {
        CompanionEmotionKind::Joy => 45,
        CompanionEmotionKind::Uncertainty => 50,
        CompanionEmotionKind::Frustration => 55,
        CompanionEmotionKind::Anxiety
        | CompanionEmotionKind::Sadness
        | CompanionEmotionKind::Fatigue
        | CompanionEmotionKind::Loneliness => 60,
        CompanionEmotionKind::None => 0,
    }
}

const EMOTION_PATTERNS: &[(CompanionEmotionKind, &[&str])] = &[
    (
        CompanionEmotionKind::Anxiety,
        &[
            "焦虑", "紧张", "压力", "担心", "慌", "害怕", "anxious", "anxiety", "stressed",
            "worried", "panic",
        ],
    ),
    (
        CompanionEmotionKind::Sadness,
        &[
            "难过",
            "伤心",
            "低落",
            "委屈",
            "沮丧",
            "sad",
            "depressed",
            "down",
            "upset",
        ],
    ),
    (
        CompanionEmotionKind::Fatigue,
        &[
            "疲惫",
            "很累",
            "太累",
            "累了",
            "困",
            "熬不住",
            "撑不住",
            "tired",
            "exhausted",
            "burnout",
        ],
    ),
    (
        CompanionEmotionKind::Frustration,
        &[
            "烦",
            "生气",
            "火大",
            "无语",
            "崩溃",
            "卡住",
            "恼火",
            "angry",
            "frustrated",
            "annoyed",
            "stuck",
        ],
    ),
    (
        CompanionEmotionKind::Joy,
        &[
            "开心",
            "高兴",
            "太好了",
            "有起色",
            "顺了",
            "完成了",
            "happy",
            "great",
            "progress",
            "done",
        ],
    ),
    (
        CompanionEmotionKind::Uncertainty,
        &[
            "不知道",
            "不确定",
            "困惑",
            "疑惑",
            "迷茫",
            "怎么做",
            "没思路",
            "confused",
            "uncertain",
            "not sure",
            "lost",
        ],
    ),
    (
        CompanionEmotionKind::Loneliness,
        &[
            "孤独",
            "孤单",
            "没人",
            "陪我",
            "想有人",
            "lonely",
            "alone",
            "companionship",
        ],
    ),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anxiety_selects_supportive_policy_without_diagnosis() {
        let result = decide(&CompanionDecisionRequest::new(
            "我现在非常焦虑，不知道怎么做",
        ));
        assert_eq!(result.tone(), CompanionTone::Supportive);
        assert_eq!(result.emotion(), CompanionEmotionKind::Anxiety);
        assert!(
            result
                .instruction()
                .expect("instruction")
                .contains("without diagnosis")
        );
        assert!(result.follow_up().is_some());
    }

    #[test]
    fn no_signal_and_no_context_stays_neutral() {
        let result = decide(&CompanionDecisionRequest::new("calculate 2 + 2"));
        assert_eq!(result.tone(), CompanionTone::Neutral);
        assert!(result.instruction().is_none());
    }
}
