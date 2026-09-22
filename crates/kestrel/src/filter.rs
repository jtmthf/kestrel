use std::fmt;
use std::str::FromStr;

use anyhow::{Context as _, Result, anyhow, bail};
use serde_json::{Map, Value};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Filter {
    Exact(Attribute, String),
    Prefix(Attribute, String),
    Suffix(Attribute, String),
    All(Vec<Filter>),
    Any(Vec<Filter>),
    Not(Box<Filter>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Attribute {
    Id,
    Source,
    Specversion,
    Type,
    Subject,
    Time,
    Data(Vec<String>),
}

impl FromStr for Filter {
    type Err = anyhow::Error;

    fn from_str(text: &str) -> Result<Self> {
        let value = serde_json::from_str(text)
            .map_err(|error| anyhow!("a filter is a JSON object, and that is not JSON: {error}"))?;
        Filter::from_json(&value)
    }
}

impl Filter {
    pub fn from_json(value: &Value) -> Result<Self> {
        let (dialect, operand) = sole_entry(value, "a filter")?;

        Ok(match dialect.as_str() {
            "exact" => {
                let (attribute, value) = comparison(dialect, operand)?;
                Filter::Exact(attribute, value)
            }
            "prefix" => {
                let (attribute, value) = comparison(dialect, operand)?;
                Filter::Prefix(attribute, value)
            }
            "suffix" => {
                let (attribute, value) = comparison(dialect, operand)?;
                Filter::Suffix(attribute, value)
            }
            "all" => Filter::All(filters(dialect, operand)?),
            "any" => Filter::Any(filters(dialect, operand)?),
            "not" => Filter::Not(Box::new(Filter::from_json(operand)?)),
            other => bail!(
                "{other} is not a filter kestrel knows: use exact, prefix, suffix, all, any or not"
            ),
        })
    }
}

fn sole_entry<'a>(value: &'a Value, what: &str) -> Result<(&'a String, &'a Value)> {
    let object: &Map<String, Value> = value
        .as_object()
        .with_context(|| format!("{what} is a JSON object"))?;
    let mut entries = object.iter();

    match (entries.next(), entries.next()) {
        (Some(entry), None) => Ok(entry),
        _ => bail!(
            "{what} has exactly one key, and {value} has {}",
            object.len()
        ),
    }
}

fn comparison(dialect: &str, operand: &Value) -> Result<(Attribute, String)> {
    let (attribute, value) = sole_entry(operand, dialect)?;
    let value = value
        .as_str()
        .with_context(|| format!("{dialect} compares against a string, not {value}"))?;

    if value.is_empty() && dialect != "exact" {
        bail!("a {dialect} of nothing matches everything it is applied to: name one");
    }

    Ok((attribute.parse()?, value.to_owned()))
}

fn filters(dialect: &str, operand: &Value) -> Result<Vec<Filter>> {
    let filters = operand
        .as_array()
        .with_context(|| format!("{dialect} takes an array of filters"))?
        .iter()
        .map(Filter::from_json)
        .collect::<Result<Vec<_>>>()?;

    if filters.is_empty() {
        bail!("{dialect} takes at least one filter");
    }
    Ok(filters)
}

impl Filter {
    pub fn to_json(&self) -> Value {
        let (dialect, operand) = match self {
            Filter::Exact(attribute, value) => ("exact", comparing(attribute, value)),
            Filter::Prefix(attribute, value) => ("prefix", comparing(attribute, value)),
            Filter::Suffix(attribute, value) => ("suffix", comparing(attribute, value)),
            Filter::All(filters) => ("all", filters.iter().map(Filter::to_json).collect()),
            Filter::Any(filters) => ("any", filters.iter().map(Filter::to_json).collect()),
            Filter::Not(filter) => ("not", filter.to_json()),
        };

        Value::Object(Map::from_iter([(dialect.to_owned(), operand)]))
    }
}

/// GitHub's word for someone with standing in the repository; every other association,
/// and an Event that carries none, is a stranger's.
const MEMBERS: &[&str] = &["OWNER", "MEMBER", "COLLABORATOR"];

/// An assignee is who was acted on, so naming one is no guard.
const ACTORS: &[&[&str]] = &[
    &["actor", "login"],
    &["user", "login"],
    &["sender", "login"],
    &["comment", "user", "login"],
];

/// Who wrote an Event, as far as a filter can say: the login an actor comparison names, and the
/// standing in the repository GitHub reports.
#[derive(Debug, Clone, Copy, Default)]
pub struct Author<'a> {
    pub login: Option<&'a str>,
    pub association: Option<&'a str>,
}

