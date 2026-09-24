pub fn command(message: &str, organization: Option<&str>) -> Option<String> {
    if let Some(name) = message.strip_prefix("no organization named ") {
        return Some(format!("kestrel organization declare {}", quoted(name)));
    }
    let command = if let Some(rest) = message.strip_prefix("no workspace named ") {
        let (name, _) = rest.split_once(" in the organization ")?;
        Some(format!(
            "kestrel workspace declare {} --repository \"$(git remote get-url origin)\" --branch \"$(git branch --show-current)\"",
            quoted(name)
        ))
    } else if let Some(rest) = message.strip_prefix("no agent named ") {
        let (name, _) = rest.split_once(" in the organization ")?;
        Some(format!("kestrel agent declare {}", quoted(name)))
    } else if let Some(rest) = message.strip_prefix("no subscription profile named ") {
        let (name, _) = rest.split_once(" in the organization ")?;
        Some(format!(
            "kestrel profile declare {} --owner \"$(id -un)\"",
            quoted(name)
        ))
    } else if let Some((_, variable)) = message
        .strip_prefix("the organization ")
        .and_then(|rest| rest.split_once(" holds no provider credential named "))
    {
        Some(format!("kestrel credential set {}", quoted(variable)))
    } else if message.contains("already has the run ") || message.contains(" is still in flight ") {
        let run = message
            .split("the run ")
            .nth(1)?
            .split_whitespace()
            .next()?;
        Some(format!("kestrel run stop {}", quoted(run)))
    } else if let Some(rest) = message.strip_prefix("the session ") {
        if let Some((session, _)) = rest.split_once(" is open, and work continues") {
            Some(format!("kestrel run enqueue --session {}", quoted(session)))
        } else if let Some((session, _)) = rest.split_once("'s instance ") {
            Some(format!("kestrel run enqueue --session {}", quoted(session)))
        } else {
            None
        }
    } else {
        None
    }?;
    Some(match organization {
        Some(organization) => format!("{command} --organization {}", quoted(organization)),
        None => command,
    })
}

fn quoted(value: &str) -> String {
    if value
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || "_-./".contains(character))
    {
        value.to_owned()
    } else {
        format!("'{}'", value.replace('\'', "'\"'\"'"))
    }
}
