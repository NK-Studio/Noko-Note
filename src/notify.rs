//! 요약 결과를 외부 메신저로 보낸다. 지금은 Slack·Discord Incoming Webhook을 지원한다.
//! 새 대상(Teams, 이메일 등)은 `Target`에 항목을 추가하고 `send`에서 처리하면 된다.

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use serde_json::json;

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Target {
    Slack,
    Discord,
}

impl Target {
    pub fn label(self) -> &'static str {
        match self {
            Target::Slack => "Slack",
            Target::Discord => "Discord",
        }
    }
}

/// Discord 메시지 한 건의 최대 길이.
const DISCORD_LIMIT: usize = 2000;
/// Slack 메시지 한 건에 안전하게 담을 길이(공식 한도 40,000자보다 넉넉히 작게).
const SLACK_LIMIT: usize = 3500;

pub async fn send(client: &reqwest::Client, target: Target, webhook: &str, markdown: &str) -> Result<()> {
    if webhook.trim().is_empty() {
        bail!("이 방에 {} 웹훅 주소가 설정되어 있지 않습니다", target.label());
    }
    let (limit, text) = match target {
        Target::Slack => (SLACK_LIMIT, to_slack_mrkdwn(markdown)),
        Target::Discord => (DISCORD_LIMIT, markdown.to_string()),
    };
    for chunk in split_message(&text, limit) {
        let body = match target {
            Target::Slack => json!({ "text": chunk }),
            Target::Discord => json!({ "content": chunk }),
        };
        let resp = client
            .post(webhook.trim())
            .json(&body)
            .send()
            .await
            .with_context(|| format!("{}에 연결하지 못했습니다", target.label()))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let detail = resp.text().await.unwrap_or_default();
            bail!("{} 전송 실패 ({status}): {}", target.label(), detail.trim());
        }
    }
    Ok(())
}

/// 마크다운을 Slack mrkdwn에 가깝게 바꾼다(제목 → 굵게, `**굵게**` → `*굵게*`).
fn to_slack_mrkdwn(markdown: &str) -> String {
    markdown
        .lines()
        .map(|line| {
            let trimmed = line.trim_start();
            let line = if trimmed.starts_with('#') {
                let heading = trimmed.trim_start_matches('#').trim();
                format!("**{heading}**")
            } else {
                line.to_string()
            };
            line.replace("**", "*")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// 긴 메시지를 줄 단위로 나눈다. 한 줄이 한도보다 길면 글자 단위로 자른다.
fn split_message(text: &str, limit: usize) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut current = String::new();
    for line in text.lines() {
        let line_len = line.chars().count();
        if line_len > limit {
            if !current.is_empty() {
                chunks.push(std::mem::take(&mut current));
            }
            let chars: Vec<char> = line.chars().collect();
            for part in chars.chunks(limit) {
                chunks.push(part.iter().collect());
            }
            continue;
        }
        if current.chars().count() + line_len + 1 > limit && !current.is_empty() {
            chunks.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push('\n');
        }
        current.push_str(line);
    }
    if !current.trim().is_empty() {
        chunks.push(current);
    }
    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_respects_limit() {
        let text = (0..100).map(|i| format!("줄 {i}")).collect::<Vec<_>>().join("\n");
        let chunks = split_message(&text, 50);
        assert!(chunks.iter().all(|c| c.chars().count() <= 50));
        assert_eq!(chunks.join("\n"), text);
    }

    #[test]
    fn split_long_line() {
        let text = "가".repeat(120);
        let chunks = split_message(&text, 50);
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks.concat(), text);
    }

    #[test]
    fn slack_headings_become_bold() {
        assert_eq!(to_slack_mrkdwn("## 결정 사항\n- **배포** 내일"), "*결정 사항*\n- *배포* 내일");
    }
}