/// How one comparison holds of a value, so what a filter says about an author is read the way
/// the matcher would read it.
#[derive(Debug, Clone, Copy)]
enum Comparison {
    Exact,
    Prefix,
    Suffix,
}

impl Comparison {
    fn holds(self, candidate: &str, value: &str) -> bool {
        match self {
            Comparison::Exact => candidate == value,
            Comparison::Prefix => candidate.starts_with(value),
            Comparison::Suffix => candidate.ends_with(value),
        }
    }
}

impl Author<'_> {
    /// Whether one comparison is about who wrote the Event, and holds of this author. A
    /// comparison about the Event's content says nothing about the author and does not hold
    /// against them.
    fn satisfies(self, attribute: &Attribute, value: &str, comparison: Comparison) -> bool {
        let Attribute::Data(path) = attribute else {
            return true;
        };

        if ACTORS
            .iter()
            .any(|actor| path.iter().map(String::as_str).eq(actor.iter().copied()))
        {
            return self
                .login
                .is_some_and(|login| comparison.holds(login, value));
        }
        if path.last().is_some_and(|key| key == "author_association") && MEMBERS.contains(&value) {
            return self
                .association
                .is_some_and(|association| MEMBERS.contains(&association));
        }

        true
    }
}

impl Filter {
    /// Whether this filter authorizes `author` to speak in a Session it opened. A filter that
    /// admits outsiders lets anyone; one that does not lets only the logins and member
    /// associations it names. Comparisons about the Event's content are ignored, so a remark
    /// from an authorized author is admitted even though the command prefix the filter was
    /// written to match does not fit it.
    pub fn admits(&self, author: Author<'_>) -> bool {
        self.admits_outsiders() || self.allows(author)
    }

    fn allows(&self, author: Author<'_>) -> bool {
        match self {
            Filter::Exact(attribute, value) => {
                author.satisfies(attribute, value, Comparison::Exact)
            }
            Filter::Prefix(attribute, value) => {
                author.satisfies(attribute, value, Comparison::Prefix)
            }
            Filter::Suffix(attribute, value) => {
                author.satisfies(attribute, value, Comparison::Suffix)
            }
            Filter::All(filters) => filters.iter().all(|filter| filter.allows(author)),
            Filter::Any(filters) => filters.iter().any(|filter| filter.allows(author)),
            // A negation of something that says nothing about the author says nothing about them.
            Filter::Not(filter) if filter.admits_outsiders() => true,
            Filter::Not(filter) => !filter.allows(author),
        }
    }

    /// Judged from the filter's shape alone, so it errs toward warning: only GitHub tells kestrel
    /// who an actor is, and only a comparison every match must pass can be trusted to decline one.
    pub fn admits_outsiders(&self) -> bool {
        !(self.requires(&Filter::names_a_member)
            || self.requires(&Filter::names_an_actor)
            || self.requires(&Filter::excludes_github))
    }

    fn requires(&self, holds: &impl Fn(&Filter) -> bool) -> bool {
        match self {
            Filter::All(filters) => filters.iter().any(|filter| filter.requires(holds)),
            Filter::Any(filters) => filters.iter().all(|filter| filter.requires(holds)),
            filter => holds(filter),
        }
    }

    fn names_a_member(&self) -> bool {
        matches!(
            self,
            Filter::Exact(Attribute::Data(path), value)
                if path.last().is_some_and(|key| key == "author_association")
                    && MEMBERS.contains(&value.as_str())
        )
    }

    fn names_an_actor(&self) -> bool {
        matches!(
            self,
            Filter::Exact(Attribute::Data(path), _)
                if ACTORS.iter().any(|actor| path.iter().map(String::as_str).eq(actor.iter().copied()))
        )
    }

    fn excludes_github(&self) -> bool {
        let (attribute, value, exact) = match self {
            Filter::Exact(attribute, value) => (attribute, value, true),
            Filter::Prefix(attribute, value) => (attribute, value, false),
            _ => return false,
        };
        let github = match attribute {
            Attribute::Source => "https://github.com/",
            Attribute::Type => "com.github.",
            _ => return false,
        };

        !(value.starts_with(github) || (!exact && github.starts_with(value.as_str())))
    }
}

fn comparing(attribute: &Attribute, value: &str) -> Value {
    Value::Object(Map::from_iter([(
        attribute.to_string(),
        Value::String(value.to_owned()),
    )]))
}

impl FromStr for Attribute {
    type Err = anyhow::Error;

