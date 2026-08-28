use std::fmt;

use objc2_app_kit::{NSRunningApplication, NSWorkspace};
use objc2_foundation::NSString;
use serde::{Deserialize, Serialize};

const INPUT_BUNDLE_ID: &str = "it.focusense.input-app";
const CHATGPT_BUNDLE_IDS: [&str; 2] = ["com.openai.codex", "com.openai.chat"];

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ExternalOwner {
    Input,
    ChatGpt,
}

impl fmt::Display for ExternalOwner {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Input => "Work Louder Input",
            Self::ChatGpt => "ChatGPT",
        })
    }
}

pub fn external_owner() -> Option<ExternalOwner> {
    objc2::rc::autoreleasepool(|_| {
        let input_running = !NSRunningApplication::runningApplicationsWithBundleIdentifier(
            &NSString::from_str(INPUT_BUNDLE_ID),
        )
        .is_empty();
        let frontmost_bundle = NSWorkspace::sharedWorkspace()
            .frontmostApplication()
            .and_then(|app| app.bundleIdentifier())
            .map(|bundle| bundle.to_string());
        classify_external_owner(input_running, frontmost_bundle.as_deref())
    })
}

fn classify_external_owner(
    input_running: bool,
    frontmost_bundle: Option<&str>,
) -> Option<ExternalOwner> {
    if input_running {
        Some(ExternalOwner::Input)
    } else if frontmost_bundle.is_some_and(|bundle| CHATGPT_BUNDLE_IDS.contains(&bundle)) {
        Some(ExternalOwner::ChatGpt)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_wins_then_chatgpt_requires_frontmost() {
        assert_eq!(
            classify_external_owner(true, Some("com.openai.chat")),
            Some(ExternalOwner::Input)
        );
        assert_eq!(
            classify_external_owner(false, Some("com.openai.codex")),
            Some(ExternalOwner::ChatGpt)
        );
        assert_eq!(
            classify_external_owner(false, Some("com.example.other")),
            None
        );
    }
}
