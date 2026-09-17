use reqwest::Response;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Event {
    pub id: Option<String>,
    pub name: Option<String>,
    pub data: String,
}

pub struct Events {
    response: Response,
    buffered: Vec<u8>,
}

impl Events {
    pub fn over(response: Response) -> Self {
        Self {
            response,
            buffered: Vec::new(),
        }
    }

    /// `None` is the server closing the stream, which says nothing about whether it meant to.
    pub async fn next(&mut self) -> reqwest::Result<Option<Event>> {
        loop {
            if let Some(event) = self.take() {
                return Ok(Some(event));
            }

            match self.response.chunk().await? {
                Some(chunk) => self.buffered.extend_from_slice(&chunk),
                None => return Ok(None),
            }
        }
    }

    fn take(&mut self) -> Option<Event> {
        loop {
            let end = self.buffered.windows(2).position(|pair| pair == b"\n\n")?;
            let frame: Vec<u8> = self.buffered.drain(..end + 2).collect();
            if let Some(event) = parsed(&String::from_utf8_lossy(&frame)) {
                return Some(event);
            }
        }
    }
}

/// A frame of nothing but comments is the keep-alive, and is no event.
fn parsed(frame: &str) -> Option<Event> {
    let mut event = Event::default();
    let mut data = Vec::new();

    for line in frame.lines() {
        let (field, value) = line.split_once(':').unwrap_or((line, ""));
        let value = value.strip_prefix(' ').unwrap_or(value);
        match field {
            "id" => event.id = Some(value.to_owned()),
            "event" => event.name = Some(value.to_owned()),
            "data" => data.push(value),
            _ => {}
        }
    }

    if data.is_empty() && event.name.is_none() {
        return None;
    }
    event.data = data.join("\n");
    Some(event)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frame_carries_its_id_its_name_and_its_data() {
        assert_eq!(
            parsed("id: s:1\nevent: entry\ndata: {\"seq\":1}\n\n"),
            Some(Event {
                id: Some("s:1".to_owned()),
                name: Some("entry".to_owned()),
                data: "{\"seq\":1}".to_owned(),
            })
        );
    }

    #[test]
    fn data_over_several_lines_is_joined_by_newlines() {
        assert_eq!(
            parsed("data:one\ndata: two\n\n").map(|event| event.data),
            Some("one\ntwo".to_owned())
        );
    }

    #[test]
    fn a_keep_alive_is_no_event() {
        assert_eq!(parsed(":\n\n"), None);
    }
}