    fn from_str(name: &str) -> Result<Self> {
        Ok(match name {
            "id" => Attribute::Id,
            "source" => Attribute::Source,
            "specversion" => Attribute::Specversion,
            "type" => Attribute::Type,
            "subject" => Attribute::Subject,
            "time" => Attribute::Time,
            _ => {
                let path = name
                    .strip_prefix("data.")
                    .map(|path| path.split('.').map(str::to_owned).collect::<Vec<_>>())
                    // A SQLite JSON path has no way to escape a quote inside a key.
                    .filter(|path| path.iter().all(|key| !key.is_empty() && !key.contains('"')));

                match path {
                    Some(path) => Attribute::Data(path),
                    None => bail!(
                        "{name} is not an attribute: use id, source, specversion, type, subject, \
                         time, or a path into data such as data.label.name"
                    ),
                }
            }
        })
    }
}

impl fmt::Display for Attribute {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Attribute::Id => f.write_str("id"),
            Attribute::Source => f.write_str("source"),
            Attribute::Specversion => f.write_str("specversion"),
            Attribute::Type => f.write_str("type"),
            Attribute::Subject => f.write_str("subject"),
            Attribute::Time => f.write_str("time"),
            Attribute::Data(path) => write!(f, "data.{}", path.join(".")),
        }
    }
}

impl fmt::Display for Filter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Filter::Exact(attribute, value) => write!(f, "{attribute} = {value:?}"),
            Filter::Prefix(attribute, value) => write!(f, "{attribute} starts with {value:?}"),
            Filter::Suffix(attribute, value) => write!(f, "{attribute} ends with {value:?}"),
            Filter::All(filters) => joined(f, filters, " and "),
            Filter::Any(filters) => joined(f, filters, " or "),
            Filter::Not(filter) => {
                f.write_str("not ")?;
                grouped(f, filter)
            }
        }
    }
}

fn joined(f: &mut fmt::Formatter<'_>, filters: &[Filter], by: &str) -> fmt::Result {
    for (at, filter) in filters.iter().enumerate() {
        if at > 0 {
            f.write_str(by)?;
        }
        grouped(f, filter)?;
    }
    Ok(())
}

