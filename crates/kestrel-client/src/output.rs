use std::io::{IsTerminal as _, Write};

use anyhow::{Context as _, Result, anyhow, bail};
use serde_json::{Map, Value};

use crate::view::View;

static ABSENT: Value = Value::Null;
const GAP: &str = "  ";

pub enum Presentation {
    Human(usize),
    Delimited,
    Json(Vec<String>),
}

impl Presentation {
    pub fn chosen(json: Option<&str>) -> Result<Self> {
        let Some(json) = json else {
            return Ok(if std::io::stdout().is_terminal() {
                Presentation::Human(columns())
            } else {
                Presentation::Delimited
            });
        };

        let fields: Vec<String> = json
            .split(',')
            .map(str::trim)
            .filter(|field| !field.is_empty())
            .map(str::to_owned)
            .collect();
        if fields.is_empty() {
            bail!("--json names the fields to emit, comma-separated: --json id,name");
        }

        Ok(Presentation::Json(fields))
    }
}

pub fn show(presentation: &Presentation, view: &View, answer: &Value) -> Result<()> {
    let records: Vec<&Value> = match answer {
        Value::Null => Vec::new(),
        Value::Array(records) => records.iter().collect(),
        record => vec![record],
    };
    let mut out = std::io::stdout().lock();

    match presentation {
        Presentation::Human(width) => human(&mut out, view, &records, *width)?,
        presentation => {
            for record in records {
                writeln!(out, "{}", line(presentation, view, record)?)?;
            }
        }
    }

    out.flush().context("writing to standard output")
}

/// One record as it arrives, which is every alignment a stream can offer. What a person is
/// shown is neither clipped nor folded onto one line, because a streamed entry is prose.
pub fn line(presentation: &Presentation, view: &View, record: &Value) -> Result<String> {
    Ok(match presentation {
        Presentation::Json(fields) => projected(record, fields)?.to_string(),
        Presentation::Delimited => delimited(record, view),
        Presentation::Human(_) => view
            .fields()
            .iter()
            .map(|field| rendered(at(record, field)))
            .collect::<Vec<_>>()
            .join(GAP),
    })
}

fn delimited(record: &Value, view: &View) -> String {
    cells(record, view.fields()).join("\t")
}

fn human(out: &mut impl Write, view: &View, records: &[&Value], width: usize) -> Result<()> {
    match view {
        View::Value(field) => {
            for record in records {
                writeln!(out, "{}", truncated(&rendered(at(record, field)), width))?;
            }
        }
        View::Rows(fields) => {
            let mut table = vec![fields.iter().map(|field| label(field)).collect()];
            table.extend(records.iter().map(|record| cells(record, fields)));
            let widths = widest(&table);
            for row in &table {
                writeln!(out, "{}", truncated(&padded(row, &widths), width))?;
            }
        }
        View::Detail(fields) => {
            let labels: Vec<String> = fields.iter().map(|field| label(field)).collect();
            let column = labels
                .iter()
                .map(|label| label.chars().count())
                .max()
                .unwrap_or_default();
            for (place, record) in records.iter().enumerate() {
                if place > 0 {
                    writeln!(out)?;
                }
                for (label, field) in labels.iter().zip(*fields) {
                    let value = rendered(at(record, field));
                    let mut lines = value.lines();
                    let first = lines.next().unwrap_or_default();
                    writeln!(
                        out,
                        "{}",
                        truncated(&format!("{label:column$}{GAP}{first}"), width)
                    )?;
                    for line in lines {
                        writeln!(
                            out,
                            "{}",
                            truncated(&format!("{:column$}{GAP}{line}", ""), width)
                        )?;
                    }
                }
            }
        }
    }

    Ok(())
}

fn projected(record: &Value, fields: &[String]) -> Result<Value> {
    let mut projection = Map::new();

    for field in fields {
        let value = at(record, field)
            .ok_or_else(|| {
                anyhow!(
                    "the control plane answered no {field}; it answered {}",
                    held(record)
                )
            })?
            .clone();
        let mut here = &mut projection;
        let mut segments = field.split('.').peekable();
        while let Some(segment) = segments.next() {
            if segments.peek().is_none() {
                here.insert(segment.to_owned(), value);
                break;
            }
            here = here
                .entry(segment)
                .or_insert_with(|| Value::Object(Map::new()))
                .as_object_mut()
                .ok_or_else(|| anyhow!("--json names {field} and the field it reaches through"))?;
        }
    }

    Ok(Value::Object(projection))
}

fn held(record: &Value) -> String {
    match record.as_object() {
        Some(record) if !record.is_empty() => record
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join(", "),
        _ => "no fields at all".to_owned(),
    }
}

/// A path that reaches through a null holds no value, which is not the same as naming a field
/// the answer does not have.
fn at<'a>(record: &'a Value, path: &str) -> Option<&'a Value> {
    let mut value = record;
    for key in path.split('.') {
        if value.is_null() {
            return Some(&ABSENT);
        }
        value = value.get(key)?;
    }

    Some(value)
}

fn cells(record: &Value, fields: &[&str]) -> Vec<String> {
    fields
        .iter()
        .map(|field| one_line(&rendered(at(record, field))))
        .collect()
}

