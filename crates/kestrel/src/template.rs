use std::fmt;
use std::io;
use std::str::FromStr;

use anyhow::{Result, anyhow, bail};
use minijinja::{Environment, UndefinedBehavior, context};

use crate::domain::Occurrence;

const FUEL: u64 = 100_000;
const RECURSION_LIMIT: usize = 100;
/// minijinja has no output limit of its own, so a render writes through one kestrel counts.
const MOST_BYTES: usize = 256 * 1024;

/// Operator-authored and evaluated inside the control plane, so it renders strictly and within
/// limits (ADR-0012).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Template(String);

impl FromStr for Template {
    type Err = anyhow::Error;

    fn from_str(source: &str) -> Result<Self> {
        environment()
            .template_from_str(source)
            .map_err(|error| anyhow!("{error:#}"))?;

        Ok(Self(source.to_owned()))
    }
}

impl fmt::Display for Template {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Template {
    pub fn render(&self, event: &Occurrence) -> Result<String> {
        let environment = environment();
        let template = environment
            .template_from_str(&self.0)
            .map_err(|error| anyhow!("{error:#}"))?;

        let mut output = Bounded::default();
        if let Err(error) = template.render_captured_to(context! { event => event }, &mut output) {
            if output.overflowed {
                bail!("it renders more than {MOST_BYTES} bytes");
            }
            bail!("{error:#}");
        }

        let rendered = String::from_utf8(output.bytes)?;
        if rendered.trim().is_empty() {
            bail!("it renders to nothing");
        }
        Ok(rendered)
    }

    pub fn render_line(&self, event: &Occurrence) -> Result<String> {
        let rendered = self.render(event)?;
        let line = rendered.trim();
        if line.lines().nth(1).is_some() {
            bail!("it renders to more than one line: {line:?}");
        }

        Ok(line.to_owned())
    }
}

fn environment<'source>() -> Environment<'source> {
    let mut environment = Environment::new();
    environment.set_undefined_behavior(UndefinedBehavior::Strict);
    environment.set_debug(true);
    environment.set_fuel(Some(FUEL));
    environment.set_recursion_limit(RECURSION_LIMIT);
    environment
}

#[derive(Default)]
struct Bounded {
    bytes: Vec<u8>,
    overflowed: bool,
}

impl io::Write for Bounded {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.bytes.len() + buf.len() > MOST_BYTES {
            self.overflowed = true;
            return Err(io::Error::other("the output limit is reached"));
        }
        self.bytes.extend_from_slice(buf);

        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labelled() -> Occurrence {
        Occurrence {
            id: "7".to_owned(),
            source: "https://github.com/jtmthf/kestrel".to_owned(),
            specversion: "1.0".to_owned(),
            r#type: "com.github.issues.labeled".to_owned(),
            subject: Some("#43".to_owned()),
            time: "2026-09-01T12:00:07Z".parse().expect("a timestamp"),
            data: serde_json::json!({
                "label": { "name": "ready-for-agent" },
                "issue": { "number": 43, "title": "an issue numbered 43" },
            }),
        }
    }

    fn refusal(source: &str) -> String {
        let template: Template = source.parse().expect("the template should parse");
        let refusal = template
            .render(&labelled())
            .expect_err(&format!("{source} should not render"));

        format!("{refusal:#}")
    }

    #[test]
    fn a_template_renders_over_the_event() {
        let template: Template =
            "{{ event.data.issue.title }} ({{ event.subject }}, {{ event.type }})"
                .parse()
                .expect("the template should parse");

        assert_eq!(
            template
                .render(&labelled())
                .expect("the template should render"),
            "an issue numbered 43 (#43, com.github.issues.labeled)"
        );
    }

    #[test]
    fn an_undefined_field_is_an_error_naming_where_it_is_and_what_is_in_scope() {
        let refusal = refusal("Review the pull request on {{ event.data.pull_request.head.ref }}");

        assert!(refusal.contains("undefined value"), "{refusal}");
        assert!(
            refusal.contains("> Review the pull request on {{ event.data.pull_request.head.ref }}"),
            "the refusal does not show the line: {refusal}"
        );
        assert!(refusal.contains('^'), "the refusal has no span: {refusal}");
        assert!(
            refusal.contains("ready-for-agent"),
            "the refusal does not dump the variables in scope: {refusal}"
        );
    }

    #[test]
    fn a_template_that_never_finishes_runs_out_of_fuel() {
        let refusal = refusal(
            "{% for i in range(100000) %}{% for j in range(100000) %}{% endfor %}{% endfor %}",
        );

        assert!(refusal.contains("fuel"), "{refusal}");
    }

    #[test]
    fn a_template_that_recurses_forever_is_stopped() {
        let refusal = refusal("{% macro again() %}{{ again() }}{% endmacro %}{{ again() }}");

        assert!(refusal.contains("recursion"), "{refusal}");
    }

    #[test]
    fn a_template_that_renders_too_much_is_stopped() {
        let refusal = refusal("{{ 'kestrel' * 100000 }}");

        assert!(refusal.contains("renders more than"), "{refusal}");
    }

    #[test]
    fn a_template_that_renders_nothing_is_an_error() {
        assert!(refusal("{{ '' }}  ").contains("renders to nothing"));
    }

    #[test]
    fn a_line_is_trimmed_and_never_two() {
        let line: Template = " kestrel/{{ event.data.issue.number }}\n"
            .parse()
            .expect("the template should parse");
        assert_eq!(
            line.render_line(&labelled())
                .expect("the line should render"),
            "kestrel/43"
        );

        let lines: Template = "kestrel/\n{{ event.data.issue.number }}"
            .parse()
            .expect("the template should parse");
        let refusal = lines
            .render_line(&labelled())
            .expect_err("two lines should not render as one");
        assert!(
            refusal.to_string().contains("more than one line"),
            "{refusal}"
        );
    }

    #[test]
    fn a_template_that_does_not_parse_is_refused_saying_where() {
        let refusal = "{{ event.data"
            .parse::<Template>()
            .expect_err("an unclosed expression should be refused");

        assert!(
            format!("{refusal:#}").contains("syntax error"),
            "{refusal:#}"
        );
    }
}