fn grouped(f: &mut fmt::Formatter<'_>, filter: &Filter) -> fmt::Result {
    match filter {
        Filter::All(filters) | Filter::Any(filters) if filters.len() > 1 => {
            write!(f, "({filter})")
        }
        _ => write!(f, "{filter}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_filter_prints_back_the_way_it_reads() {
        let filter: Filter = r##"{"all": [
            {"exact": {"source": "https://github.com/jtmthf/kestrel"}},
            {"any": [
                {"exact": {"data.label.name": "ready-for-agent"}},
                {"suffix": {"type": ".opened"}}
            ]},
            {"not": {"prefix": {"subject": "#1"}}}
        ]}"##
            .parse()
            .expect("the filter should parse");

        assert_eq!(
            filter.to_string(),
            r##"source = "https://github.com/jtmthf/kestrel" and (data.label.name = "ready-for-agent" or type ends with ".opened") and not subject starts with "#1""##
        );
    }

    #[test]
    fn a_filter_admits_outsiders_unless_every_match_is_a_members_or_not_githubs() {
        for (filter, admits) in [
            (r#"{"exact": {"type": "com.github.issues.labeled"}}"#, true),
            (
                r#"{"all": [
                    {"exact": {"type": "com.github.issues.labeled"}},
                    {"exact": {"data.issue.author_association": "MEMBER"}}
                ]}"#,
                false,
            ),
            (
                r#"{"all": [
                    {"exact": {"type": "com.github.issues.labeled"}},
                    {"any": [
                        {"exact": {"data.issue.author_association": "OWNER"}},
                        {"exact": {"data.issue.author_association": "COLLABORATOR"}}
                    ]}
                ]}"#,
                false,
            ),
            (
                r#"{"any": [
                    {"exact": {"data.issue.author_association": "OWNER"}},
                    {"exact": {"type": "com.github.issues.labeled"}}
                ]}"#,
                true,
            ),
            (
                r#"{"exact": {"data.issue.author_association": "CONTRIBUTOR"}}"#,
                true,
            ),
            (
                r#"{"not": {"exact": {"data.issue.author_association": "NONE"}}}"#,
                true,
            ),
            (
                r#"{"prefix": {"data.issue.author_association": "MEMBER"}}"#,
                true,
            ),
            (
                r#"{"exact": {"source": "https://ci.example.com/pipelines/3"}}"#,
                false,
            ),
            (r#"{"prefix": {"type": "com.example."}}"#, false),
            (r#"{"prefix": {"type": "com."}}"#, true),
            (
                r#"{"prefix": {"source": "https://github.com/jtmthf/"}}"#,
                true,
            ),
            (r#"{"suffix": {"type": ".failed"}}"#, true),
            (
                r#"{"all": [
                    {"exact": {"type": "com.github.issues.assigned"}},
                    {"any": [
                        {"exact": {"data.actor.login": "jtmthf"}},
                        {"exact": {"data.sender.login": "jtmthf"}}
                    ]}
                ]}"#,
                false,
            ),
            (r#"{"exact": {"data.comment.user.login": "jtmthf"}}"#, false),
            (r#"{"exact": {"data.assignee.login": "kestrel-bot"}}"#, true),
            (r#"{"prefix": {"data.user.login": "jt"}}"#, true),
        ] {
            let parsed: Filter = filter.parse().expect("the filter should parse");
            assert_eq!(parsed.admits_outsiders(), admits, "{filter}");
        }
    }

    fn author<'a>(login: &'a str, association: Option<&'a str>) -> Author<'a> {
        Author {
            login: Some(login),
            association,
        }
    }

    #[test]
    fn a_filter_authorizes_the_actors_it_names_and_admits_outsiders_it_does_not() {
        let dogfood: Filter = serde_json::json!({"all": [
            {"exact": {"source": "https://github.com/jtmthf/kestrel"}},
            {"exact": {"type": "com.github.issue_comment.created"}},
            {"any": [
                {"all": [
                    {"exact": {"data.user.login": "jtmthf"}},
                    {"prefix": {"data.body": "@kestrel"}}
                ]},
                {"all": [
                    {"exact": {"data.comment.user.login": "jtmthf"}},
                    {"prefix": {"data.comment.body": "@kestrel"}}
                ]}
            ]}
        ]})
        .to_string()
        .parse()
        .expect("the filter should parse");

        // The command prefix does not fit a remark, and says nothing about who wrote it.
        assert!(dogfood.admits(author("jtmthf", None)));
        assert!(!dogfood.admits(author("a-stranger", None)));
        assert!(!dogfood.admits(Author::default()));

        // A filter that admits outsiders authorizes whoever the comment came from.
        let labelled: Filter = serde_json::json!({"exact": {"type": "com.github.issues.labeled"}})
            .to_string()
            .parse()
            .expect("the filter should parse");
        assert!(labelled.admits(author("a-stranger", None)));
    }

    #[test]
    fn a_filter_authorizes_a_member_association_where_it_names_one() {
        let members: Filter =
            serde_json::json!({"exact": {"data.issue.author_association": "MEMBER"}})
                .to_string()
                .parse()
                .expect("the filter should parse");

        assert!(members.admits(author("anyone", Some("OWNER"))));
        assert!(members.admits(author("anyone", Some("COLLABORATOR"))));
        assert!(!members.admits(author("anyone", Some("CONTRIBUTOR"))));
        assert!(!members.admits(author("anyone", None)));
    }

    #[test]
    fn a_negation_about_the_events_content_does_not_decline_an_author() {
        let filter: Filter = serde_json::json!({"all": [
            {"exact": {"data.user.login": "jtmthf"}},
            {"not": {"exact": {"data.body": "not this"}}}
        ]})
        .to_string()
        .parse()
        .expect("the filter should parse");

        assert!(filter.admits(author("jtmthf", None)));
        assert!(!filter.admits(author("a-stranger", None)));
    }

    #[test]
    fn a_filter_kestrel_cannot_evaluate_is_refused_saying_why() {
        for (filter, because) in [
            (
                r#"{"exact": {"integration": "github"}}"#,
                "integration is not an attribute",
            ),
            (r#"{"sql": "type = 'x'"}"#, "sql is not a filter"),
            (r#"{"all": []}"#, "all takes at least one filter"),
            (r#"{"any": []}"#, "any takes at least one filter"),
            (
                r#"{"exact": {"type": "x", "source": "y"}}"#,
                "exactly one key",
            ),
            (r#"{"prefix": {"type": ""}}"#, "prefix of nothing"),
            (r#"{"suffix": {"type": ""}}"#, "suffix of nothing"),
            (r#"{"exact": {"data.": "x"}}"#, "data. is not an attribute"),
            (
                r#"{"exact": {"data.label..name": "x"}}"#,
                "data.label..name is not an attribute",
            ),
            (r#"{"exact": {"type": 1}}"#, "compares against a string"),
            (
                r#"{"exact": {"data.a\"b": "x"}}"#,
                "data.a\"b is not an attribute",
            ),
        ] {
            let refusal = filter
                .parse::<Filter>()
                .expect_err(&format!("{filter} should be refused"));
            assert!(
                format!("{refusal:#}").contains(because),
                "{filter} was refused unhelpfully: {refusal:#}"
            );
        }
    }
}