fn rendered(value: Option<&Value>) -> String {
    match value.unwrap_or(&ABSENT) {
        Value::Null => "-".to_owned(),
        Value::Bool(yes) => yes.to_string(),
        Value::Number(number) => number.to_string(),
        Value::String(text) => text.clone(),
        Value::Array(items) if items.is_empty() => "-".to_owned(),
        Value::Array(items) => items
            .iter()
            .map(|item| rendered(Some(item)))
            .collect::<Vec<_>>()
            .join(","),
        object => object.to_string(),
    }
}

/// A record is a line, so a brief that spans several of them is escaped rather than splitting it.
fn one_line(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

fn label(path: &str) -> String {
    path.rsplit('.').next().unwrap_or(path).replace('_', " ")
}

fn widest(table: &[Vec<String>]) -> Vec<usize> {
    (0..table.first().map_or(0, Vec::len))
        .map(|column| {
            table
                .iter()
                .map(|row| row[column].chars().count())
                .max()
                .unwrap_or_default()
        })
        .collect()
}

fn padded(row: &[String], widths: &[usize]) -> String {
    row.iter()
        .zip(widths)
        .enumerate()
        .map(|(place, (cell, width))| {
            if place + 1 == row.len() {
                cell.clone()
            } else {
                format!("{cell:width$}")
            }
        })
        .collect::<Vec<_>>()
        .join(GAP)
}

fn truncated(line: &str, width: usize) -> String {
    if line.chars().count() <= width {
        return line.to_owned();
    }
    if width == 0 {
        return String::new();
    }

    let mut cut: String = line.chars().take(width - 1).collect();
    cut.push('…');
    cut
}

fn columns() -> usize {
    terminal_size::terminal_size().map_or(usize::MAX, |(width, _)| usize::from(width.0))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const AGENTS: View = View::Rows(&["name", "runtime", "model"]);

    fn shown(presentation: &Presentation, view: &View, answer: &Value) -> Vec<String> {
        answer
            .as_array()
            .expect("an array of records")
            .iter()
            .map(|record| line(presentation, view, record).expect("a line"))
            .collect()
    }

    #[test]
    fn a_pipe_gets_every_field_of_every_record_joined_by_tabs() {
        let agents = json!([{ "name": "builder", "runtime": "opencode", "model": null }]);

        assert_eq!(
            shown(&Presentation::Delimited, &AGENTS, &agents),
            ["builder\topencode\t-"]
        );
    }

    #[test]
    fn a_record_spanning_lines_is_still_one_line_of_a_pipe() {
        let triggers = json!([{ "name": "nightly", "runtime": "a\nb", "model": null }]);

        assert_eq!(
            shown(&Presentation::Delimited, &AGENTS, &triggers),
            ["nightly\ta\\nb\t-"]
        );
    }

    /// In the order they were named, so a script reading the line rather than parsing it is
    /// reading the order it asked for.
    #[test]
    fn json_emits_the_named_fields_and_nothing_else() {
        let record = json!({ "id": "01", "name": "builder", "runtime": "opencode" });
        let presentation = Presentation::chosen(Some("name,id")).expect("a field list");

        assert_eq!(
            line(&presentation, &AGENTS, &record).expect("a line"),
            r#"{"name":"builder","id":"01"}"#
        );
    }

    #[test]
    fn json_names_a_field_inside_another_by_its_path() {
        let record = json!({ "record": "r1", "event": { "source": "github", "id": "e1" } });
        let presentation = Presentation::chosen(Some("record,event.source")).expect("a field list");

        assert_eq!(
            line(&presentation, &AGENTS, &record).expect("a line"),
            r#"{"record":"r1","event":{"source":"github"}}"#
        );
    }

    #[test]
    fn a_field_the_answer_does_not_hold_is_refused_with_what_it_does() {
        let record = json!({ "id": "01", "name": "builder" });
        let presentation = Presentation::chosen(Some("nmae")).expect("a field list");

        let refused = line(&presentation, &AGENTS, &record).expect_err("no such field");

        assert!(
            refused.to_string().contains("no nmae") && refused.to_string().contains("id, name"),
            "{refused}"
        );
    }

    #[test]
    fn an_empty_field_list_is_no_field_list() {
        assert!(Presentation::chosen(Some(" , ")).is_err());
    }

    #[test]
    fn a_terminal_gets_columns_that_line_up_under_their_labels() {
        let agents = json!([
            { "name": "builder", "runtime": "opencode", "model": "claude-opus-5" },
            { "name": "reviewer", "runtime": "acp", "model": null },
        ]);
        let mut written = Vec::new();

        human(
            &mut written,
            &AGENTS,
            &agents
                .as_array()
                .expect("records")
                .iter()
                .collect::<Vec<_>>(),
            usize::MAX,
        )
        .expect("a table");

        assert_eq!(
            String::from_utf8(written).expect("utf-8"),
            "name      runtime   model\n\
             builder   opencode  claude-opus-5\n\
             reviewer  acp       -\n"
        );
    }

    #[test]
    fn a_terminal_gets_a_line_no_wider_than_it_is() {
        let agents =
            json!([{ "name": "builder", "runtime": "opencode", "model": "claude-opus-5" }]);
        let mut written = Vec::new();

        human(
            &mut written,
            &AGENTS,
            &agents
                .as_array()
                .expect("records")
                .iter()
                .collect::<Vec<_>>(),
            12,
        )
        .expect("a table");

        for line in String::from_utf8(written).expect("utf-8").lines() {
            assert!(line.chars().count() <= 12, "{line:?} is wider than 12");
        }
    }
}
