//! Deterministic local messages used when no model provider is available.

use yunxi_protocol::MailboxItemKind;

const MAX_TEMPLATE_FIELD_CHARS: usize = 160;
const MAX_TEMPLATE_MESSAGE_CHARS: usize = 2_000;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TemplateContext {
    recipient: String,
    focus: String,
}

impl TemplateContext {
    pub fn new(recipient: impl Into<String>, focus: impl Into<String>) -> Self {
        Self {
            recipient: compact(&recipient.into(), MAX_TEMPLATE_FIELD_CHARS),
            focus: compact(&focus.into(), MAX_TEMPLATE_FIELD_CHARS),
        }
    }

    pub fn recipient(&self) -> &str {
        &self.recipient
    }

    pub fn focus(&self) -> &str {
        &self.focus
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TemplateSource {
    LocalDeterministic,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GeneratedTemplate {
    kind: MailboxItemKind,
    subject: String,
    content: String,
    source: TemplateSource,
}

impl GeneratedTemplate {
    pub fn kind(&self) -> MailboxItemKind {
        self.kind
    }

    pub fn subject(&self) -> &str {
        &self.subject
    }

    pub fn content(&self) -> &str {
        &self.content
    }

    pub fn source(&self) -> TemplateSource {
        self.source
    }
}

/// Render a bounded, deterministic love letter without calling a model.
///
/// The source is deliberately exposed so callers cannot mistake this fallback
/// for an LLM-generated message.
pub fn render_love_letter(context: &TemplateContext) -> GeneratedTemplate {
    let recipient = if context.recipient().is_empty() {
        "你"
    } else {
        context.recipient()
    };
    let focus = if context.focus().is_empty() {
        "眼前的事情"
    } else {
        context.focus()
    };
    let content = compact(
        &format!(
            "给{recipient}：\n\n今天也辛苦了。关于“{focus}”，不用急着一次做完，能向前一点就已经很好。等你准备好时，我会在这里陪你继续。\n\n云熙",
        ),
        MAX_TEMPLATE_MESSAGE_CHARS,
    );
    GeneratedTemplate {
        kind: MailboxItemKind::LoveLetter,
        subject: compact(
            &format!("给{recipient}的一封小信"),
            MAX_TEMPLATE_FIELD_CHARS,
        ),
        content,
        source: TemplateSource::LocalDeterministic,
    }
}

fn compact(value: &str, maximum: usize) -> String {
    let value = value.trim();
    if value.chars().count() <= maximum {
        return value.to_string();
    }
    let mut output = value
        .chars()
        .take(maximum.saturating_sub(3))
        .collect::<String>();
    output.push_str("...");
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallback_is_deterministic_and_explicitly_local() {
        let context = TemplateContext::new("小云", "整理路线图");
        let first = render_love_letter(&context);
        let second = render_love_letter(&context);
        assert_eq!(first, second);
        assert_eq!(first.kind(), MailboxItemKind::LoveLetter);
        assert_eq!(first.source(), TemplateSource::LocalDeterministic);
        assert!(first.content().contains("整理路线图"));
    }

    #[test]
    fn template_fields_are_bounded() {
        let context = TemplateContext::new("x".repeat(500), "y".repeat(500));
        let message = render_love_letter(&context);
        assert!(context.recipient().chars().count() <= MAX_TEMPLATE_FIELD_CHARS);
        assert!(message.content().chars().count() <= MAX_TEMPLATE_MESSAGE_CHARS);
    }
}
