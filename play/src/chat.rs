//! Chat on the live session: All, a team, or one participant.
//!
//! The text is carried unchanged. The roster decides who receives it.

use tokio::sync::mpsc;

/// One person in the room. `team` is the group a Team message uses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Participant {
    pub id: String,
    pub team: String,
}

/// Who should receive one message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Audience {
    All,
    Team(String),
    One(String),
}

/// What the sender wrote, and who it was for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatMessage {
    pub from: String,
    pub audience: Audience,
    pub text: String,
}

/// A message after the roster filter. `text` is the sender's text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Routed {
    pub recipients: Vec<String>,
    pub message: ChatMessage,
}

/// Recipient ids in roster order.
pub fn recipients(roster: &[Participant], message: &ChatMessage) -> Vec<String> {
    match &message.audience {
        Audience::All => roster.iter().map(|person| person.id.clone()).collect(),
        Audience::Team(team) => roster
            .iter()
            .filter(|person| &person.team == team)
            .map(|person| person.id.clone())
            .collect(),
        Audience::One(id) => roster
            .iter()
            .filter(|person| &person.id == id)
            .map(|person| person.id.clone())
            .collect(),
    }
}

pub fn route(roster: &[Participant], message: ChatMessage) -> Routed {
    let recipients = recipients(roster, &message);
    Routed {
        recipients,
        message,
    }
}

/// `kind`, audience, sender, then the text bytes.
pub fn encode_chat(message: &ChatMessage) -> Result<Vec<u8>, String> {
    let (kind, audience) = match &message.audience {
        Audience::All => (1u8, String::new()),
        Audience::Team(name) => (2u8, name.clone()),
        Audience::One(id) => (3u8, id.clone()),
    };
    if audience.len() > u16::MAX as usize || message.from.len() > u16::MAX as usize {
        return Err("chat name is too long".into());
    }
    let mut out =
        Vec::with_capacity(1 + 4 + audience.len() + message.from.len() + message.text.len());
    out.push(kind);
    out.extend_from_slice(&(audience.len() as u16).to_le_bytes());
    out.extend_from_slice(audience.as_bytes());
    out.extend_from_slice(&(message.from.len() as u16).to_le_bytes());
    out.extend_from_slice(message.from.as_bytes());
    out.extend_from_slice(message.text.as_bytes());
    Ok(out)
}

pub fn decode_chat(payload: &[u8]) -> Result<ChatMessage, String> {
    if payload.len() < 5 {
        return Err("chat record is short".into());
    }
    let kind = payload[0];
    let audience_len = u16::from_le_bytes([payload[1], payload[2]]) as usize;
    if payload.len() < 3 + audience_len + 2 {
        return Err("chat audience is truncated".into());
    }
    let audience_bytes = &payload[3..3 + audience_len];
    let audience_text = std::str::from_utf8(audience_bytes)
        .map_err(|_| "chat audience is not utf-8".to_string())?
        .to_string();
    let from_at = 3 + audience_len;
    let from_len = u16::from_le_bytes([payload[from_at], payload[from_at + 1]]) as usize;
    let from_start = from_at + 2;
    if payload.len() < from_start + from_len {
        return Err("chat sender is truncated".into());
    }
    let from = std::str::from_utf8(&payload[from_start..from_start + from_len])
        .map_err(|_| "chat sender is not utf-8".to_string())?
        .to_string();
    let text = std::str::from_utf8(&payload[from_start + from_len..])
        .map_err(|_| "chat text is not utf-8".to_string())?
        .to_string();
    let audience = match kind {
        1 => Audience::All,
        2 => Audience::Team(audience_text),
        3 => Audience::One(audience_text),
        _ => return Err(format!("unknown chat audience {kind}")),
    };
    Ok(ChatMessage {
        from,
        audience,
        text,
    })
}

pub async fn submit_chat(
    tx: &mpsc::Sender<ChatMessage>,
    message: ChatMessage,
) -> Result<(), String> {
    tx.send(message)
        .await
        .map_err(|_| "chat channel closed".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn room() -> Vec<Participant> {
        vec![
            Participant {
                id: "ada".into(),
                team: "red".into(),
            },
            Participant {
                id: "bea".into(),
                team: "red".into(),
            },
            Participant {
                id: "cy".into(),
                team: "blue".into(),
            },
        ]
    }

    #[test]
    fn all_team_and_one_are_three_different_sets() {
        let roster = room();
        let all = ChatMessage {
            from: "ada".into(),
            audience: Audience::All,
            text: "hello all".into(),
        };
        let team = ChatMessage {
            from: "ada".into(),
            audience: Audience::Team("red".into()),
            text: "red only".into(),
        };
        let one = ChatMessage {
            from: "ada".into(),
            audience: Audience::One("cy".into()),
            text: "psst".into(),
        };
        let all_to = recipients(&roster, &all);
        let team_to = recipients(&roster, &team);
        let one_to = recipients(&roster, &one);
        assert_ne!(all_to, team_to);
        assert_ne!(all_to, one_to);
        assert_ne!(team_to, one_to);
        assert_eq!(all_to, vec!["ada", "bea", "cy"]);
        assert_eq!(team_to, vec!["ada", "bea"]);
        assert_eq!(one_to, vec!["cy"]);
        let routed = route(&roster, decode_chat(&encode_chat(&one).unwrap()).unwrap());
        assert_eq!(routed.message.text, "psst");
        assert_eq!(routed.recipients, one_to);
    }
}
